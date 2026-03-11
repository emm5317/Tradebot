//! Trade tape — bounded circular buffer of recent trades with derived metrics.
//!
//! Tracks trade history for aggressor-side analysis, VWAP computation,
//! and volume monitoring.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// A single recorded trade.
#[derive(Debug, Clone)]
pub struct TradeRecord {
    pub ticker: String,
    pub price_cents: i64,
    pub count: i64,
    pub taker_side: String, // "yes" or "no"
    pub timestamp: Instant,
}

/// Bounded circular buffer of recent trades.
pub struct TradeTape {
    trades: VecDeque<TradeRecord>,
    max_size: usize,
}

impl TradeTape {
    pub fn new(max_size: usize) -> Self {
        Self {
            trades: VecDeque::with_capacity(max_size),
            max_size,
        }
    }

    /// Record a new trade.
    pub fn record(&mut self, trade: TradeRecord) {
        if self.trades.len() >= self.max_size {
            self.trades.pop_front();
        }
        self.trades.push_back(trade);
    }

    /// Trade aggressiveness over a time window.
    ///
    /// Returns a value in [-1.0, 1.0] where:
    /// - +1.0 = all takers buying "yes" (bullish aggression)
    /// - -1.0 = all takers selling "yes" / buying "no" (bearish aggression)
    /// -  0.0 = balanced
    pub fn aggressiveness(&self, window: Duration) -> f64 {
        let cutoff = Instant::now() - window;
        let mut yes_volume: i64 = 0;
        let mut no_volume: i64 = 0;

        for trade in self.trades.iter().rev() {
            if trade.timestamp < cutoff {
                break;
            }
            if trade.taker_side == "yes" {
                yes_volume += trade.count;
            } else {
                no_volume += trade.count;
            }
        }

        let total = yes_volume + no_volume;
        if total == 0 {
            return 0.0;
        }
        (yes_volume - no_volume) as f64 / total as f64
    }

    /// Total trade volume within a time window.
    pub fn recent_volume(&self, window: Duration) -> i64 {
        let cutoff = Instant::now() - window;
        self.trades
            .iter()
            .rev()
            .take_while(|t| t.timestamp >= cutoff)
            .map(|t| t.count)
            .sum()
    }

    /// Volume-weighted average price within a time window.
    pub fn vwap(&self, window: Duration) -> Option<f64> {
        let cutoff = Instant::now() - window;
        let mut total_value: f64 = 0.0;
        let mut total_volume: i64 = 0;

        for trade in self.trades.iter().rev() {
            if trade.timestamp < cutoff {
                break;
            }
            total_value += trade.price_cents as f64 * trade.count as f64;
            total_volume += trade.count;
        }

        if total_volume == 0 {
            return None;
        }

        // Convert from cents to decimal (e.g., 5500 → 0.55)
        Some(total_value / total_volume as f64 / 100.0)
    }

    /// Most recent trade for a specific ticker.
    pub fn last_trade(&self, ticker: &str) -> Option<&TradeRecord> {
        self.trades.iter().rev().find(|t| t.ticker == ticker)
    }

    /// Number of trades currently stored.
    pub fn len(&self) -> usize {
        self.trades.len()
    }

    pub fn is_empty(&self) -> bool {
        self.trades.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_trade(ticker: &str, price: i64, count: i64, side: &str) -> TradeRecord {
        TradeRecord {
            ticker: ticker.to_string(),
            price_cents: price,
            count,
            taker_side: side.to_string(),
            timestamp: Instant::now(),
        }
    }

    #[test]
    fn test_record_and_bounded() {
        let mut tape = TradeTape::new(3);
        tape.record(make_trade("A", 50, 10, "yes"));
        tape.record(make_trade("A", 51, 20, "no"));
        tape.record(make_trade("A", 52, 30, "yes"));
        assert_eq!(tape.len(), 3);

        // Adding a 4th should evict the oldest
        tape.record(make_trade("A", 53, 40, "no"));
        assert_eq!(tape.len(), 3);
    }

    #[test]
    fn test_aggressiveness_balanced() {
        let mut tape = TradeTape::new(100);
        tape.record(make_trade("A", 50, 10, "yes"));
        tape.record(make_trade("A", 50, 10, "no"));
        let aggr = tape.aggressiveness(Duration::from_secs(60));
        assert!((aggr).abs() < 0.001);
    }

    #[test]
    fn test_aggressiveness_bullish() {
        let mut tape = TradeTape::new(100);
        tape.record(make_trade("A", 50, 30, "yes"));
        tape.record(make_trade("A", 50, 10, "no"));
        let aggr = tape.aggressiveness(Duration::from_secs(60));
        assert!((aggr - 0.5).abs() < 0.001); // (30-10)/(30+10) = 0.5
    }

    #[test]
    fn test_aggressiveness_empty() {
        let tape = TradeTape::new(100);
        assert_eq!(tape.aggressiveness(Duration::from_secs(60)), 0.0);
    }

    #[test]
    fn test_recent_volume() {
        let mut tape = TradeTape::new(100);
        tape.record(make_trade("A", 50, 10, "yes"));
        tape.record(make_trade("A", 50, 20, "no"));
        tape.record(make_trade("A", 50, 30, "yes"));
        assert_eq!(tape.recent_volume(Duration::from_secs(60)), 60);
    }

    #[test]
    fn test_vwap() {
        let mut tape = TradeTape::new(100);
        // 10 @ 50 cents + 20 @ 60 cents = (500 + 1200) / 30 = 56.67 cents = 0.5667
        tape.record(make_trade("A", 50, 10, "yes"));
        tape.record(make_trade("A", 60, 20, "yes"));
        let vwap = tape.vwap(Duration::from_secs(60)).unwrap();
        assert!((vwap - 0.5667).abs() < 0.001);
    }

    #[test]
    fn test_vwap_empty() {
        let tape = TradeTape::new(100);
        assert!(tape.vwap(Duration::from_secs(60)).is_none());
    }

    #[test]
    fn test_last_trade() {
        let mut tape = TradeTape::new(100);
        tape.record(make_trade("A", 50, 10, "yes"));
        tape.record(make_trade("B", 60, 20, "no"));
        tape.record(make_trade("A", 55, 15, "no"));

        let last_a = tape.last_trade("A").unwrap();
        assert_eq!(last_a.price_cents, 55);

        let last_b = tape.last_trade("B").unwrap();
        assert_eq!(last_b.price_cents, 60);

        assert!(tape.last_trade("C").is_none());
    }
}
