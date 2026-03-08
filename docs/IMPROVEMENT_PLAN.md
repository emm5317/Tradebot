# Tradebot: Code Quality Fixes + Performance/Profit Improvements

## Context

The codebase review on branch `claude/codebase-improvements-analysis-F5QpG` identified 18 code quality issues (11 Rust, 7 Python) and several integration gaps. Additionally, Kalshi's API now supports batch orders and WebSocket fill channels, and is deprecating integer price/count fields (deadline March 12, 2026). The user wants to fix quality issues AND implement 10+ improvements targeting speed, functionality, and profit potential.

**User decisions:**
- Integrate batch orders + WebSocket fill channel
- Add uvloop + orjson + aws-lc-rs + DragonflyDB
- Update API types for fractional trading (lower priority)
- Keep custom Kalshi clients (no SDK swap)

---

## Phase 1: Code Quality Fixes (Rust)

### 1.1 Fix execution.rs safety issues
**Files:** `rust/src/execution.rs`

- **Add `get_position()` getter** to `PositionTracker` (line 47-80). Use it in `execute_exit()` (line 329) instead of direct `tracker.positions.get()`.
- **Fix i64→i32 casts** (lines 388, 391): Change `record_order()` to bind as `i64` directly. Update migration `003_orders.sql` to use `BIGINT` for `size_cents` and `latency_ms` columns if needed, or keep `INTEGER` and add `.try_into::<i32>().unwrap_or(i32::MAX)` with a warning log.
- **Fix P&L calculation** (line 355-357): In `execute_exit()`, capture fill price from the order response (like `execute_entry` does) and use it for P&L instead of `signal.market_price`.
- **Fix misleading comment** (line 328): Change "Sell the opposite side" to "Sell the held side to close position".

### 1.2 Fix kalshi/client.rs header panics + error parsing
**Files:** `rust/src/kalshi/client.rs`

- **Replace `.unwrap()` with `?`** on `HeaderValue::from_str()` (lines 38, 42, 46). Return `KalshiError::SigningError` on failure.
- **Parse JSON error responses** (lines 263-270): Parse response body as JSON, look for structured error codes/messages instead of string matching. Fall back to string matching if JSON parse fails.
- **Parse Retry-After** (lines 257-260): Extract `retry_after_ms` from response body JSON (Kalshi returns it in the body, not the HTTP header). Use it instead of hardcoded 1s.

### 1.3 Fix config.rs validation + credential redaction
**Files:** `rust/src/config.rs`

- **Add `validate()` method** called after `from_env()`. Assert: `max_trade_size_cents > 0`, `max_daily_loss_cents > 0`, `max_positions > 0`, `max_exposure_cents > 0`, `kelly_fraction_multiplier` in `(0.0, 1.0]`.
- **Redact URLs in Debug impl** (lines 66-67): Apply same `[REDACTED]` treatment to `redis_url` and `nats_url` as done for `database_url`.

### 1.4 Fix orderbook_feed.rs conversions + Redis failures
**Files:** `rust/src/orderbook_feed.rs`

- **Use `ToPrimitive::to_f64()`** (lines 86-89): Replace `d.to_string().parse::<f64>()` with `d.to_f64().unwrap_or(0.0)`. Add `use rust_decimal::prelude::ToPrimitive;`.
- **Add Redis failure counter** (lines 115-117): Track consecutive Redis failures. After 10 consecutive failures, log at `error!` level instead of `warn!`. Reset counter on success.

---

## Phase 2: Code Quality Fixes (Python)

### 2.1 Fix memory leaks in evaluators
**Files:** `python/signals/weather.py`, `python/signals/crypto.py`

- **Add cleanup to `_recent_signals`**: After each evaluation cycle, remove entries older than 600s. Add a `_cleanup_cooldowns()` method called at start of `evaluate()`:
  ```python
  def _cleanup_cooldowns(self):
      cutoff = datetime.now(timezone.utc) - timedelta(seconds=600)
      self._recent_signals = {k: v for k, v in self._recent_signals.items() if v > cutoff}
  ```

### 2.2 Fix publisher fire-and-forget tasks
**Files:** `python/signals/publisher.py`

- **Track background tasks** in a `set()`. Use `task.add_done_callback()` to log errors at `error` level (not just exception). Add a retry for DB persistence failures (1 retry with 1s delay).

### 2.3 Fix evaluator daemon issues
**Files:** `python/evaluator/daemon.py`

- **Refresh blackout windows** every 5 minutes (add a `_last_blackout_refresh` timestamp, re-query DB if stale).
- **Remove redundant `import json`** at line 267 (already imported at module level).
- **Use Redis MGET** for bulk orderbook fetch instead of serial `GET` per ticker. Log exceptions instead of silent `pass`.

### 2.4 Fix binance_ws.py EWMA initialization
**Files:** `python/data/binance_ws.py`

- **Initialize EWMA from first return** instead of waiting for 10 bars. If `_ewma_variance == 0.0` and a new return is available, set `_ewma_variance = log_return ** 2` (single-return bootstrap).

