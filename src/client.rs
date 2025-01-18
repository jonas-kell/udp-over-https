use crate::{
    args::Args,
    base64::{base_64_decode_string_to_bytes, base_64_encode_bytes_to_string},
    interfaces::HttpData,
};
use log_once::debug_once;
use reqwest::Client;
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{self, net::UdpSocket};

/// Listen on the UDP Port for the client and poll the server for packets to transfer
pub async fn run_client(args: &Args) -> () {
    let udp_listener_addr: SocketAddr = format!("127.0.0.1:{}", args.udp_port)
        .parse()
        .expect("Invalid udp listen address");
    let server_url = String::from(&args.http_server);
    let tcp_keep_alive_ping_ms = args.keep_alive_ms;
    let max_client_tunnel_ms = args.max_tunnel_ms;
    let listener_interrupt_ms = args.listener_interrupt_ms;

    info!("Starting UDP listener at {}", udp_listener_addr);

    // define local communication channel for relaying udp packets to the http responder to relay them
    let (sender_udp_to_http, receiver_udp_to_http) = async_channel::unbounded::<String>();
    let (sender_http_to_udp, receiver_http_to_udp) = async_channel::unbounded::<String>();

    // Define udp listener
    let udp_listener_shutdown_marker = Arc::new(AtomicBool::new(false));
    let udp_listener = udp_listener_client(
        Arc::clone(&udp_listener_shutdown_marker),
        udp_listener_addr,
        listener_interrupt_ms,
        sender_udp_to_http,
        receiver_http_to_udp,
    );

    // define http client poller
    let http_client_poller_shutdown_marker = Arc::new(AtomicBool::new(false));
    let http_client_poller = http_client_poller_handler(
        Arc::clone(&http_client_poller_shutdown_marker),
        tcp_keep_alive_ping_ms,
        max_client_tunnel_ms,
        server_url,
        sender_http_to_udp,
        receiver_udp_to_http,
        args.pre_shared_secret.clone(),
        args.force_http2,
    );

    // spawn the async runtimes in parallel
    let http_client_poller_task = tokio::spawn(http_client_poller);
    let udp_listener_task = tokio::spawn(udp_listener);
    let shutdown_task = tokio::spawn(async move {
        // listen for ctrl-c
        tokio::signal::ctrl_c().await.unwrap();

        // start shutdown of tasks
        udp_listener_shutdown_marker.store(true, Ordering::SeqCst);
        http_client_poller_shutdown_marker.store(true, Ordering::SeqCst);
    });

    // Wait for all tasks to complete
    // https://github.com/actix/actix-web/issues/2739#issuecomment-1107638674
    match tokio::try_join!(http_client_poller_task, udp_listener_task, shutdown_task) {
        Err(_) => error!("Error in at least one listening task"),
        Ok(_) => info!("All listeners closed successfully"),
    };
}

async fn udp_listener_client(
    shutdown_marker: Arc<AtomicBool>,
    udp_listener_addr: SocketAddr,
    listener_interrupt_ms: u64,
    sender_udp_to_http: async_channel::Sender<String>,
    receiver_http_to_udp: async_channel::Receiver<String>,
) -> std::io::Result<()> {
    // Bind a UDP socket to the listening address
    let socket = match UdpSocket::bind(udp_listener_addr).await {
        Ok(s) => s,
        Err(e) => {
            error!("Error binding UDP listener: {}", e);
            return Err(e);
        }
    };

    // this is kept to where the last received packet originated from, as udp hast no back-address
    let mut target_addr: SocketAddr = "127.0.0.1:0".parse().expect("Invalid target address");

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
        match tokio::time::timeout(
            Duration::from_millis(listener_interrupt_ms),
            socket.recv_from(&mut buffer),
        )
        .await
        {
            Ok(Ok((len, src))) => {
                debug!("Received {} bytes from {}", len, src);

                // update the target_address
                target_addr = src.clone();

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
        }
    }

    Ok(())
}

async fn http_client_poller_handler(
    shutdown_marker: Arc<AtomicBool>,
    tcp_keep_alive_ping_ms: u64,
    max_client_tunnel_ms: u64,
    server_url: String,
    sender_http_to_udp: async_channel::Sender<String>,
    receiver_udp_to_http: async_channel::Receiver<String>,
    pre_shared_secret: String,
    force_http2: bool,
) {
    let mut builder = Client::builder();
    if force_http2 {
        builder = builder.http2_prior_knowledge();
    }
    let http_client = match builder.build() {
        Err(e) => {
            error!("Error while building the polling client: {}", e);
            return ();
        }
        Ok(c) => c,
    };

    loop {
        if shutdown_marker.load(Ordering::SeqCst) {
            break;
        }

        // see if the buffer has something to send
        match tokio::time::timeout(
            Duration::from_millis(tcp_keep_alive_ping_ms),
            receiver_udp_to_http.recv(),
        )
        .await
        {
            Ok(Ok(packet_content)) => {
                trace!("Gotten packet from udp channel, sending immediately");
                tokio::task::spawn(http_packet_exchange(
                    http_client.clone(),
                    server_url.clone(),
                    Some(packet_content),
                    sender_http_to_udp.clone(),
                    max_client_tunnel_ms,
                    pre_shared_secret.clone(),
                ));
            }
            Ok(Err(e)) => {
                error!("Receiver error: {}", e);
            }
            Err(_) => {
                // Timeout expired, no data received -> this Err happens often and is expected, to assure the backend is pinged regularly
                trace!("Need to trigger ping because keep alive tcp limit reached");
                tokio::task::spawn(http_packet_exchange(
                    http_client.clone(),
                    server_url.clone(),
                    None,
                    sender_http_to_udp.clone(),
                    max_client_tunnel_ms,
                    pre_shared_secret.clone(),
                ));
            }
        };
    }
}

async fn http_packet_exchange(
    http_client: Client,
    server_url: String,
    udp_packet_content: Option<String>,
    sender_http_to_udp: async_channel::Sender<String>,
    max_client_tunnel_ms: u64,
    pre_shared_secret: String,
) {
    trace!("Start tunnel exchange");

    // create payload
    let mut data = HttpData {
        secret: pre_shared_secret.clone(),
        data: Vec::new(),
    };
    if let Some(packet) = udp_packet_content {
        data.data.push(packet);
    }

    // Perform HTTP request
    let res = match http_client
        .post(server_url)
        .json(&data)
        .timeout(Duration::from_millis(max_client_tunnel_ms))
        .send()
        .await
    {
        Err(e) => {
            error!("Error when contacting server: {}", e);
            return ();
        }
        Ok(res) => res,
    };

    trace!("HTTP-Communication over http-version: {:?}", res.version());
    if res.version() == reqwest::Version::HTTP_2 {
        debug_once!("HTTP-Version 2 communication enabled and working");
    }

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
    if body.secret != pre_shared_secret {
        error!("Packet was received from server with right format, but invalid pre-shared-secret");
        return ();
    }

    // forward the packets that were sent in the post data
    for packet in body.data {
        if sender_http_to_udp.send(packet.clone()).await.is_err() {
            error!("Writing into the internal back-comm channel failed");
        }
    }
}
