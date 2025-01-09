use clap::{Parser, ValueEnum};

#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Mode of operation: 'server' or 'client'
    #[arg(value_enum)]
    pub mode: Mode,
    /// Number of ms until at least once the client probes the server for new packets (only in client mode)
    #[arg(long, default_value_t = 1000)]
    pub keep_alive_ms: u64,
    /// Number of ms until interface listeners check for new tasks
    #[arg(long, default_value_t = 300)]
    pub listener_interrupt_ms: u64,
    /// Number of ms a single client tunnel pass is waited for before aborted (only in client mode)
    #[arg(long, default_value_t = 4000)]
    pub max_tunnel_ms: u64,
    /// The number of the port on that the program listens for udp packets
    #[arg(long, default_value_t = 9898)]
    pub udp_port: u32,
    /// The number of the port on that the program listens for http traffic (only in server mode)
    #[arg(long, default_value_t = 8888)]
    pub http_port: u32,
    /// Address of the udp relay target ("host:port", like "127.0.0.1:7777") (only in server mode)
    #[arg(long, default_value_t = String::from("127.0.0.1:7777"))]
    pub udp_port_relay_target: String,
    /// Address of the server that accepts http(s) traffic (only in client mode)
    #[arg(long, default_value_t = String::from("replace-with-proper-address"))]
    pub http_server: String,
    /// Pre-shared secret to improve security
    #[arg(long, default_value_t = String::from("very-nice-pre-shared-secret-please-replace-with-proper-secret"))]
    pub pre_shared_secret: String,
    /// Verbosity (-v for verbose mode)
    #[arg(short, long, action = clap::ArgAction::SetTrue)]
    pub verbose: bool,
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Debug)]
pub enum Mode {
    Server,
    Client,
}