### 2.5 Fix notifier retry logic
**Files:** `python/signals/notifier.py`

- **Add exponential backoff**: On 429, retry up to 3 times with backoff (5s, 10s, 20s from Retry-After header).
- **Dead letter logging**: If all retries fail, log the lost notification at `error` level with full payload for manual recovery.

---

## Phase 3: Performance Improvements (Speed)

### 3.1 Add uvloop + orjson to Python stack
**Files:** `python/pyproject.toml`, `python/evaluator/daemon.py`, `python/collector/daemon.py`, `python/dashboard/app.py`

- **Add dependencies** to pyproject.toml: `"uvloop>=0.21"`, `"orjson>=3.10"`
- **Set uvloop policy** at the top of each entry point (evaluator, collector, dashboard):
  ```python
  import uvloop
  uvloop.install()
  ```
- **Use orjson** for NATS signal serialization in `publisher.py` (`orjson.dumps()` instead of `json.dumps()`).
- **Use ORJSONResponse** in dashboard `app.py` for API endpoints.

### 3.2 Switch Rust crypto from openssl to aws-lc-rs
**Files:** `rust/Cargo.toml`, `rust/src/kalshi/auth.rs`

- **Replace openssl** dependency with `aws-lc-rs` in Cargo.toml.
- **Update auth.rs** to use aws-lc-rs API for RSA-PSS SHA256 signing. The signing flow is similar: load PEM → sign payload → base64 encode.
- 20-30% faster RSA operations, pure Rust (memory safe).

### 3.3 Replace Redis with DragonflyDB
**Files:** `docker/docker-compose.yml`

- **Swap Redis image** from `redis:7-alpine` to `docker.dragonflydb.io/dragonflydb/dragonfly:latest`.
- **Update healthcheck** command (DragonflyDB supports `redis-cli ping`).
- **No code changes needed** — DragonflyDB is wire-compatible with Redis. Both `fred` (Rust) and `redis[hiredis]` (Python) work unchanged.
- 70-120% throughput improvement, 30-50% less memory.

---

## Phase 4: Functionality Improvements (Profit)

### 4.1 WebSocket fill channel for real-time P&L
**Files:** `rust/src/kalshi/websocket.rs`, `rust/src/execution.rs`, `rust/src/main.rs`

- **Subscribe to "fill" channel** on the authenticated WebSocket connection. Parse fill messages: `trade_id`, `order_id`, `market_ticker`, `side`, `yes_price`, `count`, `action`.
- **Update PositionTracker** with actual fill prices instead of estimated prices.
- **Compute real-time P&L** from actual fills, updating `DailyPnl` with true values.
- **Wire into main.rs**: spawn a fill listener task alongside the orderbook feed task.

### 4.2 Position persistence across restarts
**Files:** `rust/src/execution.rs`

- **On startup**, query Kalshi `GET /portfolio/positions` for non-zero positions. Populate `PositionTracker` from API response.
- **Also query `orders` table** for recent fills to get entry prices.
- Add a `load_positions()` async function called before entering the signal loop.

### 4.3 Settlement listener + actual P&L
**Files:** `rust/src/execution.rs` (or new `rust/src/settlement.rs`)

- **Poll `GET /portfolio/settlements`** every 60s (or use fill WS channel settlement events).
- **On settlement**: look up position in tracker, compute actual P&L (revenue - cost), record to `daily_summary` table, remove from tracker.
- **Trigger Discord notification** for settlement results.

### 4.4 Batch order support
**Files:** `rust/src/kalshi/client.rs`, `rust/src/kalshi/types.rs`, `rust/src/execution.rs`

- **Add `BatchOrderRequest` type** in types.rs (array of OrderRequest, max 20).
- **Add `place_batch_orders()` method** in client.rs → `POST /portfolio/orders/batched`.
- **Buffer signals** in execution.rs: collect up to 20 entry signals within a 500ms window, then submit as a single batch. Fall back to single orders if only 1 signal.

### 4.5 Portfolio balance integration
**Files:** `rust/src/execution.rs`, `rust/src/kalshi/client.rs`

- **Query balance** on startup and cache it. Refresh after each order.
- **Use actual balance** in risk checks: replace `max_exposure_cents` hard limit with `min(max_exposure_cents, actual_balance * 0.8)`.
- **Add `get_balance()` method** to KalshiClient → `GET /portfolio/balance`.

---

## Phase 5: Profit Optimization

### 5.1 Limit orders with price improvement
**Files:** `rust/src/execution.rs`, `rust/src/kalshi/types.rs`

- **Switch from market orders to limit orders** with aggressive pricing:
  - For YES buys: set price at `best_ask` (immediate fill at best available)
  - For YES sells: set price at `best_bid`
  - This avoids market order fees while still getting immediate execution
- **Add IOC (Immediate-or-Cancel) expiration** to prevent stale resting orders.
- **Benefit**: Resting orders (those that make it to the book) are exempt from trading fees on Kalshi.

