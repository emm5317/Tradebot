use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct AppConfig {
    pub kalshi_api_url: String,
    pub kalshi_email: String,
    pub kalshi_password: String,
    pub database_url: String,
    pub redis_url: String,
    pub max_position_size: f64,
    pub daily_loss_limit: f64,
}

impl AppConfig {
    pub fn from_env() -> Result<Self, envy::Error> {
        dotenvy::dotenv().ok();
        envy::from_env()
    }
}
