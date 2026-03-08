//! Canonical crypto market state — single source of truth for all crypto feeds.
//!
//! All exchange feeds write to this struct via typed update methods.
//! Shadow RTI and derived signals recompute on every update.
//! The execution engine reads this directly — no Redis in the critical path.
//!
//! Phase 1.1: CryptoState struct with RwLock.

use std::sync::RwLock;
use std::time::Instant;

use tokio::sync::watch;
use tracing::trace;

/// Thread-safe canonical crypto state, shared across all feeds and execution.
pub struct CryptoState {
    inner: RwLock<CryptoStateInner>,
    /// Monotonic counter incremented on every state update.
    /// Crypto evaluator subscribes to this for event-driven evaluation.
    notify: watch::Sender<u64>,
}

/// Internal state fields. Write-locked by feeds, read-locked by execution.
#[derive(Debug, Clone)]
pub struct CryptoStateInner {
    // Coinbase
    pub coinbase_spot: f64,
    pub coinbase_bid: f64,
    pub coinbase_ask: f64,
    pub coinbase_updated: Option<Instant>,

    // Binance Spot
    pub binance_spot: f64,
    pub binance_spot_vol_realized: Option<f64>,
    pub binance_spot_vol_ewma: Option<f64>,
    pub binance_spot_bars_count: usize,
    pub binance_spot_updated: Option<Instant>,

    // Binance Futures
    pub perp_price: f64,
    pub mark_price: f64,
    pub funding_rate: f64,
    pub futures_obi: f64,
    pub futures_updated: Option<Instant>,

    // Deribit
    pub dvol: f64,
    pub dvol_updated: Option<Instant>,

    // Derived (recomputed on every update)
    pub shadow_rti: f64,
    pub basis: f64,
    pub best_vol: Option<f64>,
}

impl Default for CryptoStateInner {
    fn default() -> Self {
        Self {
            coinbase_spot: 0.0,
            coinbase_bid: 0.0,
            coinbase_ask: 0.0,
            coinbase_updated: None,
            binance_spot: 0.0,
            binance_spot_vol_realized: None,
            binance_spot_vol_ewma: None,
            binance_spot_bars_count: 0,
            binance_spot_updated: None,
            perp_price: 0.0,
            mark_price: 0.0,
            funding_rate: 0.0,
            futures_obi: 0.5,
            futures_updated: None,
            dvol: 0.0,
            dvol_updated: None,
            shadow_rti: 0.0,
            basis: 0.0,
            best_vol: None,
        }
    }
}

impl CryptoState {
    pub fn new() -> Self {
        let (notify, _) = watch::channel(0u64);
        Self {
            inner: RwLock::new(CryptoStateInner::default()),
            notify,
        }
    }

    /// Subscribe to state change notifications.
    /// The receiver fires on every update — watch coalesces rapid updates automatically.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.notify.subscribe()
    }

    /// Update from Coinbase feed.
    pub fn update_coinbase(&self, spot: f64, bid: f64, ask: f64) {
        let mut state = self.inner.write().unwrap();
        state.coinbase_spot = spot;
        state.coinbase_bid = bid;
        state.coinbase_ask = ask;
        state.coinbase_updated = Some(Instant::now());
        recompute_derived(&mut state);
        trace!(spot, shadow_rti = state.shadow_rti, "coinbase updated");
        drop(state);
        self.notify.send_modify(|v| *v += 1);
    }

    /// Update from Binance spot feed.
    pub fn update_binance_spot(
        &self,
        spot: f64,
        realized_vol: Option<f64>,
        ewma_vol: Option<f64>,
        bars_count: usize,
    ) {
        let mut state = self.inner.write().unwrap();
        state.binance_spot = spot;
        state.binance_spot_vol_realized = realized_vol;
        state.binance_spot_vol_ewma = ewma_vol;
        state.binance_spot_bars_count = bars_count;
        state.binance_spot_updated = Some(Instant::now());
        recompute_derived(&mut state);
        trace!(spot, shadow_rti = state.shadow_rti, "binance spot updated");
        drop(state);
        self.notify.send_modify(|v| *v += 1);
    }

    /// Update from Binance futures feed.
    pub fn update_binance_futures(
        &self,
        perp_price: f64,
        mark_price: f64,
        funding_rate: f64,
        obi: f64,
    ) {
        let mut state = self.inner.write().unwrap();
        state.perp_price = perp_price;
        state.mark_price = mark_price;
        state.funding_rate = funding_rate;
        state.futures_obi = obi;
        state.futures_updated = Some(Instant::now());
        recompute_derived(&mut state);
        drop(state);
        self.notify.send_modify(|v| *v += 1);
    }

    /// Update from Deribit DVOL feed.
    pub fn update_deribit(&self, dvol: f64) {
        let mut state = self.inner.write().unwrap();
        state.dvol = dvol;
        state.dvol_updated = Some(Instant::now());
        recompute_derived(&mut state);
        drop(state);
        self.notify.send_modify(|v| *v += 1);
    }

    /// Read-lock snapshot of current state.
    pub fn snapshot(&self) -> CryptoStateInner {
        self.inner.read().unwrap().clone()
    }
}

