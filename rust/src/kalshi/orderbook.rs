use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;

/// Side of the orderbook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Bid,
    Ask,
}

/// In-memory orderbook for a single market.
#[derive(Debug, Clone)]
pub struct Orderbook {
    pub ticker: String,
    pub bids: BTreeMap<Decimal, i64>, // price → size (sorted)
    pub asks: BTreeMap<Decimal, i64>,
    pub last_update: Instant,
}

impl Orderbook {
    pub fn new(ticker: String) -> Self {
        Self {
            ticker,
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            last_update: Instant::now(),
        }
    }

    /// Apply a snapshot (replaces all levels).
    pub fn apply_snapshot(&mut self, bids: Vec<(i64, i64)>, asks: Vec<(i64, i64)>) {
        self.bids.clear();
        for (price_cents, size) in bids {
            if let Some(p) = cents_to_decimal(price_cents) {
                if size > 0 {
                    self.bids.insert(p, size);
                }
            }
        }
        self.asks.clear();
        for (price_cents, size) in asks {
            if let Some(p) = cents_to_decimal(price_cents) {
                if size > 0 {
                    self.asks.insert(p, size);
                }
            }
        }
        self.last_update = Instant::now();
    }

    /// Apply a single price-level delta.
    pub fn apply_delta(&mut self, side: Side, price_cents: i64, delta: i64) {
        let Some(price) = cents_to_decimal(price_cents) else {
            return;
        };
        let book = match side {
            Side::Bid => &mut self.bids,
            Side::Ask => &mut self.asks,
        };

        let new_size = book.get(&price).copied().unwrap_or(0) + delta;
        if new_size <= 0 {
            book.remove(&price);
        } else {
            book.insert(price, new_size);
        }
        self.last_update = Instant::now();
    }
}

/// Thread-safe concurrent orderbook manager for all tracked markets.
pub struct OrderbookManager {
    books: DashMap<String, Orderbook>,
}

impl OrderbookManager {
    pub fn new() -> Self {
        Self {
            books: DashMap::new(),
        }
    }

    /// Get or create an orderbook for a ticker.
    pub fn get_or_create(&self, ticker: &str) -> dashmap::mapref::one::RefMut<'_, String, Orderbook> {
        self.books
            .entry(ticker.to_string())
            .or_insert_with(|| Orderbook::new(ticker.to_string()))
    }

    /// Apply a full orderbook snapshot.
    pub fn apply_snapshot(&self, ticker: &str, bids: Vec<(i64, i64)>, asks: Vec<(i64, i64)>) {
        let mut book = self.get_or_create(ticker);
        book.apply_snapshot(bids, asks);
    }

    /// Apply an incremental delta.
    pub fn apply_delta(&self, ticker: &str, side: Side, price_cents: i64, delta: i64) {
        let mut book = self.get_or_create(ticker);
        book.apply_delta(side, price_cents, delta);
    }

    /// Best bid price and size.
    pub fn best_bid(&self, ticker: &str) -> Option<(Decimal, i64)> {
        let book = self.books.get(ticker)?;
        book.bids.iter().next_back().map(|(p, s)| (*p, *s))
    }

    /// Best ask price and size.
    pub fn best_ask(&self, ticker: &str) -> Option<(Decimal, i64)> {
        let book = self.books.get(ticker)?;
        book.asks.iter().next().map(|(p, s)| (*p, *s))
    }

    /// Mid price = (best_bid + best_ask) / 2.
    pub fn mid_price(&self, ticker: &str) -> Option<Decimal> {
        let bid = self.best_bid(ticker)?.0;
        let ask = self.best_ask(ticker)?.0;
        Some((bid + ask) / Decimal::from(2))
    }

    /// Spread = best_ask - best_bid.
    pub fn spread(&self, ticker: &str) -> Option<Decimal> {
        let bid = self.best_bid(ticker)?.0;
        let ask = self.best_ask(ticker)?.0;
        Some(ask - bid)
    }

    /// Total depth at a specific price level.
    pub fn depth_at_price(&self, ticker: &str, side: Side, price: Decimal) -> i64 {
        let Some(book) = self.books.get(ticker) else {
            return 0;
        };
        let levels = match side {
            Side::Bid => &book.bids,
            Side::Ask => &book.asks,
        };
        levels.get(&price).copied().unwrap_or(0)
    }

    /// Estimated fill price for a given order size, walking the book.
    /// Iterates levels directly without collecting into a Vec.
    pub fn estimated_fill_price(&self, ticker: &str, side: Side, size: i64) -> Option<Decimal> {
        let book = self.books.get(ticker)?;

        let mut remaining = size;
        let mut total_cost = Decimal::ZERO;

        match side {
            // Buying: walk asks from lowest to highest
            Side::Bid => {
                for (price, level_size) in book.asks.iter() {
                    let fill = remaining.min(*level_size);
                    total_cost += *price * Decimal::from(fill);
                    remaining -= fill;
                    if remaining <= 0 {
                        break;
                    }
                }
            }
            // Selling: walk bids from highest to lowest
            Side::Ask => {
                for (price, level_size) in book.bids.iter().rev() {
                    let fill = remaining.min(*level_size);
                    total_cost += *price * Decimal::from(fill);
                    remaining -= fill;
                    if remaining <= 0 {
                        break;
                    }
                }
            }
        }

        if remaining > 0 {
            return None; // not enough liquidity
        }

        Some(total_cost / Decimal::from(size))
    }

    /// Order imbalance: bid_volume / (bid_volume + ask_volume).
    /// Returns 0.5 for balanced, >0.5 for buy pressure, <0.5 for sell pressure.
    pub fn order_imbalance(&self, ticker: &str) -> Option<f64> {
        let book = self.books.get(ticker)?;
        let bid_vol: i64 = book.bids.values().sum();
        let ask_vol: i64 = book.asks.values().sum();
        let total = bid_vol + ask_vol;
        if total == 0 {
            return None;
        }
        Some(bid_vol as f64 / total as f64)
    }

    /// Check if the orderbook data is stale.
    pub fn is_stale(&self, ticker: &str, max_age: Duration) -> bool {
        match self.books.get(ticker) {
            Some(book) => book.last_update.elapsed() > max_age,
            None => true,
        }
    }

    /// Remove an orderbook (when unsubscribing from a market).
    pub fn remove(&self, ticker: &str) {
        self.books.remove(ticker);
    }
}

