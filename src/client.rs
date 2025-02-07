use crate::{
    args::Args,
    base64::{base_64_decode_string_to_bytes, base_64_encode_bytes_to_string},
    interfaces::HttpData,
    CURRENT_PROTOCOL_VERSION,
};
use log_once::debug_once;
use reqwest::Client;
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, SystemTime},
};
use tokio::{self, net::UdpSocket};

/// Listen on the UDP Port for the client and poll the server for packets to transfer
pub async fn run_client(args: &Args) -> () {
    let udp_listener_addr: SocketAddr = format!("127.0.0.1:{}", args.udp_port)
        .parse()
        .expect("Invalid udp listen address");
    let server_url = String::from(&args.http_server);
    let keep_alive_connections = args.keep_alive_connections;
    let max_client_tunnel_ms = args.max_tunnel_ms;
    let listener_interrupt_ms = args.listener_interrupt_ms;
    let max_number_of_aggregate_messages = args.max_number_of_aggregate_messages;
    let max_server_delay_ms = args.max_server_delay_ms;

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
        keep_alive_connections,
        max_server_delay_ms,
        max_client_tunnel_ms,
        max_number_of_aggregate_messages,
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
                    if target_addr.port() != 0 {
                        match socket
                            .send_to(&base_64_decode_string_to_bytes(&packet), target_addr)
                            .await
                        {
                            Ok(len) => debug!("Forwarded {} bytes to {}", len, target_addr),
                            Err(e) => error!("Error when emitting udp packet: {}", e),
                        };
                    } else {
                        trace!(
                            "Got packet, but no inbound packet yet, so no port to emit it back to"
                        );
                    }
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
    keep_alive_connections: u16,
    max_server_delay_ms: u16,
    max_client_tunnel_ms: u64,
    max_number_of_aggregate_messages: usize,
    server_url: String,
    sender_http_to_udp: async_channel::Sender<String>,
    receiver_udp_to_http: async_channel::Receiver<String>,
    pre_shared_secret: String,
    force_http2: bool,
) {
    // Arc-mutex that keeps track of current system status behavior
    let active_server_calls_nr: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
    let channel_time_times_hundred_tenner_rolling_average_ms: Arc<AtomicU32> =
        Arc::new(AtomicU32::new(0));
    let waiting_time_times_hundred_tenner_rolling_average_ms: Arc<AtomicU32> =
        Arc::new(AtomicU32::new(0));

    // Http client pool creation
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

    // main loop that reads from the buffer and correctly dispatches http-transit handlers
    loop {
        if shutdown_marker.load(Ordering::SeqCst) {
            break;
        }

        // see if the buffer has something to send
        let mut sending_data = HttpData {
            version: CURRENT_PROTOCOL_VERSION,
            queue_messages: 0, // will be filled later in the function
            send_back_mess: Some(match u16::try_from(max_number_of_aggregate_messages) {
                Ok(v) => v,
                Err(_) => 1,
            }), // TODO -> flow control
            wait_ms: Some(max_server_delay_ms), // TODO -> flow control
            secret: pre_shared_secret.clone(),
            data: Vec::new(),
        };

        match tokio::time::timeout(
            Duration::from_millis(keep_alive_connections.into()),
            receiver_udp_to_http.recv(),
        )
        .await
        {
            Ok(Ok(packet_content)) => {
                trace!("Gotten packet from udp channel, adding to queue to send");
                sending_data.data.push(packet_content);
            }
            Ok(Err(e)) => {
                error!("Receiver error: {}", e);
            }
            Err(_) => {
                // Timeout expired, no data received -> this Err happens often and is expected, to assure the backend is pinged regularly
                trace!("Waiting for message: timeout reached");
            }
        };

        // connection is being sent in any case, try to load it with as many additional packets as are instantly available and allowed
        let mut num_add_packets_remaining =
            std::cmp::max(1, max_number_of_aggregate_messages) - sending_data.data.len(); // data.len() is at most 1, at least 0
        while num_add_packets_remaining > 0 {
            match receiver_udp_to_http.try_recv() {
                Err(e) => {
                    if !e.is_empty() {
                        error!("Reading from channel error: {}", e);
                    }
                    break; // needs to break out of add-packet collection loop in ANY case on error
                }
                Ok(mes) => {
                    sending_data.data.push(mes); // additional packet on successful direct read
                    num_add_packets_remaining -= 1;
                }
            }
        }

        // to know in the task later how many messages are pending for logging info about the channel aggregated
        let num_extra_mess = receiver_udp_to_http.len();
        sending_data.queue_messages = match u16::try_from(num_extra_mess) {
            Err(_) => u16::MAX,
            Ok(v) => v,
        };
        if num_extra_mess > 0 {
            warn!("Sending queue still holds {} messages", num_extra_mess);
        }

        // the result of this does not influence the next step of the program, so this will be executed in the background
        tokio::task::spawn(http_packet_exchange(
            Arc::clone(&active_server_calls_nr),
            Arc::clone(&channel_time_times_hundred_tenner_rolling_average_ms),
            Arc::clone(&waiting_time_times_hundred_tenner_rolling_average_ms),
            http_client.clone(),
            server_url.clone(),
            sending_data,
            sender_http_to_udp.clone(),
            max_client_tunnel_ms,
        ));
    }
}

