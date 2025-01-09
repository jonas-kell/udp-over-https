#[macro_use]
extern crate log;

use crate::args::{Args, Mode};
use crate::base64::{base_64_decode_string_to_bytes, base_64_encode_bytes_to_string};
use actix_web::{middleware, web, App, HttpResponse, HttpServer, Responder};
use async_channel;
use clap::Parser;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::{net::UdpSocket, time};

mod args;
mod base64;

#[derive(Serialize, Deserialize)]
struct HttpData {
    secret: String,
    data: Vec<String>,
}

#[derive(Debug)]
struct AppState {
    args: Args,
    receiver_udp_to_http: async_channel::Receiver<String>,
    sender_http_to_udp: async_channel::Sender<String>,
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    std::env::set_var("RUST_LOG", "debug,actix=off,reqwest=off,hyper=off");

    let args = Args::parse();
    if args.verbose {
        std::env::set_var("RUST_LOG", "trace"); // more logs!!
    }

    env_logger::init();

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
    let udp_relay_target_addr: SocketAddr = format!("{}", args.udp_port_relay_target)
        .parse()
        .expect("Invalid udp relay target address");
    let tcp_keep_alive_ping_ms = args.keep_alive_ms;

    info!("Starting HTTP server at http://{}", http_server_addr);
    info!("Starting UDP listener at {}", udp_listener_addr);

    // define local communication channel for relaying udp packets to the http responder to relay them
    let (sender_udp_to_http, receiver_udp_to_http) = async_channel::unbounded::<String>();
    let (sender_http_to_udp, receiver_http_to_udp) = async_channel::unbounded::<String>();

