//! Feed health tracking for staleness detection.
//!
//! Each feed reports updates via `record_update()`. The execution engine
//! checks required feeds before order submission via `required_feeds_healthy()`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use serde::Serialize;
use tracing::warn;

/// Per-feed staleness thresholds.
const THRESHOLDS: &[(&str, u64)] = &[
    ("kalshi_ws", 5),
    ("coinbase", 5),
    ("binance_spot", 10),
    ("binance_futures", 5),
    ("deribit", 10),
];

/// Required feeds per strategy.
/// Crypto uses OR-based check: at least one spot venue must be healthy.
const CRYPTO_SPOT_VENUES: &[&str] = &["binance_spot", "coinbase"];
const WEATHER_REQUIRED: &[&str] = &["kalshi_ws"];

/// Thread-safe feed health tracker with granular scoring.
pub struct FeedHealth {
    pub last_update: DashMap<String, Instant>,
    thresholds: HashMap<String, Duration>,
    /// P50 latency thresholds per feed (Phase 5.8)
    p50_thresholds: HashMap<String, Duration>,
}

/// P50 latency thresholds per feed (in seconds).
const P50_THRESHOLDS: &[(&str, u64)] = &[
    ("kalshi_ws", 2),
    ("coinbase", 1),
    ("binance_spot", 1),
    ("binance_futures", 1),
    ("deribit", 5),
];

