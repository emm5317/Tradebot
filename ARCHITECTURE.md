# ARCHITECTURE.md
System architecture reference for tradebot.
---
## System Overview
```
                    ┌─────────────────────────────────────────────┐
                    │              OBSERVATION LAYER               │
                    │                  (Python)                    │
                    │                                             │
                    │  ┌─────────────┐    ┌──────────────────┐   │
                    │  │ Iowa Mesonet│    │ Binance WebSocket│   │
                    │  │ ASOS 1-min  │    │ BTC tick stream  │   │
                    │  └──────┬──────┘    └────────┬─────────┘   │
                    │         ▼                     ▼             │
                    │  ┌─────────────┐    ┌──────────────────┐   │
                    │  │Physics model│    │ Black-Scholes    │   │
                    │  │ N(Δ/σ√t)   │    │ N(d2) binary     │   │
                    │  └──────┬──────┘    └────────┬─────────┘   │
                    │         ▼                     ▼             │
                    │  ┌────────────────────────────────────┐    │
                    │  │  Settlement Scanner (every 30s)    │    │
                    │  │  Filters: 8-18 min to settlement   │    │
                    │  │  Edge threshold: weather >5%,      │    │
                    │  │                  crypto >6 cents    │    │
                    │  └──────────────────┬─────────────────┘    │
                    └─────────────────────┼───────────────────────┘
                                          │ Signal JSON
                                          ▼
                                   ┌──────────────┐
                                   │ Redis Streams │
                                   │  < 1ms local  │
                                   └──────┬───────┘
                                          │
                    ┌─────────────────────┼───────────────────────┐
                    │              EXECUTION ENGINE                │
                    │                  (Rust)                      │
                    │                                             │
                    │         ┌────────────────────┐              │
                    │         │  Signal Consumer   │              │
                    │         │  (crossbeam chan)   │              │
                    │         └─────────┬──────────┘              │
                    │                   ▼                          │
                    │         ┌────────────────────┐              │
                    │         │   Risk Manager     │              │
                    │         │  AtomicI64 checks  │              │
                    │         │  Kill switch       │              │
                    │         │  Circuit breaker   │              │
                    │         │  Time gate         │              │
                    │         └─────────┬──────────┘              │
                    │                   ▼                          │
                    │         ┌────────────────────┐              │
                    │         │  Kelly Sizing      │              │
                    │         │  Quarter-Kelly     │              │
                    │         │  $25 hard cap      │              │
                    │         └─────────┬──────────┘              │
                    │                   ▼                          │
                    │         ┌────────────────────┐              │
                    │         │  Kalshi Client     │              │
                    │         │  HTTP/2 persistent │              │
                    │         │  RSA-SHA256 auth   │              │
                    │         └─────────┬──────────┘              │
                    │                   │                          │
                    │         ┌─────────┴──────────┐              │
                    │         ▼                     ▼              │
                    │  ┌────────────┐     ┌──────────────┐       │
                    │  │ PostgreSQL │     │  Axum UI     │       │
                    │  │ orders,    │     │  terminal.html│       │
                    │  │ signals,   │     │  kill switch  │       │
                    │  │ contracts  │     │  status API   │       │
                    │  └────────────┘     └──────────────┘       │
                    └─────────────────────────────────────────────┘
```
## Critical Path: Signal to Order
This is the latency-sensitive path. Target: <50ms end-to-end.
```
Signal lands in Redis Stream
  │  Redis XREAD (~0.1ms)
  ▼
Deserialize signal (simd-json)
  │  (~0.05ms)
  ▼
Kill switch check (AtomicBool::load)
  │  (~0.001ms)
  ▼
Time gate check (minutes_to_settlement)
  │  (~0.001ms)
  ▼
Risk manager checks:
  ├─ Daily loss limit (AtomicI64::load)
  ├─ Open position count (DashMap::len)
  ├─ Total exposure (AtomicI64::load)
  └─ Circuit breaker state
  │  (~0.01ms total — all lock-free)
  ▼
Kelly position sizing
  │  (~0.01ms)
  ▼
HTTP POST to Kalshi (pre-authenticated, persistent conn)
  │  (~30-40ms — network round trip, this dominates)
  ▼
Order ACK received
  │  Parse response (simd-json)
  ▼
Update state:
  ├─ DashMap: insert position
  ├─ AtomicI64: update exposure
  └─ PostgreSQL: persist order (async, not on critical path)
```
Network round trip to Kalshi is the bottleneck. Everything else is optimized to be negligible.
## Data Flow
### Inbound Data
| Source | Protocol | Frequency | Consumer |
|--------|----------|-----------|----------|
| Iowa State Mesonet | HTTPS poll | Every 60s | `python/data/mesonet.py` |
| Aviation Weather (METAR) | HTTPS poll | Every 60s (fallback) | `python/data/asos.py` |
| Binance | WebSocket | Tick-by-tick | `python/data/binance_ws.py` |
| Kalshi markets | WebSocket | Real-time orderbook | `rust/src/kalshi/websocket.rs` |
| Kalshi REST | HTTPS | On-demand (market list, history) | `rust/src/kalshi/client.rs` |
### Internal Data Flow
| From | To | Via | Format |
|------|----|-----|--------|
| Python signals | Rust execution | Redis Streams | JSON (see CLAUDE.md for schema) |
| Rust execution | PostgreSQL | sqlx | SQL (compile-time checked) |
| Rust state | UI | Axum JSON API | HTTP |
| PostgreSQL | Python backtest | sqlx / psycopg | SQL |
## Database Schema
Four core tables. See `migrations/` for exact DDL.
- **contracts** — Market metadata: ticker, category, threshold, station, settlement time, outcome
- **signals** — Every signal generated, whether acted on or not (audit trail)
- **orders** — Every order placed, with Kalshi order ID, fill price, PnL
- **daily_summary** — Aggregated daily stats for performance tracking
Key design decisions:
- All money is integer cents (`size_cents`, `pnl_cents`, `gross_pnl_cents`)
- All timestamps are `TIMESTAMPTZ` (UTC)
- `signals.acted_on` links signals to orders for edge analysis
- `orders.outcome` is set post-settlement for PnL calculation
## Concurrency Model
The Rust execution engine runs on tokio with the following task structure:
```
main()
  ├─ spawn: kalshi_websocket_listener    (updates market prices in DashMap)
  ├─ spawn: redis_signal_consumer        (reads signals, sends to channel)
  ├─ spawn: order_processor              (receives from channel, executes trades)
  ├─ spawn: settlement_watcher           (resolves positions post-settlement)
  ├─ spawn: token_refresher              (keeps Kalshi auth token warm)
  └─ axum::serve(ui_router)             (blocks on main, serves UI)
```
Shared state lives in `AppState`:
- `DashMap<String, Position>` — open positions (lock-free reads/writes)
- `AtomicI64` — daily_loss, open_exposure (lock-free)
- `AtomicBool` — kill_switch (lock-free)
- `sqlx::PgPool` — database connection pool (not on hot path)
- `reqwest::Client` — HTTP/2 persistent connection to Kalshi
## Startup Sequence
Order matters. The system pre-warms everything before accepting signals:
1. Load config from `.env`
2. Connect to PostgreSQL, verify schema version
3. Connect to Redis, verify connectivity
4. Authenticate with Kalshi, cache bearer token
5. Open persistent HTTP/2 connection to Kalshi
6. Fetch all active markets, cache in memory
7. Subscribe to Kalshi WebSocket for active markets
8. Initialize position state from database (recover from crash)
9. Reset daily counters if new trading day
10. Begin consuming Redis signals
If any step 1–8 fails, the system does not start. No partial initialization.
## Failure Modes and Recovery
| Failure | Detection | Response |
|---------|-----------|----------|
| Kalshi WS disconnect | tokio timeout | Reconnect with backoff; pause signals until reconnected |
| Redis down | Connection error on XREAD | Log error, retry; orders in flight complete but no new signals |
| Kalshi API 5xx | HTTP status code | Retry once; if second failure, skip signal (it's time-sensitive) |
| Kalshi auth expired | 401 response | Immediate re-auth; order retried if within time window |
| ASOS data stale | Observation timestamp >5min old | Suppress weather signals for that station |
| Binance WS disconnect | Heartbeat timeout | Reconnect; suppress crypto signals until feed is live |
| PostgreSQL down | sqlx error | Orders still execute (state in memory); DB writes queued |
| Process crash | — | On restart: step 8 above reconstructs state from DB |
