use actix_web::{web, App, HttpResponse, HttpServer, Responder};
use clap::{Parser, ValueEnum};
use reqwest::Client;
use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::{net::UdpSocket, time};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Mode of operation: 'server' or 'client'
    #[arg(value_enum)]
    mode: Mode,
    /// Number of ms until at least once the client probes the server for new packets
    #[arg(long, default_value_t = 1000)]
    keep_alive_ms: u64,
    /// The number of the port on that the program listens for udp packets
    #[arg(long, default_value_t = 9898)]
    udp_port: u32,
    /// The number of the port on that the program listens for http traffic (only in server mode)
    #[arg(long, default_value_t = 8888)]
    http_port: u32,
    /// Address of the udp relay target ("host:port", like "127.0.0.1:7777") (only in server mode)
    #[arg(long, default_value_t = String::from("127.0.0.1:7777"))]
    udp_port_relay_target: String,
    /// Address of the server that accepts http(s) traffic (only in client mode)
    #[arg(long, default_value_t = String::from("replace-with-proper-address"))]
    http_server: String,
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Debug)]
enum Mode {
    Server,
    Client,
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let args = Args::parse();

    match args.mode {
        Mode::Server => run_server(&args).await,
        Mode::Client => run_client(&args).await,
    }

    Ok(())
}

/// Start the HTTP server and UDP listener for the server
async fn run_server(args: &Args) -> () {
    let http_server_addr = format!("0.0.0.0:{}", args.http_port); // TCP address for HTTP server, always on all interfaces
    let udp_listener_addr: SocketAddr = format!("127.0.0.1:{}", args.udp_port)
        .parse()
        .expect("Invalid udp listen address");
    let tcp_keep_alive_ping_ms = args.keep_alive_ms;

    println!("Starting HTTP server at http://{}", http_server_addr);
    println!("Starting UDP listener at {}", udp_listener_addr);

    // Define http server
    let http_server = match HttpServer::new(|| App::new().route("/", web::get().to(hello_world)))
        .bind(http_server_addr)
    {
        Ok(bound) => bound,
        Err(e) => {
            eprintln!("Error binding http server: {}", e);
            return ();
        }
    }
    .disable_signals()
    .run();
    let server_handle = http_server.handle();

    // Define udp listener
    let udp_listener_shutdown_marker = Arc::new(AtomicBool::new(false));
    let udp_listener = udp_listener_server(
        Arc::clone(&udp_listener_shutdown_marker),
        udp_listener_addr,
        tcp_keep_alive_ping_ms,
    );

    let http_server_task = tokio::spawn(http_server);
    let udp_listener_task = tokio::spawn(udp_listener);
    let shutdown_task = tokio::spawn(async move {
        // listen for ctrl-c
        tokio::signal::ctrl_c().await.unwrap();

        // start shutdown of tasks
        let http_server_stop = server_handle.stop(true);
        udp_listener_shutdown_marker.store(true, Ordering::SeqCst);

        // await shutdown of server gracefully
        http_server_stop.await;
    });

    // Wait for all tasks to complete
    // https://github.com/actix/actix-web/issues/2739#issuecomment-1107638674
    match tokio::try_join!(http_server_task, udp_listener_task, shutdown_task) {
        Err(_) => println!("Error in at least one listening task"),
        Ok(_) => println!("All listeners closed successfully"),
    };
}

/// Handler for the http server endpoints
async fn hello_world() -> impl Responder {
    HttpResponse::Ok().json(serde_json::json!({ "message": "Hello, Actix!" }))
}

async fn udp_listener_server(
    shutdown_marker: Arc<AtomicBool>,
    listen_addr: SocketAddr,
    tcp_keep_alive_ping_ms: u64,
) -> std::io::Result<()> {
    let socket = match UdpSocket::bind(listen_addr).await {
        Err(e) => {
            eprintln!("Error binding UDP listener: {}", e);
            return Err(e);
        }
        Ok(s) => s,
    };

    // define a buffer that is larger than the max possible udp-packet-size
    let mut buffer = vec![0u8; 66000];

    loop {
        if shutdown_marker.load(Ordering::SeqCst) {
            break;
        }

        match time::timeout(
            Duration::from_millis(tcp_keep_alive_ping_ms),
            socket.recv_from(&mut buffer),
        )
        .await
        {
            Ok(Ok((len, src))) => {
                println!("Received {} bytes from {}", len, src);
            }
            Ok(Err(e)) => {
                eprintln!("Error receiving UDP packet: {}", e);
            }
            Err(_) => {
                // Timeout expired, no data received -> this Err happens often and is expected, to assure the loop is cycled regularly
                println!("No UDP packet received in the last keep alive. Continuing...");
            }
        };
    }

    Ok(())
}

/// Listen on the UDP Port for the client and poll the server for packets to transfer
async fn run_client(args: &Args) -> () {
    let udp_listener_addr: SocketAddr = format!("127.0.0.1:{}", args.udp_port)
        .parse()
        .expect("Invalid udp listen address");
    let server_url = String::from(&args.http_server);
    let tcp_keep_alive_ping_ms = args.keep_alive_ms;

    println!("Starting UDP listener at {}", udp_listener_addr);

    // Bind a UDP socket to the listening address
    let socket = match UdpSocket::bind(udp_listener_addr).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error binding UDP listener: {}", e);
            return ();
        }
    };

    // define a buffer that is larger than the max possible udp-packet-size
    let mut buffer = vec![0u8; 66000];

    let http_client = Client::new();
    loop {
        match time::timeout(
            Duration::from_millis(tcp_keep_alive_ping_ms),
            socket.recv_from(&mut buffer),
        )
        .await
        {
            Ok(Ok((len, src))) => {
                println!("Received {} bytes from {}", len, src);
                http_packet_exchange(&http_client, &server_url, Some((len, &buffer))).await;
            }
            Ok(Err(e)) => {
                eprintln!("Error receiving UDP packet: {}", e);
            }
            Err(_) => {
                // Timeout expired, no data received -> this Err happens often and is expected, to assure the loop is cycled regularly
                println!(
                    "No UDP packet received in the last keep alive. Poll backend for new data..."
                );
                http_packet_exchange(&http_client, &server_url, None).await;
            }
        }
    }
}

async fn http_packet_exchange(
    http_client: &Client,
    server_url: &str,
    udp_packet_content: Option<(usize, &Vec<u8>)>,
) {
    let _ = udp_packet_content; // TODO

    // Perform HTTP request
    let res = match http_client.get(server_url).send().await {
        Err(e) => {
            eprintln!("Error when contacting server: {}", e);
            return ();
        }
        Ok(res) => res,
    };

    let status = res.status();
    if status.is_success() {
        println!("Successful tunnel exchange")
    }
    // let body = res.text().await.expect("Failed to read response body");
}
