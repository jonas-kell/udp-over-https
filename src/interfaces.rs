use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct HttpData {
    pub secret: String,
    pub data: Vec<String>,
}
