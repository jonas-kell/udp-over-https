use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct HttpData {
    pub version: u8,
    pub queue_messages: u16,
    pub wait_ms: Option<u16>,
    pub send_back_mess: Option<u16>,
    pub secret: String,
    pub data: Vec<String>,
}
