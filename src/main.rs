use std::net::SocketAddr;
use tokio::net::UdpSocket;
use tokio::io;

#[tokio::main]
async fn main() -> io::Result<()> {
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