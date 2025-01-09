use crate::{
    args::Args,
    base64::{base_64_decode_string_to_bytes, base_64_encode_bytes_to_string},
    interfaces::HttpData,
};
use reqwest::Client;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::{net::UdpSocket, time};

/// Listen on the UDP Port for the client and poll the server for packets to transfer
pub async fn run_client(args: &Args) -> () {
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
