use serde::Deserialize;

#[derive(Clone, Deserialize)]
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

    #[serde(default = "default_db_pool_size")]
    pub database_pool_size: u32,

    pub log_level: String,
    pub log_format: String,

    pub discord_webhook_url: Option<String>,
    pub http_port: u16,
}

fn default_db_pool_size() -> u32 {
    20
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
            database_pool_size = self.database_pool_size,
            http_port = self.http_port,
            log_level = %self.log_level,
            kalshi_base_url = %self.kalshi_base_url,
            "configuration loaded"
        );
    }
}

/// Custom Debug impl that redacts secrets.
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("database_url", &"[redacted]")
            .field("redis_url", &self.redis_url)
            .field("nats_url", &self.nats_url)
            .field("kalshi_api_key", &"[redacted]")
            .field("kalshi_private_key_path", &"[redacted]")
            .field("kalshi_base_url", &self.kalshi_base_url)
            .field("kalshi_ws_url", &self.kalshi_ws_url)
            .field("paper_mode", &self.paper_mode)
            .field("max_trade_size_cents", &self.max_trade_size_cents)
            .field("max_daily_loss_cents", &self.max_daily_loss_cents)
            .field("max_positions", &self.max_positions)
            .field("max_exposure_cents", &self.max_exposure_cents)
            .field("database_pool_size", &self.database_pool_size)
            .field("http_port", &self.http_port)
            .finish()
    }
}
