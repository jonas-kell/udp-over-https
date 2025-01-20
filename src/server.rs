use crate::args::Args;
use crate::base64::{base_64_decode_string_to_bytes, base_64_encode_bytes_to_string};
use crate::interfaces::HttpData;
use crate::CURRENT_PROTOCOL_VERSION;
use actix_web::{middleware, web, App, HttpResponse, HttpServer, Responder};
use async_channel;
use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::{net::UdpSocket, time};

#[derive(Debug)]
struct AppState {
    args: Args,
    receiver_udp_to_http: async_channel::Receiver<String>,
    sender_http_to_udp: async_channel::Sender<String>,
}

/// Start the HTTP server and UDP listener for the server
pub async fn run_server(args: &Args) -> () {
    let http_server_addr = format!("0.0.0.0:{}", args.http_port); // TCP address for HTTP server, always on all interfaces
    let udp_listener_addr: SocketAddr = format!("127.0.0.1:{}", args.udp_port)
        .parse()
        .expect("Invalid udp listen address");
    let udp_relay_target_addr: SocketAddr = format!("{}", args.udp_port_relay_target)
        .parse()
        .expect("Invalid udp relay target address");
    let listener_interrupt_ms = args.listener_interrupt_ms;

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
        listener_interrupt_ms,
        sender_udp_to_http,
        receiver_http_to_udp,
    );

    // spawn the async runtimes in parallel
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
    if post_data.version != CURRENT_PROTOCOL_VERSION {
        error!(
            "Client supplied protocol with version {}, but we wanted {}",
            post_data.version, CURRENT_PROTOCOL_VERSION
        );
        return HttpResponse::BadRequest().body("Wrong Protocol Version");
    }

    let args: &Args = &app_state.args;

    // TODO unsecure: this is not a constant-time-compare...
    if args.pre_shared_secret != post_data.secret {
        error!("Packet was sent to server with right format, but invalid pre-shared-secret");
        return HttpResponse::Unauthorized().body("No valid authorization");
    }

    let mut max_number_of_packets_to_send_back: u16 = match post_data.send_back_mess {
        None => u16::MAX,
        Some(v) => v,
    };

    // forward the packets that were sent in the post data
    for packet in &post_data.data {
        if app_state
            .sender_http_to_udp
            .send(packet.clone())
            .await
            .is_err()
        {
            error!("Writing into the internal back-comm channel failed");
            // This is no problem, as the channel must not be lossless. The TCP Protocol of the levels above will take care of this
        }
    }

    // read the packets to send back from the internal channel
    let mut data = HttpData {
        version: CURRENT_PROTOCOL_VERSION,
        add_messages: 0,
        send_back_mess: None,
        secret: String::from(&args.pre_shared_secret),
        data: Vec::new(),
    };

    while max_number_of_packets_to_send_back > 0 {
        match app_state.receiver_udp_to_http.try_recv() {
            Ok(mes) => {
                data.data.push(mes);
                max_number_of_packets_to_send_back -= 1;
            }
            Err(e) => {
                if !e.is_empty() {
                    error!("Receiver error, but it is not empty: {}", e);
                }
                // if the channel is empty, no use holding the connection open
                // TODO ACTUALLY maybe it would come in helpful and more efficient to have a little server-control here, but we'll keep it at client control at the moment
                break;
            }
        }
    }
    // write back how many messages congest the cannel upstream
    data.add_messages = match u16::try_from(app_state.receiver_udp_to_http.len()) {
        Err(_) => u16::MAX,
        Ok(v) => v,
    };

    // return the data
    HttpResponse::Ok().json(data)
}

async fn udp_listener_server(
    shutdown_marker: Arc<AtomicBool>,
    listen_addr: SocketAddr,
    target_addr: SocketAddr,
    listener_interrupt_ms: u64,
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
            Duration::from_millis(listener_interrupt_ms),
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
