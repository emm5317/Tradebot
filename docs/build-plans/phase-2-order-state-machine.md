# Phase 2 — Order State Machine

**Timeline:** Weeks 4–8
**Risk:** HIGH
**Goal:** Replace fire-and-forget order submission with a robust state machine that handles partial fills, cancellations, and restart recovery

**Dependencies from earlier phases:**
- Phase 0.2: Kill switch (`Arc<KillSwitchState>`) — state machine must cancel pending orders on kill
- Phase 0.4: Feed health (`Arc<FeedHealth>`) — use for stale-book prevention (2.6) instead of reimplementing
- Phase 1.1: `CryptoState` snapshot at order time — capture for post-trade attribution (feeds into 5.3)
- Phase 1.2: `CryptoFairValue` — log with order for model evaluation tracking

---

## 2.1 Order State Machine Enum

### Problem
Current `execution.rs` treats orders as fire-and-forget: submit, assume filled, move on. No tracking of order lifecycle, no handling of partial fills or rejections.

### Implementation

**New file:** `rust/src/order_state.rs`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderState {
    /// Signal received, pre-validation
    Pending,
    /// Risk checks passed, about to submit
    Submitting,
    /// Sent to exchange, waiting for ACK
    Acknowledged,
    /// Partially filled (qty > 0 but < requested)
    PartialFill,
    /// Completely filled
    Filled,
    /// Cancel requested
    CancelPending,
    /// Cancel confirmed by exchange
    Cancelled,
    /// Cancel+replace in flight
    Replacing,
    /// Exchange rejected the order
    Rejected,
    /// Unknown state (needs reconciliation)
    Unknown,
}
```

**Transition rules (enforced):**
- `Pending → Submitting` (risk check passed)
- `Submitting → Acknowledged` (exchange ACK received)
- `Acknowledged → PartialFill | Filled | Rejected`
- `PartialFill → Filled | CancelPending | Replacing`
- `CancelPending → Cancelled | Filled` (race condition: fill arrives before cancel ACK)
- `Replacing → Acknowledged | Rejected`
- Any state → `Unknown` (connection lost mid-operation)

Every transition logged with tracing at INFO level.

**Order struct:**
```rust
struct ManagedOrder {
    client_order_id: String,
    kalshi_order_id: Option<String>,
    ticker: String,
    signal_type: String,           // "crypto" or "weather"
    direction: String,
    requested_qty: i64,
    filled_qty: i64,
    state: OrderState,
    created_at: Instant,
    transitions: Vec<(OrderState, Instant)>,
    // Phase 1 integration: capture model state at order time for attribution
    crypto_snapshot: Option<CryptoStateInner>,   // snapshot when order created
    model_prob: f64,                              // model probability at order time
    market_price: f64,                            // market price at order time
}
```

> **Enhancement (from Phase 1):** Capturing `CryptoStateInner` snapshot at order creation enables post-trade attribution (Phase 5.3) without replaying state. The snapshot is cheap — it's a `Clone` of ~200 bytes.

---

## 2.2 Idempotency Keys and Client Order IDs

### Problem
Current idempotency key is `{ticker}-{direction}-{timestamp_ms}` which is not deterministic across retries.

### Implementation
- Client order ID format: `tb-{signal_hash}-{sequence}` where `signal_hash` is a deterministic hash of signal parameters
- Sequence number increments on retry (same signal, different attempt)
- Kalshi's `client_order_id` field used for idempotent submission
- DB `orders` table: add `client_order_id` column with unique constraint

---

## 2.3 Cancel/Replace Semantics

### Problem
No ability to cancel or modify live orders. If market moves after submission, order sits at stale price.

### Implementation
- `cancel_order(client_order_id)` → Kalshi DELETE endpoint
- `replace_order(client_order_id, new_price)` → cancel + resubmit (Kalshi doesn't support atomic modify)
- State transitions: `Acknowledged → CancelPending → Cancelled → Submitting`
- Timeout: if cancel not ACKed within 5s, mark Unknown and reconcile

---

## 2.4 Partial Fill Handling

### Problem
Current code assumes all-or-nothing fills. Partial fills leave phantom positions.

### Implementation
- Track `filled_qty` vs `requested_qty`
- On partial fill: update position tracker with actual filled amount
- Decision: hold partial position (don't auto-cancel remainder)
- Remainder auto-expires if market order (Kalshi market orders fill-or-kill)
- For future limit orders: implement cancel-remainder logic

---

## 2.5 Restart Recovery (Reconciliation)

### Problem
On restart, position tracker is empty. Open positions from previous session are invisible.

### Implementation

**Startup reconciliation gate:**
1. On startup, before processing any new signals, query Kalshi REST API for open orders
2. Query `orders` table for orders with `status IN ('pending', 'filled')` and `settled_at IS NULL`
3. Reconcile: mark orders that were filled on Kalshi but not in DB
4. Rebuild position tracker from reconciled state
5. Log discrepancies at WARN level
6. Block signal processing until reconciliation completes

**New migration:** `014_order_state_tracking.sql`
- Add `state` column to `orders` (enum matching OrderState)
- Add `filled_qty` column
- Add `transitions` JSONB column for audit trail

---

## 2.6 Execution Safeguards

### Problem
Various edge cases can cause bad order submission: stale orderbook, rate limiting, rapid-fire signals.

### Implementation

**Stale-book prevention (leverage Phase 0.4 FeedHealth):**
- Use `feed_health.required_feeds_healthy(&signal.signal_type)` (already exists) as first gate
- Additionally check orderbook `updated_at` timestamp before order submission
- If orderbook data >5s old, reject signal with reason
- Do NOT reimplement staleness — FeedHealth already tracks per-feed thresholds

**Rate-limit backoff:**
- Track Kalshi API rate limit headers (X-RateLimit-Remaining, X-RateLimit-Reset)
- When remaining < 10: exponential backoff
- When remaining = 0: pause all order submission until reset

**Duplicate signal suppression (signal-type-aware, aligns with Phase 3.5):**
- Per-ticker cooldown (already exists at 300s in Python)
- Add Rust-side dedup with signal-type-aware cooldowns:
  - Crypto: 30s per ticker (contracts settle every 60s)
  - Weather: 120s per ticker
- This prepares for Phase 3.5's full cooldown overhaul

**Max order frequency:**
- Global: max 10 orders per minute
- Per-ticker: max 2 orders per 5 minutes
- Configurable via env vars

---

## 2.7 Kill Switch Integration (Enhancement)

### Problem
Phase 0.2 kill switch blocks new signal processing, but has no effect on in-flight orders. When a kill switch activates, pending/acknowledged orders should be cancelled.

### Implementation
- On kill switch state change: iterate all ManagedOrders in `Acknowledged` or `PartialFill` state
- Transition each to `CancelPending`, send cancel to Kalshi
- Log all forced cancellations at WARN level
- This requires the state machine from 2.1 — cannot be done with current fire-and-forget model

---

## Verification Checklist

- [x] All 10 order states reachable and logged
- [x] Invalid state transitions panic in debug, warn in release
- [x] Client order IDs are deterministic for same signal
- [x] Startup reconciliation correctly rebuilds position tracker
- [x] Partial fills tracked accurately
- [x] Rate limit backoff prevents 429 errors
- [x] Stale orderbook detection uses FeedHealth (not reimplemented)
- [x] Kill switch activation cancels in-flight orders
- [x] Signal-type-aware cooldowns (crypto 30s, weather 120s)

**Status: Complete**