impl FeedHealth {
    pub fn new() -> Self {
        let thresholds = THRESHOLDS
            .iter()
            .map(|(name, secs)| (name.to_string(), Duration::from_secs(*secs)))
            .collect();

        let p50_thresholds = P50_THRESHOLDS
            .iter()
            .map(|(name, secs)| (name.to_string(), Duration::from_secs(*secs)))
            .collect();

        Self {
            last_update: DashMap::new(),
            thresholds,
            p50_thresholds,
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

    /// Phase 5.8: Compute a health score (0.0-1.0) for a specific feed.
    ///
    /// - 1.0: receiving data, latency < P50 threshold
    /// - 0.75: receiving data, latency > P50 but < staleness threshold
    /// - 0.50: receiving data, but within staleness threshold (intermittent)
    /// - 0.25: last update > threshold but < 2x threshold
    /// - 0.0: last update > 2x threshold or never updated
    pub fn health_score(&self, feed_name: &str) -> f64 {
        let threshold = match self.thresholds.get(feed_name) {
            Some(t) => *t,
            None => return 1.0, // Unknown feed — don't block
        };

        let p50 = self.p50_thresholds.get(feed_name)
            .copied()
            .unwrap_or(threshold / 2);

        match self.last_update.get(feed_name) {
            Some(last) => {
                let elapsed = last.elapsed();
                if elapsed < p50 {
                    1.0
                } else if elapsed < threshold {
                    0.75
                } else if elapsed < threshold * 2 {
                    0.25
                } else {
                    0.0
                }
            }
            None => 0.0, // Never updated
        }
    }

    /// Phase 5.8: Compute aggregate health for a strategy.
    pub fn strategy_health(&self, signal_type: &str) -> f64 {
        match signal_type {
            "crypto" => {
                // Best score among spot venues (OR-based)
                CRYPTO_SPOT_VENUES
                    .iter()
                    .map(|name| self.health_score(name))
                    .fold(0.0_f64, f64::max)
            }
            "weather" => {
                WEATHER_REQUIRED
                    .iter()
                    .map(|name| self.health_score(name))
                    .fold(f64::INFINITY, f64::min)
            }
            _ => 1.0,
        }
    }

    /// Phase 5.8: Get detailed health for all feeds.
    pub fn health_detail(&self) -> Vec<FeedHealthDetail> {
        THRESHOLDS
            .iter()
            .map(|(name, _)| {
                let score = self.health_score(name);
                let age_ms = self
                    .last_update
                    .get(*name)
                    .map(|t| t.elapsed().as_millis() as u64);
                FeedHealthDetail {
                    feed: name.to_string(),
                    score,
                    age_ms,
                    healthy: self.is_healthy(name),
                }
            })
            .collect()
    }

    /// Phase 5.8: System-wide health (minimum across all strategies).
    pub fn system_health(&self) -> f64 {
        let crypto = self.strategy_health("crypto");
        let weather = self.strategy_health("weather");
        crypto.min(weather)
    }

    /// Check if required feeds for a signal type are healthy.
    /// Crypto: at least one spot venue must be healthy (OR-based).
    /// Weather: all required feeds must be healthy (AND-based).
    /// Returns Ok(()) if healthy, or Err(list of stale feeds).
    pub fn required_feeds_healthy(&self, signal_type: &str) -> Result<(), Vec<String>> {
        match signal_type {
            "crypto" => {
                // OR-based: at least one spot venue must be healthy
                let any_healthy = CRYPTO_SPOT_VENUES.iter().any(|name| self.is_healthy(name));
                if any_healthy {
                    Ok(())
                } else {
                    let stale: Vec<String> = CRYPTO_SPOT_VENUES
                        .iter()
                        .filter(|name| !self.is_healthy(name))
                        .map(|name| name.to_string())
                        .collect();
                    warn!(
                        signal_type = signal_type,
                        stale_feeds = ?stale,
                        "all spot venues are stale"
                    );
                    Err(stale)
                }
            }
            "weather" => {
                let stale: Vec<String> = WEATHER_REQUIRED
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
            _ => Ok(()),
        }
    }
}

/// Detailed health info for a single feed.
#[derive(Debug, Clone, Serialize)]
pub struct FeedHealthDetail {
    pub feed: String,
    pub score: f64,
    pub age_ms: Option<u64>,
    pub healthy: bool,
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

        // Manually insert an old timestamp (binance_spot threshold = 10s)
        health
            .last_update
            .insert("binance_spot".to_string(), Instant::now() - Duration::from_secs(15));

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
        // No updates — all spot venues stale
        let result = health.required_feeds_healthy("crypto");
        assert!(result.is_err());
        let stale = result.unwrap_err();
        assert!(stale.contains(&"binance_spot".to_string()));
        assert!(stale.contains(&"coinbase".to_string()));
    }

    #[test]
    fn test_required_feeds_crypto_healthy_binance() {
        let health = FeedHealth::new();
        health.record_update("binance_spot");
        assert!(health.required_feeds_healthy("crypto").is_ok());
    }

    #[test]
    fn test_required_feeds_crypto_healthy_coinbase() {
        let health = FeedHealth::new();
        // Coinbase alone should be sufficient (OR-based)
        health.record_update("coinbase");
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

    // Phase 5.8 tests
    #[test]
    fn test_health_score_fresh() {
        let health = FeedHealth::new();
        health.record_update("binance_spot");
        // Just updated — should be 1.0
        assert_eq!(health.health_score("binance_spot"), 1.0);
    }

    #[test]
    fn test_health_score_never_updated() {
        let health = FeedHealth::new();
        assert_eq!(health.health_score("binance_spot"), 0.0);
    }

    #[test]
    fn test_health_score_stale() {
        let health = FeedHealth::new();
        // Insert timestamp beyond 2x threshold (binance_spot threshold = 10s)
        health
            .last_update
            .insert("binance_spot".to_string(), Instant::now() - Duration::from_secs(25));
        assert_eq!(health.health_score("binance_spot"), 0.0);
    }

    #[test]
    fn test_health_score_degraded() {
        let health = FeedHealth::new();
        // Between threshold and 2x threshold (threshold=10s, so 15s is > 10s but < 20s)
        health
            .last_update
            .insert("binance_spot".to_string(), Instant::now() - Duration::from_secs(15));
        assert_eq!(health.health_score("binance_spot"), 0.25);
    }

    #[test]
    fn test_health_score_unknown_feed() {
        let health = FeedHealth::new();
        assert_eq!(health.health_score("unknown_feed"), 1.0);
    }

    #[test]
    fn test_strategy_health() {
        let health = FeedHealth::new();
        // No updates — crypto health should be 0
        assert_eq!(health.strategy_health("crypto"), 0.0);

        // Update binance_spot — crypto health should be 1.0
        health.record_update("binance_spot");
        assert_eq!(health.strategy_health("crypto"), 1.0);
    }

    #[test]
    fn test_system_health() {
        let health = FeedHealth::new();
        // No updates — system health is 0
        assert_eq!(health.system_health(), 0.0);

        // Update all required feeds
        health.record_update("binance_spot");
        health.record_update("kalshi_ws");
        assert_eq!(health.system_health(), 1.0);
    }

    #[test]
    fn test_health_detail() {
        let health = FeedHealth::new();
        health.record_update("binance_spot");
        let detail = health.health_detail();
        assert!(!detail.is_empty());

        let bs = detail.iter().find(|d| d.feed == "binance_spot").unwrap();
        assert!(bs.healthy);
        assert_eq!(bs.score, 1.0);
        assert!(bs.age_ms.is_some());

        let cb = detail.iter().find(|d| d.feed == "coinbase").unwrap();
        assert!(!cb.healthy);
        assert_eq!(cb.score, 0.0);
    }
}
