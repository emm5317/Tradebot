use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub database_url: String,
    pub redis_url: String,
    pub nats_url: String,

    pub kalshi_api_key: String,
    pub kalshi_private_key_path: String,
    pub kalshi_base_url: String,
    pub kalshi_ws_url: String,

    pub binance_ws_url: String,
    pub mesonet_base_url: String,

    pub paper_mode: bool,
    pub max_trade_size_cents: i64,
    pub max_daily_loss_cents: i64,
    pub max_positions: usize,
    pub max_exposure_cents: i64,
    pub kelly_fraction_multiplier: f64,

    pub log_level: String,
    pub log_format: String,

    pub discord_webhook_url: Option<String>,
    pub http_port: u16,
}

impl Config {
    pub fn from_env() -> Result<Self, envy::Error> {
        envy::from_env::<Self>()
    }

    /// Log non-secret configuration values at startup.
    pub fn log_startup(&self) {
        tracing::info!(
            paper_mode = self.paper_mode,
            max_trade_size_cents = self.max_trade_size_cents,
            max_daily_loss_cents = self.max_daily_loss_cents,
            max_positions = self.max_positions,
            max_exposure_cents = self.max_exposure_cents,
            kelly_fraction = self.kelly_fraction_multiplier,
            http_port = self.http_port,
            log_level = %self.log_level,
            kalshi_base_url = %self.kalshi_base_url,
            "configuration loaded"
        );
    }
}