async fn http_packet_exchange(
    active_server_calls_nr: Arc<AtomicUsize>,
    channel_time_times_hundred_tenner_rolling_average_ms: Arc<AtomicU32>,
    waiting_time_times_hundred_tenner_rolling_average_ms: Arc<AtomicU32>,
    http_client: Client,
    server_url: String,
    data: HttpData,
    sender_http_to_udp: async_channel::Sender<String>,
    max_client_tunnel_ms: u64,
) {
    trace!("Start tunnel exchange");

    let num_packets_sent = data.data.len();
    let num_packets_in_client_queue = data.queue_messages;

    let start_exchange = SystemTime::now();

    active_server_calls_nr.fetch_add(1, Ordering::SeqCst);

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
            active_server_calls_nr.fetch_sub(1, Ordering::SeqCst);
            return (); // caution to decrement on error/early exit
        }
        Ok(res) => res,
    };

    trace!("HTTP-Communication over http-version: {:?}", res.version());
    if res.version() == reqwest::Version::HTTP_2 {
        debug_once!("HTTP-Version 2 communication enabled and working");
    }

    // check that the exchange succeeded in terms of code
    let status = res.status();
    if !status.is_success() {
        error!(
            "Got back faulty code from tunnel exchange: {}",
            status.as_u16()
        );
        active_server_calls_nr.fetch_sub(1, Ordering::SeqCst);
        return (); // caution to decrement on error/early exit
    }

    // attempt parse the received json response
    let body = match res.json::<HttpData>().await {
        Err(e) => {
            error!("Server answered, but json body could not be parsed: {}", e);
            active_server_calls_nr.fetch_sub(1, Ordering::SeqCst);
            return (); // caution to decrement on error/early exit
        }
        Ok(b) => {
            active_server_calls_nr.fetch_sub(1, Ordering::SeqCst); // default decrement
            b
        }
    };

    // request data is all back, one less request is on the wire
    let nr_running_requests = active_server_calls_nr.load(Ordering::SeqCst);

    // time the request took
    let exchange_duration_ms = match SystemTime::now().duration_since(start_exchange) {
        Err(_) => 0,
        Ok(d) => d.as_millis(),
    };

    // check returned pre-shared-secret (must also evidently be the same as the one send upstream)
    if body.secret != data.secret {
        error!("Packet was received from server with right format, but invalid pre-shared-secret");
        return ();
    }

    // collect logging data from the exchange (before sending on, because moving vec)
    let num_packets_returned = body.data.len();
    let num_packets_in_server_queue = body.queue_messages;
    let server_waited_for_ms = match body.wait_ms {
        Some(v) => v,
        None => 0,
    };

    // forward the packets that were sent in the post data
    for packet in body.data {
        if sender_http_to_udp.send(packet.clone()).await.is_err() {
            error!("Writing into the internal back-comm channel failed");
        }
    }

    // calculate the rolling average of how long the channel communication takes (without waiting at server)
    let rolling_avg_transfer = match channel_time_times_hundred_tenner_rolling_average_ms
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |x| {
            let current_rolling_val = f64::from(x) * 0.01;
            let this_wire_transfer = f64::from(u32::try_from(exchange_duration_ms).unwrap_or(10))
                - f64::from(server_waited_for_ms);

            let res = 0.9 * current_rolling_val + 0.1 * this_wire_transfer;

            Some((res * 100.0).round() as u32)
        }) {
        Err(e) => {
            error!(
                "Rolling average computation should not be able to error: {}",
                e
            );
            return ();
        }
        Ok(v) => v / 100,
    };
    let rolling_avg_waiting = match waiting_time_times_hundred_tenner_rolling_average_ms
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |x| {
            let current_rolling_val = f64::from(x) * 0.01;
            let this_waiting_time = f64::from(server_waited_for_ms);

            let res = 0.9 * current_rolling_val + 0.1 * this_waiting_time;

            Some((res * 100.0).round() as u32)
        }) {
        Err(e) => {
            error!(
                "Rolling average computation should not be able to error: {}",
                e
            );
            return ();
        }
        Ok(v) => v / 100,
    };

    // give a resume of what just happened // TODO downgrade to debug
    warn!("HTTP-Exchange finished. Sent {} and received {} udp-packets. Client queue has {} and server queue {} udp-packets. Took {}ms over the wire of which it waited {}ms. {} running requests, average wire-transfer takes {}ms and average wait {}ms", num_packets_sent, num_packets_returned, num_packets_in_client_queue, num_packets_in_server_queue, exchange_duration_ms, server_waited_for_ms, nr_running_requests, rolling_avg_transfer, rolling_avg_waiting);
}
