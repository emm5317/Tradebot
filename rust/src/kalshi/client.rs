use super::auth::AuthManager;

/// HTTP client wrapper for the Kalshi REST API.
pub struct KalshiClient {
    http: reqwest::Client,
    base_url: String,
    auth: AuthManager,
}

impl KalshiClient {
    pub fn new(base_url: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url,
            auth: AuthManager::new(),
        }
    }

    // TODO: get_markets, place_order, cancel_order, get_positions, etc.
}
