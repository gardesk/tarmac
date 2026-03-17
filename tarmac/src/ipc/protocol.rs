use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    pub fn ok(data: serde_json::Value) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn ok_empty() -> Self {
        Self {
            success: true,
            data: None,
            error: None,
        }
    }

    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(msg.into()),
        }
    }
}

/// Get the socket path for IPC.
pub fn socket_path() -> std::path::PathBuf {
    if let Ok(path) = std::env::var("TARMAC_SOCKET") {
        return std::path::PathBuf::from(path);
    }
    let user = std::env::var("USER").unwrap_or_else(|_| "unknown".to_string());
    std::path::PathBuf::from(format!("/tmp/tarmac-{}.sock", user))
}