impl Default for OrderbookManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert cents (i64) to Decimal (e.g., 75 → 0.75).
fn cents_to_decimal(cents: i64) -> Option<Decimal> {
    Decimal::from_i64(cents).map(|d| d / Decimal::from(100))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_orderbook_snapshot_and_queries() {
        let mgr = OrderbookManager::new();

        mgr.apply_snapshot(
            "TEST-TICKER",
            vec![(45, 10), (44, 20), (43, 30)], // bids
            vec![(55, 15), (56, 25), (57, 35)], // asks
        );

        // Best bid = 0.45 (highest)
        let (bid_price, bid_size) = mgr.best_bid("TEST-TICKER").unwrap();
        assert_eq!(bid_price, Decimal::from_str_exact("0.45").unwrap());
        assert_eq!(bid_size, 10);

        // Best ask = 0.55 (lowest)
        let (ask_price, ask_size) = mgr.best_ask("TEST-TICKER").unwrap();
        assert_eq!(ask_price, Decimal::from_str_exact("0.55").unwrap());
        assert_eq!(ask_size, 15);

        // Mid = (0.45 + 0.55) / 2 = 0.50
        let mid = mgr.mid_price("TEST-TICKER").unwrap();
        assert_eq!(mid, Decimal::from_str_exact("0.50").unwrap());

        // Spread = 0.55 - 0.45 = 0.10
        let spread = mgr.spread("TEST-TICKER").unwrap();
        assert_eq!(spread, Decimal::from_str_exact("0.10").unwrap());
    }

    #[test]
    fn test_delta_application() {
        let mgr = OrderbookManager::new();
        mgr.apply_snapshot("T", vec![(50, 10)], vec![(60, 10)]);

        // Add to existing bid level
        mgr.apply_delta("T", Side::Bid, 50, 5);
        assert_eq!(mgr.depth_at_price("T", Side::Bid, Decimal::from_str_exact("0.50").unwrap()), 15);

        // Remove from bid level (goes to zero → removed)
        mgr.apply_delta("T", Side::Bid, 50, -15);
        assert_eq!(mgr.depth_at_price("T", Side::Bid, Decimal::from_str_exact("0.50").unwrap()), 0);
    }

    #[test]
    fn test_estimated_fill_price() {
        let mgr = OrderbookManager::new();
        mgr.apply_snapshot(
            "T",
            vec![],
            vec![(55, 10), (56, 20), (57, 30)],
        );

        // Small order fills at best ask
        let fill = mgr.estimated_fill_price("T", Side::Bid, 5).unwrap();
        assert_eq!(fill, Decimal::from_str_exact("0.55").unwrap());

        // Larger order walks the book: 10@55 + 5@56 = 830/15
        let fill = mgr.estimated_fill_price("T", Side::Bid, 15).unwrap();
        let expected = (Decimal::from_str_exact("0.55").unwrap() * Decimal::from(10)
            + Decimal::from_str_exact("0.56").unwrap() * Decimal::from(5))
            / Decimal::from(15);
        assert_eq!(fill, expected);

        // Too large → None
        assert!(mgr.estimated_fill_price("T", Side::Bid, 100).is_none());
    }

    #[test]
    fn test_order_imbalance() {
        let mgr = OrderbookManager::new();
        mgr.apply_snapshot("T", vec![(50, 100)], vec![(60, 100)]);

        let imbalance = mgr.order_imbalance("T").unwrap();
        assert!((imbalance - 0.5).abs() < 0.001);

        // Skew towards bids
        mgr.apply_delta("T", Side::Bid, 49, 200);
        let imbalance = mgr.order_imbalance("T").unwrap();
        assert!(imbalance > 0.5);
    }

    #[test]
    fn test_staleness() {
        let mgr = OrderbookManager::new();
        mgr.apply_snapshot("T", vec![(50, 10)], vec![(60, 10)]);

        assert!(!mgr.is_stale("T", Duration::from_secs(60)));
        assert!(mgr.is_stale("NONEXISTENT", Duration::from_secs(60)));
    }
}
