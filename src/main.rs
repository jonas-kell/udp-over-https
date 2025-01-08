use clap::{Parser, ValueEnum};
use std::net::SocketAddr;
use tokio::io;
use tokio::net::UdpSocket;

/// Simple program to greet a person
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Mode of operation: 'server' or 'client'
    #[arg(value_enum)]
    mode: Mode,

    /// Number of times to greet
    #[arg(short, long, default_value_t = 1)]
    count: u8,
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Debug)]
enum Mode {
    Server,
    Client,
}

#[tokio::main]
async fn main() -> io::Result<()> {
    let args = Args::parse();

    // Listening address and port
    let listen_addr: SocketAddr = "127.0.0.1:8080".parse().expect("Invalid listen address");

    // Target address and port
    let target_addr: SocketAddr = "127.0.0.1:9090".parse().expect("Invalid target address");

    // Bind a UDP socket to the listening address
    let socket = UdpSocket::bind(listen_addr).await?;
    println!("Listening on {}", listen_addr);

    let mut buffer = vec![0u8; 1024]; // Buffer to store incoming packets

    loop {
        // Receive a UDP packet
        let (len, src) = socket.recv_from(&mut buffer).await?;
        println!("Received {} bytes from {}", len, src);

        // Forward the packet to the target address
        socket.send_to(&buffer[..len], target_addr).await?;
        println!("Forwarded {} bytes to {}", len, target_addr);
    }
}