    // Define http server
    let args_copy = Arc::new((*args).clone());
    let http_server = match HttpServer::new(move || {
        let args_clone = (*Arc::clone(&args_copy)).clone();
        App::new()
            .route("/", web::post().to(server_main_http_request_handler))
            .app_data(web::Data::new(AppState {
                args: args_clone,
                receiver_udp_to_http: receiver_udp_to_http.clone(),
                sender_http_to_udp: sender_http_to_udp.clone(),
            }))
            .app_data(web::PayloadConfig::new(1000000)) // 1mb payload limit
            .app_data(web::JsonConfig::default().limit(1000000)) // 1mb json limit
            .wrap(middleware::Logger::default())
    })
    .bind(http_server_addr)
    {
        Ok(bound) => bound,
        Err(e) => {
            error!("Error binding http server: {}", e);
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
        udp_relay_target_addr,
        tcp_keep_alive_ping_ms,
        sender_udp_to_http,
        receiver_http_to_udp,
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
        Err(_) => error!("Error in at least one listening task"),
        Ok(_) => info!("All listeners closed successfully"),
    };
}

/// Handler for the http server endpoints
async fn server_main_http_request_handler(
    app_state: web::Data<AppState>,
    post_data: web::Json<HttpData>,
) -> impl Responder {
    let args: &Args = &app_state.args;

    // TODO unsecure: this is not a constant-time-compare...
    if args.pre_shared_secret != post_data.secret {
        error!("Packet was sent to server with right format, but invalid pre-shared-secret");
        HttpResponse::Unauthorized();
    }

    // forward the packets that were sent in the post data
    let sender_http_to_udp = app_state.sender_http_to_udp.clone();
    for packet in &post_data.data {
        if sender_http_to_udp.send(packet.clone()).await.is_err() {
            error!("Writing into the internal back-comm channel failed");
        }
    }

    // read the packets to send back from the internal channel
    let mut data = HttpData {
        secret: String::from(&args.pre_shared_secret),
        data: Vec::new(),
    };
    let receiver_udp_to_http = app_state.receiver_udp_to_http.clone();
    loop {
        match receiver_udp_to_http.try_recv() {
            Ok(mes) => {
                data.data.push(mes);
            }
            Err(e) => {
                if !e.is_empty() {
                    error!("Receiver error, but it is not empty: {}", e);
                }
                break;
            }
        }
    }

    // return the data
    HttpResponse::Ok().json(data)
}

async fn udp_listener_server(
    shutdown_marker: Arc<AtomicBool>,
    listen_addr: SocketAddr,
    target_addr: SocketAddr,
    tcp_keep_alive_ping_ms: u64,
    sender_udp_to_http: async_channel::Sender<String>,
    receiver_http_to_udp: async_channel::Receiver<String>,
) -> std::io::Result<()> {
    let socket = match UdpSocket::bind(listen_addr).await {
        Err(e) => {
            error!("Error binding UDP listener: {}", e);
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

        // send all packets in the channel onward
        loop {
            match receiver_http_to_udp.try_recv() {
                Ok(packet) => {
                    match socket
                        .send_to(&base_64_decode_string_to_bytes(&packet), target_addr)
                        .await
                    {
                        Ok(len) => debug!("Forwarded {} bytes to {}", len, target_addr),
                        Err(e) => error!("Error when emitting udp packet: {}", e),
                    };
                }
                Err(e) => {
                    if !e.is_empty() {
                        error!("Back-receiver error, but it is not empty: {}", e);
                    }
                    break;
                }
            }
        }

        // try to receive packets on the udp port
        match time::timeout(
            Duration::from_millis(tcp_keep_alive_ping_ms),
            socket.recv_from(&mut buffer),
        )
        .await
        {
            Ok(Ok((len, src))) => {
                debug!("Received {} bytes from {}", len, src);
                if sender_udp_to_http
                    .send(base_64_encode_bytes_to_string(&buffer[..len]))
                    .await
                    .is_err()
                {
                    error!("Writing into the internal comm channel failed");
                }
            }
            Ok(Err(e)) => {
                error!("Error receiving UDP packet: {}", e);
            }
            Err(_) => {
                // Timeout expired, no data received -> this Err happens often and is expected, to assure the loop is cycled regularly
                trace!("No UDP packet received in the last keep alive. Continuing...");
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

    info!("Starting UDP listener at {}", udp_listener_addr);

    // Bind a UDP socket to the listening address
    let socket = match UdpSocket::bind(udp_listener_addr).await {
        Ok(s) => s,
        Err(e) => {
            error!("Error binding UDP listener: {}", e);
            return ();
        }
    };

    // define a buffer that is larger than the max possible udp-packet-size
    let mut buffer = vec![0u8; 66000];
    let mut target_address: SocketAddr = "127.0.0.1:0".parse().expect("Invalid target address");

    let http_client = Client::new();
    loop {
        match time::timeout(
            Duration::from_millis(tcp_keep_alive_ping_ms),
            socket.recv_from(&mut buffer),
        )
        .await
        {
            Ok(Ok((len, src))) => {
                debug!("Received {} bytes from {}", len, src);

                // update the target_address
                target_address = src.clone();

                // handle exchange
                http_packet_exchange(
                    &args,
                    &http_client,
                    &server_url,
                    Some((len, &buffer)),
                    &socket,
                    &target_address,
                )
                .await;
            }
            Ok(Err(e)) => {
                error!("Error receiving UDP packet: {}", e);
            }
            Err(_) => {
                // Timeout expired, no data received -> this Err happens often and is expected, to assure the loop is cycled regularly
                trace!(
                    "No UDP packet received in the last keep alive. Poll backend for new data..."
                );
                http_packet_exchange(
                    &args,
                    &http_client,
                    &server_url,
                    None,
                    &socket,
                    &target_address,
                )
                .await;
            }
        }
    }
}

async fn http_packet_exchange(
    args: &Args,
    http_client: &Client,
    server_url: &str,
    udp_packet_content: Option<(usize, &Vec<u8>)>,
    udp_socket: &UdpSocket,
    target_addr: &SocketAddr,
) {
    // create payload
    let mut data = HttpData {
        secret: String::from(&args.pre_shared_secret),
        data: Vec::new(),
    };
    if let Some((len, buffer)) = udp_packet_content {
        data.data
            .push(base_64_encode_bytes_to_string(&buffer[..len]));
    }

    // Perform HTTP request
    let res = match http_client.post(server_url).json(&data).send().await {
        Err(e) => {
            error!("Error when contacting server: {}", e);
            return ();
        }
        Ok(res) => res,
    };

    let status = res.status();
    if !status.is_success() {
        error!(
            "Got back faulty code from tunnel exchange: {}",
            status.as_u16()
        );
        return ();
    }

    // attempt parse the received json response
    let body = match res.json::<HttpData>().await {
        Err(e) => {
            error!("Server answered, but json body could not be parsed: {}", e);
            return ();
        }
        Ok(b) => b,
    };

    // check returned pre-shared-secret
    if body.secret != args.pre_shared_secret {
        error!("Packet was received from server with right format, but invalid pre-shared-secret");
        return ();
    }

    // emit all the udp packets we got, forwarded to the last port from that a message was sent to the client for tunneling
    for packet in body.data {
        match udp_socket
            .send_to(&base_64_decode_string_to_bytes(&packet), target_addr)
            .await
        {
            Ok(len) => debug!("Forwarded {} bytes to {}", len, target_addr),
            Err(e) => error!("Error when emitting udp packet: {}", e),
        };
    }
}
