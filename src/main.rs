use actix_web::{web, App, HttpResponse, HttpServer, Responder};
use clap::{Parser, ValueEnum};
use reqwest::Client;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::{net::UdpSocket, time};

/// Simple program demonstrating HTTP server and client modes with Actix
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Mode of operation: 'server' or 'client'
    #[arg(value_enum)]
    mode: Mode,
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
        Mode::Server => run_server().await,
        Mode::Client => run_client().await,
    }
}

/// Start the HTTP server
async fn run_server() -> std::io::Result<()> {
    let addr = "127.0.0.1:8080";
    let listen_addr: SocketAddr = "127.0.0.1:8081".parse().expect("Invalid listen address");

    println!("Starting server at http://{}", addr);

    HttpServer::new(|| App::new().route("/", web::get().to(hello_world)))
        .bind(addr)?
        .run()
        .await
}

/// Handler for the server's `/` endpoint
async fn hello_world() -> impl Responder {
    HttpResponse::Ok().json(serde_json::json!({ "message": "Hello, Actix!" }))
}

/// Run the client
async fn run_client() -> std::io::Result<()> {
    let listen_addr: SocketAddr = "127.0.0.1:8081".parse().expect("Invalid listen address");
    // let target_addr: SocketAddr = "127.0.0.1:9090".parse().expect("Invalid target address");
    let url = "http://127.0.0.1:8080";
    let tcp_keep_alive_ping_ms = 1000;

    // Bind a UDP socket to the listening address
    let socket = UdpSocket::bind(listen_addr).await?;
    println!("Listening on {}", listen_addr);

    let mut buffer = vec![0u8; 66000]; // Buffer to store incoming packets

    let client = Client::new();

    loop {
        // Timeout for waiting on UDP socket
        match time::timeout(
            Duration::from_millis(tcp_keep_alive_ping_ms),
            socket.recv_from(&mut buffer),
        )
        .await
        {
            Ok(Ok((len, src))) => {
                println!("Received {} bytes from {}", len, src);

                // Perform HTTP request
                let res = client
                    .get(url)
                    .send()
                    .await
                    .expect("Failed to send request");

                let status = res.status();
                let body = res.text().await.expect("Failed to read response body");

                println!("Response Status: {}", status);
                println!("Response Body: {}", body);
            }
            Ok(Err(e)) => {
                eprintln!("Error receiving UDP packet: {}", e);
            }
            Err(_) => {
                println!("Keep alive ping timing");
            }
        }
    }
}