### 5.2 Dynamic Kelly scaling
**Files:** `python/signals/utils.py`, `python/evaluator/daemon.py`

- **Track recent signal accuracy** in evaluator daemon: maintain a rolling window of last 50 signals with outcomes (from settlements).
- **Scale Kelly multiplier**:
  - Base: `config.kelly_fraction_multiplier` (0.25)
  - If accuracy > 60% over last 50: multiply by 1.5 (up to 0.375)
  - If accuracy < 45%: multiply by 0.5 (down to 0.125)
  - Clamp to [0.1, 0.5] range
- **Publish scaling factor** in model state for dashboard visibility.

### 5.3 Fast-path evaluation for near-expiry contracts
**Files:** `python/evaluator/daemon.py`

- **Add a fast-path loop** (2s cycle) for contracts settling within 5 minutes.
- **Rationale**: Near-expiry contracts have rapidly changing edge. A 10s evaluation cycle misses profitable entry/exit windows.
- **Implementation**: Run two concurrent loops — standard (10s, contracts 5-30 min out) and fast (2s, contracts <5 min).

### 5.4 Signal confidence weighting
**Files:** `python/signals/utils.py`, `python/signals/weather.py`, `python/signals/crypto.py`

- **Add confidence score** to signal evaluation: `confidence = (1 - spread/0.20) * time_decay_factor`.
  - `time_decay_factor` = 1.0 at 30 min, 0.5 at 2 min (less confident as time shrinks)
  - Tight spread = high confidence, wide spread = low confidence
- **Scale position size** by confidence: `adjusted_kelly = kelly * confidence`.
- **Include confidence** in SignalSchema for dashboard display and audit trail.

---

## Phase 6 (Lower Priority): Kalshi API Type Migration

### 6.1 Update to fixed-point string prices
**Files:** `rust/src/kalshi/types.rs`, `python/signals/types.py`

- **Rust**: Change `yes_price: Option<i64>` to `Option<String>` (fixed-point dollar strings like "0.55"). Add helper to convert to/from `rust_decimal::Decimal`.
- **Python**: Update Pydantic schemas to accept both old integer and new string formats during transition.
- **Update all callers** that compute with prices to parse the new format.

---

## Implementation Order

```
Phase 1 (Rust quality)   ✅ COMPLETE — commit d230276
Phase 2 (Python quality) ✅ COMPLETE — commit d230276
Phase 3 (Performance)    ✅ COMPLETE — commit eaed142
Phase 4 (Functionality)  ✅ COMPLETE — commit ea84158
Phase 5 (Profit)         ✅ COMPLETE — commit ea84158
Phase 6 (API migration)  ⏳ PENDING  — lower priority, deadline March 12, 2026
```

Phases 1-5 are complete. Phase 6 (Kalshi API type migration to fixed-point string prices) remains as a lower-priority item before the March 12, 2026 deprecation deadline.

**Note:** The aws-lc-rs crypto swap (originally Phase 3) was deferred — aws-lc-rs expects PKCS#8 DER format rather than PEM, requiring a more significant rewrite of auth.rs.

---

## Verification Plan

1. **Rust compilation**: `cd rust && cargo build` — must compile cleanly after each phase
2. **Rust tests**: `cd rust && cargo test` — existing orderbook tests must pass
3. **Rust clippy**: `cd rust && cargo clippy -- -D warnings` — no new warnings
4. **Python tests**: `cd python && python -m pytest tests/ -v` — all 11 test files pass
5. **Docker compose**: `docker compose -f docker/docker-compose.yml up -d` — DragonflyDB starts and passes healthcheck
6. **Integration smoke test**: Start collector → evaluator → Rust dev in paper mode, verify signals flow through NATS → execution engine logs orders
7. **Dashboard**: `just dashboard` — verify SSE endpoints return data with orjson serialization

---

## Files Modified (Summary)

| Phase | Files |
|-------|-------|
| 1 | `rust/src/execution.rs`, `rust/src/kalshi/client.rs`, `rust/src/config.rs`, `rust/src/orderbook_feed.rs` |
| 2 | `python/signals/weather.py`, `python/signals/crypto.py`, `python/signals/publisher.py`, `python/evaluator/daemon.py`, `python/data/binance_ws.py`, `python/signals/notifier.py` |
| 3 | `python/pyproject.toml`, `python/evaluator/daemon.py`, `python/collector/daemon.py`, `python/dashboard/app.py`, `python/signals/publisher.py`, `rust/Cargo.toml`, `rust/src/kalshi/auth.rs`, `docker/docker-compose.yml` |
| 4 | `rust/src/kalshi/websocket.rs`, `rust/src/kalshi/client.rs`, `rust/src/kalshi/types.rs`, `rust/src/execution.rs`, `rust/src/main.rs` |
| 5 | `python/signals/utils.py`, `python/evaluator/daemon.py`, `python/signals/weather.py`, `python/signals/crypto.py`, `rust/src/execution.rs`, `rust/src/kalshi/types.rs` |
| 6 | `rust/src/kalshi/types.rs`, `python/signals/types.py` |
