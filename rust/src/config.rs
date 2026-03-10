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

    #[serde(default = "default_coinbase_ws_url")]
    pub coinbase_ws_url: String,
    #[serde(default = "default_binance_futures_ws_url")]
    pub binance_futures_ws_url: String,
    #[serde(default = "default_deribit_ws_url")]
    pub deribit_ws_url: String,
    #[serde(default = "default_binance_spot_ws_url")]
    pub binance_spot_ws_url: String,
    #[serde(default)]
    pub enable_coinbase: bool,
    #[serde(default)]
    pub enable_binance_futures: bool,
    #[serde(default)]
    pub enable_binance_spot: bool,
    #[serde(default)]
    pub enable_deribit: bool,

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

    // RTI venue weighting (Phase 4.2)
    #[serde(default = "default_rti_stale_threshold_secs")]
    pub rti_stale_threshold_secs: u64,
    #[serde(default = "default_rti_outlier_threshold_pct")]
    pub rti_outlier_threshold_pct: f64,
    #[serde(default = "default_rti_min_venues")]
    pub rti_min_venues: usize,

    // Kill switches (default: false = trading enabled)
    #[serde(default)]
    pub kill_switch_all: bool,
    #[serde(default)]
    pub kill_switch_crypto: bool,
    #[serde(default)]
    pub kill_switch_weather: bool,

    // Crypto evaluator tuning
    #[serde(default = "default_crypto_entry_min_minutes")]
    pub crypto_entry_min_minutes: f64,
    #[serde(default = "default_crypto_entry_max_minutes")]
    pub crypto_entry_max_minutes: f64,
    #[serde(default = "default_crypto_min_edge")]
    pub crypto_min_edge: f64,
    #[serde(default = "default_crypto_min_kelly")]
    pub crypto_min_kelly: f64,
    #[serde(default = "default_crypto_min_confidence")]
    pub crypto_min_confidence: f64,
    #[serde(default = "default_crypto_cooldown_secs")]
    pub crypto_cooldown_secs: u64,
    #[serde(default = "default_weather_cooldown_secs")]
    pub weather_cooldown_secs: u64,

    // Phase 10: Directional model guards
    #[serde(default = "default_crypto_max_market_disagreement")]
    pub crypto_max_market_disagreement: f64,
    #[serde(default = "default_crypto_directional_min_conviction")]
    pub crypto_directional_min_conviction: f64,
}

fn default_db_pool_size() -> u32 {
    20
}

fn default_coinbase_ws_url() -> String {
    "wss://advanced-trade-ws.coinbase.com".to_string()
}

fn default_binance_futures_ws_url() -> String {
    "wss://fstream.binance.com".to_string()
}

fn default_binance_spot_ws_url() -> String {
    "wss://stream.binance.us:9443/ws/btcusd@trade".to_string()
}

fn default_deribit_ws_url() -> String {
    "wss://www.deribit.com/ws/api/v2".to_string()
}

fn default_crypto_entry_min_minutes() -> f64 {
    3.0
}

fn default_crypto_entry_max_minutes() -> f64 {
    20.0
}

fn default_crypto_min_edge() -> f64 {
    0.06
}

fn default_crypto_min_kelly() -> f64 {
    0.04
}

fn default_crypto_min_confidence() -> f64 {
    0.50
}

fn default_crypto_cooldown_secs() -> u64 {
    30
}

fn default_weather_cooldown_secs() -> u64 {
    120
}

fn default_crypto_max_market_disagreement() -> f64 {
    0.25
}

fn default_crypto_directional_min_conviction() -> f64 {
    0.05
}

fn default_rti_stale_threshold_secs() -> u64 {
    5
}

fn default_rti_outlier_threshold_pct() -> f64 {
    0.5
}

fn default_rti_min_venues() -> usize {
    2
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
            crypto_entry_window = %format!("{}-{} min", self.crypto_entry_min_minutes, self.crypto_entry_max_minutes),
            crypto_min_edge = self.crypto_min_edge,
            crypto_min_confidence = self.crypto_min_confidence,
            crypto_cooldown_secs = self.crypto_cooldown_secs,
            weather_cooldown_secs = self.weather_cooldown_secs,
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
