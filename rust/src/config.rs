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
    /// Required confirmation string when PAPER_MODE=false.
    /// Must be set to "yes-i-understand-real-money" to enable live trading.
    #[serde(default)]
    pub confirm_live_trading: String,
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
    #[serde(default = "default_crypto_max_edge")]
    pub crypto_max_edge: f64,
    #[serde(default = "default_crypto_cooldown_secs")]
    pub crypto_cooldown_secs: u64,
    #[serde(default = "default_weather_cooldown_secs")]
    pub weather_cooldown_secs: u64,

    // Phase 10: Directional model guards
    #[serde(default = "default_crypto_max_market_disagreement")]
    pub crypto_max_market_disagreement: f64,
    #[serde(default = "default_crypto_directional_min_conviction")]
    pub crypto_directional_min_conviction: f64,

    // Phase 14: Configurable model parameters (previously hardcoded)
    #[serde(default = "default_crypto_vol_multiplier")]
    pub crypto_vol_multiplier: f64,
    #[serde(default = "default_crypto_prob_ceiling")]
    pub crypto_prob_ceiling: f64,
    #[serde(default = "default_crypto_risk_reward_max_ratio")]
    pub crypto_risk_reward_max_ratio: f64,
    #[serde(default = "default_crypto_kelly_fill_min")]
    pub crypto_kelly_fill_min: f64,

    // Phase 14: Tail compression and rate limiting
    #[serde(default = "default_crypto_compress_factor")]
    pub crypto_compress_factor: f64,
    #[serde(default = "default_crypto_max_signals_per_hour")]
    pub crypto_max_signals_per_hour: u32,

    // Phase 13: Per-asset enable flags (BTC default true, others false)
    #[serde(default = "default_true")]
    pub enable_crypto_btc: bool,
    #[serde(default)]
    pub enable_crypto_eth: bool,
    #[serde(default)]
    pub enable_crypto_sol: bool,
    #[serde(default)]
    pub enable_crypto_xrp: bool,
    #[serde(default)]
    pub enable_crypto_doge: bool,
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
    0.17 // ~10 seconds before close
}

fn default_crypto_entry_max_minutes() -> f64 {
    20.0
}

fn default_crypto_min_edge() -> f64 {
    0.08
}

fn default_crypto_min_kelly() -> f64 {
    0.04
}

fn default_crypto_min_confidence() -> f64 {
    0.40
}

fn default_crypto_max_edge() -> f64 {
    0.25
}

fn default_crypto_cooldown_secs() -> u64 {
    300
}

fn default_crypto_vol_multiplier() -> f64 {
    2.5
}

fn default_crypto_prob_ceiling() -> f64 {
    0.80
}

fn default_crypto_risk_reward_max_ratio() -> f64 {
    5.0
}

