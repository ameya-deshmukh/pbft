use serde::{Serialize, Deserialize};
use blake2::{Blake2b, Digest};

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    operation: String,
    timestamp: u64,
    client: Option<String>,
}

impl Request {
    pub fn from(s: &String) -> Self {
        serde_json::from_str(s).unwrap()
    }

    pub fn operation(&self) -> String {
        self.operation.clone()
    }
}

#[derive(Debug)]
pub struct PrePrepare {
    // view indicates the view in which the message is being sent
    view: u64,
    // sequence number for pre-prepare messages
    n: u64,
    // client message's digest
    digest: String,
}

impl PrePrepare {
    pub fn from(view: u64, n: u64, message: String) -> Self {
        let hash = Blake2b::digest(message.as_bytes());
        let digest = format!("{:x}", hash);
        Self { view, n, digest }
    }
}

pub struct PrePrepareSequence {
    value: u64,
}

impl PrePrepareSequence {
    pub fn new() -> Self {
        Self { value: 0 }
    }

    pub fn increment(&mut self) {
        self.value += 1
    }

    pub fn value(&self) -> u64 {
        self.value
    }
}