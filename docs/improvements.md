# Codebase Improvements Plan

10 improvements across functionality, speed, and UX. Ordered by implementation dependency.

> **Status**: All 10 phases implemented as of commit `0ccaeb9`. See [BUILD_STATUS.md](BUILD_STATUS.md) for detailed review.

---

## Phase 1: Foundations (unblocks everything else)

### 1. Extract Shared Trading Utilities — COMPLETE

**Files**: `python/signals/utils.py`

Extracted `estimate_fill_price()`, `compute_kelly()`, `compute_effective_edge()` from duplicated code in `weather.py` and `crypto.py`. Both evaluators now import from the shared module.

**Tests**: `python/tests/test_utils.py`

---

### 2. Concurrent Market Snapshot Fetching — COMPLETE

**Files**: `python/collector/daemon.py`

Replaced serial contract fetching with `asyncio.gather()` + `Semaphore(8)`. Each fetch is wrapped in try/except so one failure doesn't abort the batch.

**Impact**: 5-10x faster snapshot cycle. Fresher data for signal evaluation.

---

### 3. Evaluator Plugin Registry — COMPLETE

**Files**: `python/signals/registry.py`

```python
class EvaluatorRegistry:
    _evaluators: dict[str, BaseEvaluator] = {}

    def register(self, signal_type: str, evaluator: BaseEvaluator): ...
    def get(self, signal_type: str) -> BaseEvaluator | None: ...
    def all(self) -> dict[str, BaseEvaluator]: ...
```

Weather and crypto evaluators register at startup. New markets implement the protocol and register.

**Tests**: `python/tests/test_registry.py`

---

## Phase 2: Core Pipeline

### 4. Signal Orchestration Loop (EvaluationDaemon) — COMPLETE

**Files**: `python/evaluator/daemon.py`

10-second evaluation cycle:
1. Query contracts settling within 30 min
2. Fetch latest observations + BTC state
3. Override DB snapshots with Redis orderbook data (from Rust WS feed)
4. Call registered evaluator for each contract
5. Publish signals via NATS + DB + Redis

---

### 5. Rust NATS Consumer + Order Execution — COMPLETE

**Files**: `rust/src/execution.rs`, `rust/src/main.rs`

- Subscribes to `tradebot.signals` NATS subject
- Deserializes `SignalSchema` JSON from Python evaluator
- Position manager prevents double-entry
- Risk checks: max trade size, daily loss, max positions, max exposure
- Paper mode logs orders without hitting Kalshi API
- Live mode places orders with RSA-PSS signed requests + idempotency keys
- Records orders to `orders` table with latency_ms

---

### 6. EWMA Volatility Estimation — COMPLETE

**Files**: `python/data/binance_ws.py`

Added `_recompute_vol_ewma()` alongside simple volatility. EWMA formula with λ=0.94 (RiskMetrics standard). Crypto evaluator uses EWMA vol when available, falls back to simple.

---

### 7. Orderbook Depth via Redis Bridge — COMPLETE

**Files**: `rust/src/orderbook_feed.rs`, `python/evaluator/daemon.py`

Rust WebSocket feed writes orderbook summaries to Redis keys `orderbook:{ticker}` every 500ms. Python evaluator reads Redis for real-time data, falls back to DB snapshots if stale.

---

## Phase 3: UX & Observability

### 8. Web Dashboard (Terminal-Style) — COMPLETE

**Files**: `python/dashboard/`

FastAPI + Jinja2 + htmx + SSE. Retro terminal aesthetic (dark background, green/amber text, monospace).

**Pages**: Live signals, model state, positions, signal history, system health.

**Run**: `just dashboard` → `:8050`

---

### 9. Discord Webhook Notifications — COMPLETE

**Files**: `python/signals/notifier.py`

`DiscordNotifier` class with signal alerts, error reporting, and daily summaries. Rate-limited to avoid Discord throttling. Wired into the evaluator daemon.

---

## Phase 4: Validation

### 10. Backtesting Framework — COMPLETE

**Files**: `python/backtester/engine.py`

Historical replay engine that queries settled contracts, simulates time progression through evaluators, and computes:
- Accuracy (% correct direction)
- Brier score (probability calibration)
- Calibration curve (binned predicted vs actual)
- Simulated P&L (Kelly-sized at estimated fills)
- Signal count breakdown

**Run**: `just backtest 2024-01-01 2024-06-30`

**Tests**: Coverage across all evaluators and the backtester itself.

---

## Implementation Order

```
Phase 1 (foundations):        ✅ ALL COMPLETE
  #1 Extract shared utils
  #2 Concurrent snapshots
  #3 Evaluator registry
                               │
Phase 2 (core pipeline):      ✅ ALL COMPLETE
  #4 Signal orchestration loop
  #5 Rust NATS consumer
  #6 EWMA volatility
  #7 Orderbook depth
                               │
Phase 3 (UX):                 ✅ ALL COMPLETE
  #8 Web dashboard
  #9 Discord notifications
                               │
Phase 4 (validation):         ✅ ALL COMPLETE
  #10 Backtesting framework
```
