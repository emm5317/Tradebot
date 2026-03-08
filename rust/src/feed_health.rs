//! Feed health tracking for staleness detection.
//!
//! Each feed reports updates via `record_update()`. The execution engine
//! checks required feeds before order submission via `required_feeds_healthy()`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tracing::warn;

/// Per-feed staleness thresholds.
const THRESHOLDS: &[(&str, u64)] = &[
    ("kalshi_ws", 5),
    ("coinbase", 2),
    ("binance_spot", 2),
    ("binance_futures", 2),
    ("deribit", 10),
];

/// Required feeds per strategy.
const CRYPTO_REQUIRED: &[&str] = &["binance_spot"];
const WEATHER_REQUIRED: &[&str] = &["kalshi_ws"];

/// Thread-safe feed health tracker.
pub struct FeedHealth {
    last_update: DashMap<String, Instant>,
    thresholds: HashMap<String, Duration>,
}

impl FeedHealth {
    pub fn new() -> Self {
        let thresholds = THRESHOLDS
            .iter()
            .map(|(name, secs)| (name.to_string(), Duration::from_secs(*secs)))
            .collect();

        Self {
            last_update: DashMap::new(),
            thresholds,
        }
    }

    /// Record that a feed has received fresh data.
    pub fn record_update(&self, feed_name: &str) {
        self.last_update
            .insert(feed_name.to_string(), Instant::now());
    }

    /// Check if a specific feed is healthy (last update within threshold).
    pub fn is_healthy(&self, feed_name: &str) -> bool {
        let threshold = match self.thresholds.get(feed_name) {
            Some(t) => *t,
            None => return true, // Unknown feed — don't block
        };

        match self.last_update.get(feed_name) {
            Some(last) => last.elapsed() < threshold,
            None => false, // No update ever recorded — stale
        }
    }

    /// Check if all required feeds for a signal type are healthy.
    /// Returns Ok(()) if all healthy, or Err(list of stale feeds).
    pub fn required_feeds_healthy(&self, signal_type: &str) -> Result<(), Vec<String>> {
        let required = match signal_type {
            "crypto" => CRYPTO_REQUIRED,
            "weather" => WEATHER_REQUIRED,
            _ => return Ok(()),
        };

        let stale: Vec<String> = required
            .iter()
            .filter(|name| !self.is_healthy(name))
            .map(|name| name.to_string())
            .collect();

        if stale.is_empty() {
            Ok(())
        } else {
            warn!(
                signal_type = signal_type,
                stale_feeds = ?stale,
                "required feeds are stale"
            );
            Err(stale)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_healthy_when_recent() {
        let health = FeedHealth::new();
        health.record_update("binance_spot");
        assert!(health.is_healthy("binance_spot"));
    }

    #[test]
    fn test_stale_when_no_update() {
        let health = FeedHealth::new();
        // Never recorded an update for binance_spot
        assert!(!health.is_healthy("binance_spot"));
    }

    #[test]
    fn test_stale_after_threshold() {
        let health = FeedHealth::new();
        health.record_update("binance_spot");

        // Manually insert an old timestamp
        health
            .last_update
            .insert("binance_spot".to_string(), Instant::now() - Duration::from_secs(5));

        assert!(!health.is_healthy("binance_spot"));
    }

    #[test]
    fn test_unknown_feed_is_healthy() {
        let health = FeedHealth::new();
        // Unknown feeds don't block
        assert!(health.is_healthy("unknown_feed"));
    }

    #[test]
    fn test_required_feeds_crypto() {
        let health = FeedHealth::new();
        // No updates — crypto required feeds should be stale
        let result = health.required_feeds_healthy("crypto");
        assert!(result.is_err());
        let stale = result.unwrap_err();
        assert!(stale.contains(&"binance_spot".to_string()));
    }

    #[test]
    fn test_required_feeds_crypto_healthy() {
        let health = FeedHealth::new();
        health.record_update("binance_spot");
        assert!(health.required_feeds_healthy("crypto").is_ok());
    }

    #[test]
    fn test_required_feeds_weather() {
        let health = FeedHealth::new();
        assert!(health.required_feeds_healthy("weather").is_err());

        health.record_update("kalshi_ws");
        assert!(health.required_feeds_healthy("weather").is_ok());
    }

    #[test]
    fn test_unknown_signal_type_always_ok() {
        let health = FeedHealth::new();
        assert!(health.required_feeds_healthy("unknown").is_ok());
    }
}
