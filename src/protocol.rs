use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Get {
        key: String,
    },
    Put {
        key: String,
        value: Vec<u8>,
        ttl_secs: Option<u64>,
    },
    Clear,
    Stop,
    Status,
    Inspect,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    Miss,
    Hit { value: Vec<u8> },
    Done,
    Status(Status),
    Entries { entries: Vec<Entry> },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Status {
    pub pid: u32,
    pub uptime_secs: u64,
    pub idle_timeout_secs: Option<u64>,
    pub cached: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Entry {
    pub key: String,
    /// Enough of the value to recognize it, never the whole thing.
    pub preview: String,
    pub expires_in_secs: Option<u64>,
}
