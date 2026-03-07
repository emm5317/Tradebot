/// Manages Kalshi API authentication and token refresh.

pub struct AuthManager {
    token: Option<String>,
    // TODO: expiry tracking, auto-refresh
}

impl AuthManager {
    pub fn new() -> Self {
        Self { token: None }
    }

    pub async fn login(&mut self, _email: &str, _password: &str) -> Result<(), Box<dyn std::error::Error>> {
        // TODO: POST /login, store token
        Ok(())
    }

    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }
}
