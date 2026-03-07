use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum KalshiError {
    #[error("rate limited, retry after {retry_after:?}")]
    RateLimit { retry_after: Duration },

    #[error("insufficient funds")]
    InsufficientFunds,

    #[error("market closed")]
    MarketClosed,

    #[error("invalid order: {reason}")]
    InvalidOrder { reason: String },

    #[error("authentication failure")]
    AuthFailure,

    #[error("server error: {0}")]
    ServerError(u16),

    #[error("network error: {0}")]
    NetworkError(#[from] reqwest::Error),

    #[error("signing error: {0}")]
    SigningError(String),

    #[error("websocket error: {0}")]
    WebSocketError(String),

    #[error("{0}")]
    Other(String),
}