/// Recompute derived fields: shadow RTI, basis, best vol.
fn recompute_derived(state: &mut CryptoStateInner) {
    // Shadow RTI: weighted average of available spot prices
    // Coinbase 60%, Binance 40% (Coinbase is a CFB RTI constituent)
    let mut total_weight = 0.0;
    let mut weighted_sum = 0.0;

    if state.coinbase_spot > 0.0 {
        weighted_sum += state.coinbase_spot * 0.6;
        total_weight += 0.6;
    }
    if state.binance_spot > 0.0 {
        weighted_sum += state.binance_spot * 0.4;
        total_weight += 0.4;
    }

    if total_weight > 0.0 {
        state.shadow_rti = weighted_sum / total_weight;
    } else if state.mark_price > 0.0 {
        state.shadow_rti = state.mark_price;
    } else if state.perp_price > 0.0 {
        state.shadow_rti = state.perp_price;
    }

    // Basis: perp - shadow_rti
    if state.perp_price > 0.0 && state.shadow_rti > 0.0 {
        state.basis = state.perp_price - state.shadow_rti;
    } else {
        state.basis = 0.0;
    }

    // Best vol: prefer DVOL > EWMA > realized
    if state.dvol > 0.0 {
        // DVOL is annualized percentage (e.g. 52.3 means 52.3%)
        state.best_vol = Some(state.dvol / 100.0);
    } else if let Some(ewma) = state.binance_spot_vol_ewma {
        if ewma > 0.0 {
            state.best_vol = Some(ewma);
        }
    } else if let Some(realized) = state.binance_spot_vol_realized {
        if realized > 0.0 {
            state.best_vol = Some(realized);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shadow_rti_both_venues() {
        let cs = CryptoState::new();
        cs.update_coinbase(95000.0, 94990.0, 95010.0);
        cs.update_binance_spot(95100.0, None, None, 0);

        let snap = cs.snapshot();
        // 0.6 * 95000 + 0.4 * 95100 = 57000 + 38040 = 95040
        assert!((snap.shadow_rti - 95040.0).abs() < 0.01);
    }

    #[test]
    fn test_shadow_rti_coinbase_only() {
        let cs = CryptoState::new();
        cs.update_coinbase(95000.0, 94990.0, 95010.0);

        let snap = cs.snapshot();
        assert!((snap.shadow_rti - 95000.0).abs() < 0.01);
    }

    #[test]
    fn test_shadow_rti_binance_only() {
        let cs = CryptoState::new();
        cs.update_binance_spot(95100.0, None, None, 0);

        let snap = cs.snapshot();
        assert!((snap.shadow_rti - 95100.0).abs() < 0.01);
    }

    #[test]
    fn test_shadow_rti_fallback_to_mark() {
        let cs = CryptoState::new();
        cs.update_binance_futures(95300.0, 95200.0, 0.0001, 0.5);

        let snap = cs.snapshot();
        assert!((snap.shadow_rti - 95200.0).abs() < 0.01);
    }

    #[test]
    fn test_basis_computation() {
        let cs = CryptoState::new();
        cs.update_coinbase(95000.0, 0.0, 0.0);
        cs.update_binance_futures(95300.0, 95200.0, 0.0001, 0.5);

        let snap = cs.snapshot();
        // basis = perp (95300) - shadow_rti (95000)
        assert!((snap.basis - 300.0).abs() < 0.01);
    }

    #[test]
    fn test_best_vol_dvol_preferred() {
        let cs = CryptoState::new();
        cs.update_binance_spot(95000.0, Some(0.65), Some(0.70), 30);
        cs.update_deribit(52.3);

        let snap = cs.snapshot();
        assert!((snap.best_vol.unwrap() - 0.523).abs() < 0.001);
    }

    #[test]
    fn test_best_vol_ewma_fallback() {
        let cs = CryptoState::new();
        cs.update_binance_spot(95000.0, Some(0.65), Some(0.70), 30);

        let snap = cs.snapshot();
        assert!((snap.best_vol.unwrap() - 0.70).abs() < 0.001);
    }

    #[test]
    fn test_best_vol_realized_fallback() {
        let cs = CryptoState::new();
        cs.update_binance_spot(95000.0, Some(0.65), None, 30);

        let snap = cs.snapshot();
        assert!((snap.best_vol.unwrap() - 0.65).abs() < 0.001);
    }

    #[test]
    fn test_snapshot_is_consistent() {
        let cs = CryptoState::new();
        cs.update_coinbase(95000.0, 94990.0, 95010.0);
        cs.update_binance_spot(95100.0, Some(0.65), Some(0.70), 30);
        cs.update_binance_futures(95300.0, 95200.0, 0.0001, 0.55);
        cs.update_deribit(52.3);

        let snap = cs.snapshot();
        assert!(snap.shadow_rti > 0.0);
        assert!(snap.basis != 0.0);
        assert!(snap.best_vol.is_some());
        assert!(snap.coinbase_updated.is_some());
        assert!(snap.binance_spot_updated.is_some());
        assert!(snap.futures_updated.is_some());
        assert!(snap.dvol_updated.is_some());
    }
}
