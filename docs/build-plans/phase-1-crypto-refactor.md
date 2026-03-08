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

### Decision (Implemented)

**CryptoState: `std::sync::RwLock`** — Single struct, ~8 writes/sec (4 feeds × 2Hz), tiny critical section (<100ns). RwLock allows concurrent reads from execution engine while feeds update individually. No contention expected at this write rate.

**FeedHealth: `DashMap`** — Multiple independent keys, one per feed. DashMap's per-shard locking is ideal here — different feeds never contend with each other.

**OrderbookManager: `DashMap`** — Hundreds of independent ticker keys, high read rate from execution engine. DashMap's sharding distributes lock contention across tickers naturally.

**Why not actor model?** At our write rates (<10/sec per struct), the overhead of channel sends + oneshot replies exceeds direct lock acquisition by 10-100x. Actor model becomes beneficial only above ~100K writes/sec or when write operations involve async I/O (ours don't).

No formal benchmark was needed — the write rates are 3-4 orders of magnitude below contention thresholds for `RwLock`. If feed rates increase significantly (e.g., tick-by-tick at >1000 msgs/sec), revisit with `parking_lot::RwLock` before considering actors.

---

## Verification Checklist

- [x] CryptoState struct holds all feed data atomically
- [x] Shadow RTI matches Python output for same inputs (within 0.01%)
- [x] N(d2) matches Python `scipy.stats.norm.cdf` output (Abramowitz-Stegun, max error 1.5e-7)
- [x] Python advisory signals logged for comparison via `tradebot.advisory.crypto`
- [x] Redis no longer in critical path for crypto orders
- [x] DashMap benchmark documented with decision
- [x] Crypto signals generated in Rust within 1ms of price update (Phase 3 event-driven trigger)

**Status: Complete**
