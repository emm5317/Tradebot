> **Status: COMPLETE** — All 10 improvements in this plan have been implemented across Phases 0-5. This document is preserved as historical reference. See `docs/build-plans/` for phase-by-phase implementation details.

# Codebase Improvements Plan

10 improvements across functionality, speed, and UX. Ordered by implementation dependency.

---

## Phase 1: Foundations (unblocks everything else)

### 1. Extract Shared Trading Utilities

**Files**: `python/signals/utils.py` (new)

`_estimate_fill_price()` and `_compute_kelly()` are copy-pasted identically in `weather.py` and `crypto.py`. The spread-adjusted edge calculation is also duplicated.

**Changes**:
- Create `signals/utils.py` with `estimate_fill_price()`, `compute_kelly()`, `compute_effective_edge()`
- Update `weather.py` and `crypto.py` to import from `utils.py`
- Move the shared edge → direction → spread → kelly → emit flow into a helper
- Keep evaluator-specific logic (model computation) in each evaluator

**Tests**: Existing tests continue to pass; add unit tests for `utils.py`.

---

### 2. Concurrent Market Snapshot Fetching

**Files**: `python/collector/daemon.py`

The `_collect_market_snapshots()` method fetches each contract serially. With 30+ contracts, this takes 30+ seconds per cycle.

**Changes**:
- Replace the serial `for ticker in tickers` loop with `asyncio.gather()` + `Semaphore(8)`
- Each fetch is wrapped in a try/except so one failure doesn't abort the batch
- Rate limit compliance: 8 concurrent * 0.67s pacing ≈ 12 req/s (within 100 req/min with margin)

**Before**:
```python
for ticker in tickers:
    resp = await client.get(f".../{ticker}")
```

**After**:
```python
sem = asyncio.Semaphore(8)
async def _fetch_one(ticker):
    async with sem:
        resp = await client.get(f".../{ticker}")
        ...
results = await asyncio.gather(*[_fetch_one(t) for t in tickers], return_exceptions=True)
```

**Impact**: 5-10x faster snapshot cycle. Fresher data = better signal quality.

---

### 3. Evaluator Plugin Registry

**Files**: `python/signals/registry.py` (new)

Adding new market types (sports, politics) currently requires duplicating boilerplate. A simple registry auto-discovers evaluators.

**Design**:
```python
class EvaluatorRegistry:
    _evaluators: dict[str, BaseEvaluator] = {}

    def register(self, signal_type: str, evaluator: BaseEvaluator): ...
    def get(self, signal_type: str) -> BaseEvaluator | None: ...
    def all(self) -> dict[str, BaseEvaluator]: ...

class BaseEvaluator(Protocol):
    def evaluate(self, contract, data, orderbook) -> tuple[Signal | None, Rejection | None, ModelState]: ...
    def evaluate_exit(self, ...) -> Signal | None: ...
```

Weather and crypto evaluators register at startup. New markets just implement the protocol and register.

---

## Phase 2: Core Pipeline

### 4. Signal Orchestration Loop (EvaluationDaemon)

**Files**: `python/evaluator/daemon.py` (new)

Currently nothing connects the collector to the evaluators. This daemon runs every ~10s:

**Flow**:
1. Query contracts settling within 30 min (from DB)
2. For each contract, fetch latest observation + orderbook state
3. Look up the registered evaluator by signal_type
4. Call `evaluator.evaluate(contract, data, orderbook)`
5. If signal: publish via `SignalPublisher` (NATS + DB + Redis)
6. If rejection: publish rejection for UI visibility
7. Always publish model state to Redis