fn default_crypto_kelly_fill_min() -> f64 {
    0.02
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

fn default_crypto_compress_factor() -> f64 {
    0.20
}

fn default_crypto_max_signals_per_hour() -> u32 {
    20
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

fn default_true() -> bool {
    true
}

impl Config {
    pub fn from_env() -> Result<Self, envy::Error> {
        envy::from_env::<Self>()
    }

    /// Validate configuration bounds and cross-field invariants.
    /// Call after `from_env()` to catch misconfiguration before startup.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        // Live trading ceremony: require explicit confirmation when PAPER_MODE=false
        if !self.paper_mode && self.confirm_live_trading != "yes-i-understand-real-money" {
            errors.push(
                "PAPER_MODE=false requires CONFIRM_LIVE_TRADING=yes-i-understand-real-money".to_string(),
            );
        }

        // Trading limits must be positive
        if self.max_trade_size_cents <= 0 {
            errors.push(format!(
                "MAX_TRADE_SIZE_CENTS must be > 0, got {}",
                self.max_trade_size_cents
            ));
        }
        if self.max_daily_loss_cents <= 0 {
            errors.push(format!(
                "MAX_DAILY_LOSS_CENTS must be > 0, got {}",
                self.max_daily_loss_cents
            ));
        }
        if self.max_exposure_cents <= 0 {
            errors.push(format!(
                "MAX_EXPOSURE_CENTS must be > 0, got {}",
                self.max_exposure_cents
            ));
        }
        if self.max_positions == 0 {
            errors.push("MAX_POSITIONS must be > 0".to_string());
        }

        // Kelly fraction must be in (0, 1]
        if self.kelly_fraction_multiplier <= 0.0 || self.kelly_fraction_multiplier > 1.0 {
            errors.push(format!(
                "KELLY_FRACTION_MULTIPLIER must be in (0.0, 1.0], got {}",
                self.kelly_fraction_multiplier
            ));
        }

        // Crypto model parameters
        if self.crypto_vol_multiplier < 0.1 || self.crypto_vol_multiplier > 10.0 {
            errors.push(format!(
                "CRYPTO_VOL_MULTIPLIER must be in [0.1, 10.0], got {}",
                self.crypto_vol_multiplier
            ));
        }
        if self.crypto_prob_ceiling < 0.5 || self.crypto_prob_ceiling > 1.0 {
            errors.push(format!(
                "CRYPTO_PROB_CEILING must be in [0.5, 1.0], got {}",
                self.crypto_prob_ceiling
            ));
        }
        if self.crypto_compress_factor < 0.0 || self.crypto_compress_factor > 1.0 {
            errors.push(format!(
                "CRYPTO_COMPRESS_FACTOR must be in [0.0, 1.0], got {}",
                self.crypto_compress_factor
            ));
        }
        if self.crypto_risk_reward_max_ratio <= 0.0 {
            errors.push(format!(
                "CRYPTO_RISK_REWARD_MAX_RATIO must be > 0, got {}",
                self.crypto_risk_reward_max_ratio
            ));
        }

        // Edge/kelly/confidence must be in [0, 1]
        if self.crypto_min_edge < 0.0 || self.crypto_min_edge > 1.0 {
            errors.push(format!(
                "CRYPTO_MIN_EDGE must be in [0.0, 1.0], got {}",
                self.crypto_min_edge
            ));
        }
        if self.crypto_max_edge < 0.0 || self.crypto_max_edge > 1.0 {
            errors.push(format!(
                "CRYPTO_MAX_EDGE must be in [0.0, 1.0], got {}",
                self.crypto_max_edge
            ));
        }
        if self.crypto_min_kelly < 0.0 || self.crypto_min_kelly > 1.0 {
            errors.push(format!(
                "CRYPTO_MIN_KELLY must be in [0.0, 1.0], got {}",
                self.crypto_min_kelly
            ));
        }
        if self.crypto_min_confidence < 0.0 || self.crypto_min_confidence > 1.0 {
            errors.push(format!(
                "CRYPTO_MIN_CONFIDENCE must be in [0.0, 1.0], got {}",
                self.crypto_min_confidence
            ));
        }

        // Cross-field: min_edge < max_edge
        if self.crypto_min_edge >= self.crypto_max_edge {
            errors.push(format!(
                "CRYPTO_MIN_EDGE ({}) must be < CRYPTO_MAX_EDGE ({})",
                self.crypto_min_edge, self.crypto_max_edge
            ));
        }

        // Cross-field: entry time window
        if self.crypto_entry_min_minutes >= self.crypto_entry_max_minutes {
            errors.push(format!(
                "CRYPTO_ENTRY_MIN_MINUTES ({}) must be < CRYPTO_ENTRY_MAX_MINUTES ({})",
                self.crypto_entry_min_minutes, self.crypto_entry_max_minutes
            ));
        }
        if self.crypto_entry_min_minutes < 0.0 {
            errors.push(format!(
                "CRYPTO_ENTRY_MIN_MINUTES must be >= 0, got {}",
                self.crypto_entry_min_minutes
            ));
        }

        // Market disagreement / conviction in [0, 1]
        if self.crypto_max_market_disagreement < 0.0 || self.crypto_max_market_disagreement > 1.0 {
            errors.push(format!(
                "CRYPTO_MAX_MARKET_DISAGREEMENT must be in [0.0, 1.0], got {}",
                self.crypto_max_market_disagreement
            ));
        }

        // Signals per hour sanity
        if self.crypto_max_signals_per_hour == 0 {
            errors.push("CRYPTO_MAX_SIGNALS_PER_HOUR must be > 0".to_string());
        }

        // RTI config
        if self.rti_min_venues == 0 {
            errors.push("RTI_MIN_VENUES must be > 0".to_string());
        }

        // URL format checks
        if !self.kalshi_base_url.starts_with("https://") {
            errors.push(format!(
                "KALSHI_BASE_URL must start with https://, got '{}'",
                self.kalshi_base_url
            ));
        }
        if !self.kalshi_ws_url.starts_with("wss://") {
            errors.push(format!(
                "KALSHI_WS_URL must start with wss://, got '{}'",
                self.kalshi_ws_url
            ));
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Return list of enabled crypto assets based on config flags.
    pub fn enabled_crypto_assets(&self) -> Vec<crate::crypto_asset::CryptoAsset> {
        use crate::crypto_asset::CryptoAsset;
        let mut assets = Vec::new();
        if self.enable_crypto_btc { assets.push(CryptoAsset::BTC); }
        if self.enable_crypto_eth { assets.push(CryptoAsset::ETH); }
        if self.enable_crypto_sol { assets.push(CryptoAsset::SOL); }
        if self.enable_crypto_xrp { assets.push(CryptoAsset::XRP); }
        if self.enable_crypto_doge { assets.push(CryptoAsset::DOGE); }
        assets
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
            crypto_min_kelly = self.crypto_min_kelly,
            crypto_min_confidence = self.crypto_min_confidence,
            crypto_vol_multiplier = self.crypto_vol_multiplier,
            crypto_prob_ceiling = self.crypto_prob_ceiling,
            crypto_risk_reward_max_ratio = self.crypto_risk_reward_max_ratio,
            crypto_kelly_fill_min = self.crypto_kelly_fill_min,
            crypto_compress_factor = self.crypto_compress_factor,
            crypto_max_signals_per_hour = self.crypto_max_signals_per_hour,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a Config with valid defaults for testing.
    fn valid_config() -> Config {
        Config {
            database_url: "postgres://localhost/test".to_string(),
            redis_url: "redis://localhost:6379".to_string(),
            nats_url: "nats://localhost:4222".to_string(),
            kalshi_api_key: "test-key".to_string(),
            kalshi_private_key_path: "test.pem".to_string(),
            kalshi_base_url: "https://api.kalshi.com".to_string(),
            kalshi_ws_url: "wss://api.kalshi.com/ws".to_string(),
            binance_ws_url: default_binance_spot_ws_url(),
            mesonet_base_url: "https://mesonet.agron.iastate.edu".to_string(),
            coinbase_ws_url: default_coinbase_ws_url(),
            binance_futures_ws_url: default_binance_futures_ws_url(),
            deribit_ws_url: default_deribit_ws_url(),
            binance_spot_ws_url: default_binance_spot_ws_url(),
            enable_coinbase: false,
            enable_binance_futures: false,
            enable_binance_spot: false,
            enable_deribit: false,
            paper_mode: true,
            confirm_live_trading: String::new(),
            max_trade_size_cents: 2500,
            max_daily_loss_cents: 10000,
            max_positions: 5,
            max_exposure_cents: 15000,
            kelly_fraction_multiplier: 0.25,
            database_pool_size: default_db_pool_size(),
            log_level: "info".to_string(),
            log_format: "json".to_string(),
            discord_webhook_url: None,
            http_port: 3030,
            rti_stale_threshold_secs: default_rti_stale_threshold_secs(),
            rti_outlier_threshold_pct: default_rti_outlier_threshold_pct(),
            rti_min_venues: default_rti_min_venues(),
            kill_switch_all: false,
            kill_switch_crypto: false,
            kill_switch_weather: false,
            crypto_entry_min_minutes: default_crypto_entry_min_minutes(),
            crypto_entry_max_minutes: default_crypto_entry_max_minutes(),
            crypto_min_edge: default_crypto_min_edge(),
            crypto_min_kelly: default_crypto_min_kelly(),
            crypto_min_confidence: default_crypto_min_confidence(),
            crypto_max_edge: default_crypto_max_edge(),
            crypto_cooldown_secs: default_crypto_cooldown_secs(),
            weather_cooldown_secs: default_weather_cooldown_secs(),
            crypto_max_market_disagreement: default_crypto_max_market_disagreement(),
            crypto_directional_min_conviction: default_crypto_directional_min_conviction(),
            crypto_vol_multiplier: default_crypto_vol_multiplier(),
            crypto_prob_ceiling: default_crypto_prob_ceiling(),
            crypto_risk_reward_max_ratio: default_crypto_risk_reward_max_ratio(),
            crypto_kelly_fill_min: default_crypto_kelly_fill_min(),
            crypto_compress_factor: default_crypto_compress_factor(),
            crypto_max_signals_per_hour: default_crypto_max_signals_per_hour(),
            enable_crypto_btc: true,
            enable_crypto_eth: false,
            enable_crypto_sol: false,
            enable_crypto_xrp: false,
            enable_crypto_doge: false,
        }
    }

    #[test]
    fn valid_config_passes_validation() {
        assert!(valid_config().validate().is_ok());
    }

    #[test]
    fn negative_trade_size_fails() {
        let mut c = valid_config();
        c.max_trade_size_cents = -1;
        let errs = c.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.contains("MAX_TRADE_SIZE_CENTS")));
    }

    #[test]
    fn zero_max_positions_fails() {
        let mut c = valid_config();
        c.max_positions = 0;
        let errs = c.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.contains("MAX_POSITIONS")));
    }

    #[test]
    fn kelly_out_of_range_fails() {
        let mut c = valid_config();
        c.kelly_fraction_multiplier = 1.5;
        let errs = c.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.contains("KELLY_FRACTION_MULTIPLIER")));
    }

    #[test]
    fn vol_multiplier_bounds() {
        let mut c = valid_config();
        c.crypto_vol_multiplier = 0.0;
        assert!(c.validate().is_err());
        c.crypto_vol_multiplier = 15.0;
        assert!(c.validate().is_err());
        c.crypto_vol_multiplier = 2.5;
        assert!(c.validate().is_ok());
    }

    #[test]
    fn min_edge_must_be_less_than_max_edge() {
        let mut c = valid_config();
        c.crypto_min_edge = 0.30;
        c.crypto_max_edge = 0.25;
        let errs = c.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.contains("CRYPTO_MIN_EDGE") && e.contains("CRYPTO_MAX_EDGE")));
    }

    #[test]
    fn entry_time_window_cross_check() {
        let mut c = valid_config();
        c.crypto_entry_min_minutes = 25.0;
        c.crypto_entry_max_minutes = 20.0;
        let errs = c.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.contains("ENTRY_MIN_MINUTES")));
    }

    #[test]
    fn url_format_validation() {
        let mut c = valid_config();
        c.kalshi_base_url = "http://insecure.com".to_string();
        let errs = c.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.contains("KALSHI_BASE_URL") && e.contains("https://")));
    }

    #[test]
    fn ws_url_must_be_wss() {
        let mut c = valid_config();
        c.kalshi_ws_url = "ws://insecure.com/ws".to_string();
        let errs = c.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.contains("KALSHI_WS_URL") && e.contains("wss://")));
    }

    #[test]
    fn prob_ceiling_bounds() {
        let mut c = valid_config();
        c.crypto_prob_ceiling = 0.3;
        assert!(c.validate().is_err());
        c.crypto_prob_ceiling = 1.1;
        assert!(c.validate().is_err());
        c.crypto_prob_ceiling = 0.80;
        assert!(c.validate().is_ok());
    }

    #[test]
    fn multiple_errors_collected() {
        let mut c = valid_config();
        c.max_trade_size_cents = -1;
        c.max_daily_loss_cents = 0;
        c.max_positions = 0;
        let errs = c.validate().unwrap_err();
        assert!(errs.len() >= 3);
    }

    #[test]
    fn live_mode_without_confirmation_fails() {
        let mut c = valid_config();
        c.paper_mode = false;
        c.confirm_live_trading = String::new();
        let errs = c.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.contains("CONFIRM_LIVE_TRADING")));
    }

    #[test]
    fn live_mode_with_wrong_confirmation_fails() {
        let mut c = valid_config();
        c.paper_mode = false;
        c.confirm_live_trading = "yes".to_string();
        let errs = c.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.contains("CONFIRM_LIVE_TRADING")));
    }

    #[test]
    fn live_mode_with_correct_confirmation_passes() {
        let mut c = valid_config();
        c.paper_mode = false;
        c.confirm_live_trading = "yes-i-understand-real-money".to_string();
        assert!(c.validate().is_ok());
    }

    #[test]
    fn paper_mode_does_not_require_confirmation() {
        let c = valid_config(); // paper_mode=true, confirm_live_trading=""
        assert!(c.validate().is_ok());
    }

    #[test]
    fn debug_redacts_secrets() {
        let c = valid_config();
        let debug = format!("{:?}", c);
        assert!(debug.contains("[redacted]"));
        assert!(!debug.contains("test-key"));
        assert!(!debug.contains("test.pem"));
        assert!(!debug.contains("postgres://"));
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
