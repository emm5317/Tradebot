# Phase 1 — Crypto Architecture Refactor

**Timeline:** Weeks 2–6
**Risk:** HIGH
**Goal:** Move crypto decision-making from Python+Redis polling to Rust in-process, cutting latency from ~10s+200ms to sub-millisecond

---

## 1.1 CryptoState Struct (Canonical In-Process State)

### Problem
Crypto state is fragmented across multiple Redis keys (`crypto:coinbase`, `crypto:binance_futures`, `crypto:deribit_dvol`, `crypto:binance_spot`) with no atomic consistency guarantee. Each feed writes independently; the Python evaluator reads stale snapshots.

### Implementation

**New file:** `rust/src/crypto_state.rs`

```rust
pub struct CryptoState {
    inner: RwLock<CryptoStateInner>,
}

struct CryptoStateInner {
    // Coinbase
    coinbase_spot: f64,
    coinbase_bid: f64,
    coinbase_ask: f64,
    coinbase_updated: Instant,

    // Binance Spot
    binance_spot: f64,
    binance_spot_vol_realized: Option<f64>,
    binance_spot_vol_ewma: Option<f64>,
    binance_spot_updated: Instant,

    // Binance Futures
    perp_price: f64,
    mark_price: f64,
    funding_rate: f64,
    futures_updated: Instant,

    // Deribit
    dvol: f64,
    dvol_updated: Instant,

    // Derived (computed on write)
    shadow_rti: f64,  // 0.6 * coinbase + 0.4 * binance
    basis: f64,
    best_vol: Option<f64>,  // dvol > ewma > realized
}
```

- All feeds write via `crypto_state.update_coinbase(...)`, `update_binance_spot(...)`, etc.
- Shadow RTI recomputed on every update
- Feeds still flush to Redis for Python advisory/dashboard (backward compat)
- `Arc<CryptoState>` passed to feeds and execution engine

### Migration Path
- Phase 1.1: Create struct, feeds write to both CryptoState + Redis
- Phase 1.4: Remove Redis writes from crypto feeds

---

## 1.2 Inline Shadow RTI + N(d2) in Rust

### Problem
The crypto fair-value computation currently lives in Python (`models/crypto_fv.py`). It runs every 10s via polling, missing real-time price movements.

### Implementation

**New file:** `rust/src/crypto_fv.rs`

Port from `python/models/crypto_fv.py`:
- Shadow RTI: `0.6 * coinbase_spot + 0.4 * binance_spot`
- Time-scaled volatility: `vol * sqrt(minutes_remaining / 525_600)`
- N(d2) binary probability: standard normal CDF of `(ln(shadow_rti/strike) + 0.5*vol^2*t) / (vol*sqrt(t))`
- Spread penalty: 15% discount when spread > 10%
- Edge calculation: `|model_prob - market_price| - spread_penalty`

**Dependencies:** Need `statrs` crate for normal CDF, or implement Abramowitz–Stegun rational approximation (zero deps, ~10 lines).

**Trigger:** Recompute on every CryptoState update (price change), not on a timer.

---

## 1.3 Demote Python EvaluationDaemon to AdvisoryDaemon (Crypto)

### Problem
Python's 10s evaluation loop is now redundant for crypto. But we still want Python's signal as a cross-check during transition.

### Implementation

- Rename `CryptoSignalEvaluator` usage to advisory only
- Python crypto signals published to `tradebot.advisory.crypto` (not `tradebot.signals`)
- Rust logs comparison: `rust_edge=X, python_advisory_edge=Y, delta=Z`
- When delta > 5%: emit warning for investigation
- After 2 weeks of clean operation: remove Python crypto evaluator entirely

---

## 1.4 Remove Redis from Crypto Decision Path

### Problem
Redis adds ~1ms per read + serialization overhead. For crypto, decisions should use in-process CryptoState.

### Implementation

- Rust execution engine reads `CryptoState` directly (via `Arc<CryptoState>`)
- Remove Redis reads from crypto signal evaluation
- Keep Redis writes for: dashboard, Python advisory, debugging
- Redis becomes purely observational for crypto

---

## 1.5 DashMap Benchmark vs Actor Model

### Problem
Orderbook state uses DashMap (sharded concurrent hashmap). Need to validate this is the right choice vs a dedicated actor/channel model.

### Benchmark
- Measure: read latency, write latency, contention under load
- Compare: DashMap vs `tokio::sync::mpsc` actor with `oneshot` replies
- Decision criteria: If P99 read latency < 10μs, keep DashMap. Otherwise, migrate to actor.

---

## Verification Checklist

- [ ] CryptoState struct holds all feed data atomically
- [ ] Shadow RTI matches Python output for same inputs (within 0.01%)
- [ ] N(d2) matches Python `scipy.stats.norm.cdf` output
- [ ] Crypto signals generated in Rust within 1ms of price update
- [ ] Python advisory signals logged for comparison
- [ ] Redis no longer in critical path for crypto orders
- [ ] DashMap benchmark documented with decision