**Dependencies**: Uses the registry (#3), shared utils (#1), and benefits from concurrent snapshots (#2).

**Config additions to `Settings`**:
- `evaluation_interval_seconds: int = 10`
- `nats_url: str`
- `redis_url: str`

---

### 5. Rust NATS Consumer + Order Execution

**Files**: `rust/src/main.rs`, `rust/src/execution.rs` (new)

The Rust binary connects to all services but does nothing. This adds:

**Changes**:
- Subscribe to `tradebot.signals` NATS subject
- Deserialize incoming `SignalSchema` JSON
- Position manager: check if already holding a position on this ticker
- Risk checks: max trade size, daily loss limit, max positions, max exposure
- Paper mode: log the would-be order without hitting Kalshi API
- Live mode: `KalshiClient.place_order()` with idempotency key
- Record order to `orders` table with latency_ms
- On fill/settlement: update order status, compute PnL

**Safety**:
- `PAPER_MODE=true` by default — logs orders without executing
- Idempotency keys prevent duplicate orders on NATS redelivery
- Daily loss circuit breaker stops trading when `MAX_DAILY_LOSS_CENTS` exceeded

---

### 6. EWMA Volatility Estimation

**Files**: `python/data/binance_ws.py`

Current volatility uses equal-weighted 30 1-min returns. EWMA weights recent returns more heavily.

**Changes**:
- Add `_recompute_vol_ewma()` alongside existing `_recompute_vol()`
- EWMA formula: `σ²_t = λ * σ²_{t-1} + (1-λ) * r²_t` with λ=0.94 (RiskMetrics standard)
- Expose both `realized_vol_30m` (simple) and `ewma_vol_30m` on `CryptoState`
- Crypto evaluator uses `ewma_vol_30m` when available, falls back to simple

**Impact**: More responsive to regime changes (BTC going from quiet to volatile). Better Black-Scholes pricing during transitions.

---

### 7. Orderbook Depth in Market Snapshots

**Files**: `python/collector/daemon.py`, `python/signals/types.py`

Currently `best_bid`, `best_ask`, `bid_depth`, `ask_depth` are all `None`. The Rust orderbook manager already computes these from the WebSocket feed.

**Approach**: Bridge Rust orderbook data to Python via Redis.

**Changes**:
- Rust: Write orderbook summaries to Redis keys `orderbook:{ticker}` (JSON with best_bid, best_ask, bid_depth, ask_depth, mid_price, spread) every time the book updates
- Python EvaluationDaemon: Read `orderbook:{ticker}` from Redis instead of relying on REST snapshot
- Fall back to REST snapshot data if Redis key is missing/stale

**Impact**: Accurate fill price estimates, real spread data, depth-aware Kelly sizing.

---

## Phase 3: UX & Observability

### 8. Web Dashboard (Terminal-Style)

**Files**: `python/dashboard/` (new directory)

FastAPI app with a retro terminal aesthetic (dark background, green/amber text, monospace fonts).

**Stack**: FastAPI + Jinja2 templates + htmx for interactivity + SSE for live updates. No JS framework.

**Pages**:
- **Live Signals**: Real-time signal stream from NATS `tradebot.signals.live` via SSE
- **Model State**: Per-contract model internals (probabilities, edge, direction) from Redis
- **Positions**: Open positions, P&L, entry price, current edge
- **History**: Signal history with filters (date range, signal_type, acted_on)
- **System Health**: Connection status (DB, Redis, NATS, Binance WS), collector cycle times
- **Backtester Results**: Calibration charts, simulated P&L curves

**Design**: Terminal/hacker aesthetic — CSS-only, no images. Monospace font, dark theme, colored text for signal direction (green=YES, red=NO), blinking cursor on active sections.

**Dependencies**: `fastapi`, `uvicorn`, `jinja2`, `sse-starlette`, `redis[hiredis]`

---

### 9. Discord Webhook Notifications

**Files**: `python/signals/notifier.py` (new)

Lightweight notification system that hooks into `SignalPublisher`.

**Events**:
- Signal entry (ticker, direction, edge, kelly)
- Signal exit (ticker, held direction, P&L)
- Errors exceeding threshold (3+ in 5 minutes)
- Daily P&L summary (end of trading day)

**Design**:
```python
class DiscordNotifier:
    def __init__(self, webhook_url: str | None): ...
    async def notify_signal(self, signal: SignalSchema): ...
    async def notify_error(self, error: str, context: dict): ...
    async def notify_daily_summary(self, summary: dict): ...
```

**Rate limiting**: Max 1 message per 5 seconds to avoid Discord throttling. Queue messages and batch if needed.

---

## Phase 4: Validation

### 10. Backtesting Framework

**Files**: `python/backtester/engine.py` (new)

Replays historical data through evaluators to validate model accuracy before going live.

**Design**:
```python
class Backtester:
    def __init__(self, pool, registry: EvaluatorRegistry): ...

    async def run(
        self,
        start: datetime,
        end: datetime,
        signal_types: list[str] | None = None,
    ) -> BacktestResult: ...
```

**Flow**:
1. Query historical contracts with known settlements from `contracts` table
2. For each contract, query time-aligned observations and market snapshots
3. Simulate time progression: feed data to evaluators at each snapshot timestamp
4. Record signals that would have fired
5. Compare model_prob against actual_outcome (settled_yes field)
6. Compute metrics

**Metrics** (`BacktestResult`):
- **Accuracy**: % of signals where direction matched settlement
- **Brier Score**: Mean squared error of probability predictions
- **Calibration**: Binned predicted prob vs actual win rate (reliability diagram)
- **Simulated P&L**: Net P&L assuming Kelly-sized orders at estimated fill prices
- **Edge Decay**: How edge changes as time-to-settlement decreases
- **Signal Count**: Breakdown by type, direction, rejection reason

**Output**: JSON results + optional CSV export. Dashboard (#8) renders calibration charts.

---

## Implementation Order

```
Phase 1 (foundations):
  #1 Extract shared utils ──────────┐
  #2 Concurrent snapshots ──────────┤
  #3 Evaluator registry ────────────┘
                                     │
Phase 2 (core pipeline):             ▼
  #4 Signal orchestration loop ──── depends on #1, #2, #3
  #5 Rust NATS consumer ─────────── independent (parallel with #4)
  #6 EWMA volatility ────────────── independent (parallel with #4)
  #7 Orderbook depth ────────────── depends on #5 (Rust writes to Redis)
                                     │
Phase 3 (UX):                        ▼
  #8 Web dashboard ──────────────── depends on #4 (needs live data)
  #9 Discord notifications ──────── depends on #4 (hooks into publisher)
                                     │
Phase 4 (validation):                ▼
  #10 Backtesting framework ─────── depends on #3 (uses registry)
```

## New Dependencies

**Python** (add to `pyproject.toml`):
```toml
dependencies = [
    # ... existing ...
    "redis[hiredis]>=5",       # Redis client for dashboard + orderbook bridge
    "fastapi>=0.115",          # Dashboard web framework
    "uvicorn[standard]>=0.32", # ASGI server
    "jinja2>=3.1",             # HTML templates
    "sse-starlette>=2",        # Server-Sent Events
]
```

**Rust**: No new dependencies needed — all required crates already in Cargo.toml.

## New Justfile Commands

```just
# Evaluation loop
evaluator:
    cd python && python -m evaluator.daemon

# Dashboard
dashboard:
    cd python && python -m dashboard.app

# Backtesting
backtest start end:
    cd python && python -m backtester.engine --start {{start}} --end {{end}}
```
