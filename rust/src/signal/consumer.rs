use serde::Deserialize;

/// Consumes trading signals from Redis pub/sub (published by Python).
#[derive(Debug, Deserialize)]
pub struct Signal {
    pub ticker: String,
    pub side: String,
    pub edge: f64,
    pub confidence: f64,
    pub source: String,
}

pub struct SignalConsumer {
    // TODO: Redis connection
}

impl SignalConsumer {
    pub fn new() -> Self {
        Self {}
    }

    pub async fn listen(&self) -> Result<(), Box<dyn std::error::Error>> {
        // TODO: subscribe to Redis channel, parse Signal, forward to execution
        Ok(())
    }
}
