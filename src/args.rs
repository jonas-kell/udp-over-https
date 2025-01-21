use clap::{Parser, ValueEnum};

#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Mode of operation: 'server' or 'client'
    #[arg(value_enum)]
    pub mode: Mode,
    /// Number of ms until at least once the client probes the server for new packets (only in client mode)
    #[arg(long, default_value_t = 100)]
    pub keep_alive_ms: u64,
    /// Number of ms the server is waiting max before sending back an empty packet-aggregation (only in client mode)
    #[arg(long, default_value_t = 1)]
    pub max_server_delay_ms: u16,
    /// Maximum number of messages that can be carried over one http exchange to/from the server (only in client mode)
    #[arg(long, default_value_t = 1)]
    pub max_number_of_aggregate_messages: usize,
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
    /// EXTRA Verbosity (--verbose_all for verbose mode)
    #[arg(long, action = clap::ArgAction::SetTrue)]
    pub verbose_all: bool,
    /// Force the usage of Http protocol version 2
    #[arg(long, action = clap::ArgAction::SetTrue)]
    pub force_http2: bool,
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Debug)]
pub enum Mode {
    Server,
    Client,
}
