# BE-7: UI Backend — Axum API + WebSocket Push

**Dependencies**: BE-5 (execution state), BE-6 (metrics)
**Blocks**: BE-9 (integration)
**Language**: Rust (Axum)

---

## Overview

The UI backend serves the terminal HTML interface and provides real-time state updates via REST endpoints and WebSocket push. All reads come from in-memory state — no database queries on the hot path.

---

## BE-7.1: Static File Serving + API Foundation

### Deliverable
`rust/src/ui/routes.rs` + `rust/src/ui/state.rs`

### Specification

```rust
pub struct AppState {
    pub risk: Arc<RiskManager>,
    pub positions: Arc<PositionManager>,
    pub orderbook: Arc<OrderbookManager>,
    pub latency: Arc<LatencyTracker>,
    pub kalshi: Arc<KalshiClient>,
    pub config: Arc<Config>,
    pub start_time: Instant,
    pub ws_broadcaster: broadcast::Sender<WsEvent>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(serve_terminal_html))
        .route("/api/status", get(get_status))
        .route("/api/health", get(get_health))   // NEW
        .route("/api/positions", get(get_positions))
        .route("/api/signals", get(get_signals))
        .route("/api/risk", get(get_risk))
        .route("/api/pnl", get(get_pnl))
        .route("/api/orderbook/:ticker", get(get_orderbook))
        .route("/api/metrics", get(get_metrics))
        .route("/api/kill-switch", post(toggle_kill_switch))
        .route("/ws", get(ws_handler))
        .with_state(state)
}
```

### CORS
Configured for local development only:
```rust
let cors = CorsLayer::new()
    .allow_origin(Any)
    .allow_methods([Method::GET, Method::POST])
    .allow_headers(Any);
```

### Verification
- `cargo run`, open `http://localhost:3000` → terminal UI loads
- All API endpoints return valid JSON

---

## BE-7.2: Real-Time State Endpoints

### Deliverable
JSON API endpoints consumed by the terminal UI.

### Endpoints

**GET /api/status**
```json
{
  "mode": "paper",
  "balance_cents": 50000,
  "uptime_seconds": 3600,
  "kalshi_ws": "connected",
  "binance_ws": "connected",
  "nats": "connected",
  "postgres": "connected",
  "active_contracts_tracked": 42,
  "signals_today": 12,
  "orders_today": 5
}
```

**GET /api/health** (NEW)
```json
{
  "status": "healthy",
  "components": {
    "kalshi_ws": {"status": "connected", "last_message_age_ms": 150},
    "binance_ws": {"status": "connected", "last_message_age_ms": 80},
    "nats": {"status": "connected"},
    "postgres": {"status": "connected", "pool_size": 10, "idle": 8}
  },
  "last_signal_age_seconds": 45,
  "last_order_age_seconds": 120
}
```

Status logic:
- `healthy` — all components connected, recent signals flowing
- `degraded` — one or more feeds disconnected, still operational
- `unhealthy` — database down or kill switch active

**GET /api/positions**
```json
{
  "positions": [
    {
      "ticker": "KXTEMP-26MAR07-KORD-T50-B55",
      "direction": "yes",
      "size_cents": 1200,
      "fill_price": 0.42,
      "model_prob_current": 0.68,
      "edge_current": 0.08,
      "minutes_remaining": 7.5,
      "pnl_unrealized_cents": 312
    }
  ],
  "total_exposure_cents": 3400,
  "slots_used": 2,
  "slots_max": 5
}
```

**GET /api/risk**
```json
{
  "daily_loss_cents": -1200,
  "daily_loss_limit_cents": -10000,
  "daily_loss_pct": 12.0,
  "open_exposure_cents": 3400,
  "exposure_limit_cents": 15000,
  "positions_open": 2,
  "positions_max": 5,
  "circuit_breaker": "normal",
  "circuit_breaker_reset_in": null,
  "kill_switch": false,
  "recent_losses_in_window": 1
}
```

**GET /api/pnl**
```json
{
  "today": {
    "gross_pnl_cents": 2800,
    "fees_cents": -150,
    "net_pnl_cents": 2650,
    "trades": 8,
    "wins": 5,
    "losses": 3,
    "win_rate": 0.625,
    "avg_edge_at_entry": 0.072
  },
  "recent_trades": [...]
}
```

**GET /api/orderbook/:ticker**
```json
{
  "ticker": "KXTEMP-26MAR07-KORD-T50-B55",
  "bids": [{"price": 0.42, "size": 50}, {"price": 0.41, "size": 120}],
  "asks": [{"price": 0.45, "size": 30}, {"price": 0.46, "size": 80}],
  "mid_price": 0.435,
  "spread": 0.03,
  "imbalance": 0.68,
  "last_update_ms": 150
}
```

**GET /api/metrics**
```json
{
  "latency": {
    "deser": {"p50_ms": 0.2, "p95_ms": 0.5, "p99_ms": 1.1},
    "risk": {"p50_ms": 0.1, "p95_ms": 0.3, "p99_ms": 0.8},
    "total": {"p50_ms": 180, "p95_ms": 220, "p99_ms": 350}
  },
  "throughput": {
    "signals_per_minute": 2.4,
    "orders_per_minute": 0.8
  }
}
```

### Verification
- While paper trading, hit each endpoint via curl
- Verify response matches expected state
- Verify no database queries on any endpoint (all from in-memory state)

---

## BE-7.3: WebSocket Push to UI

### Deliverable
Axum WebSocket at `/ws` pushing real-time events.

### Events

```rust
#[derive(Serialize)]
#[serde(tag = "type")]
pub enum WsEvent {
    PriceUpdate { ticker: String, mid_price: f64, spread: f64 },
    Signal { signal: SignalSummary },
    Order { order: OrderSummary },
    Settlement { ticker: String, outcome: String, pnl_cents: i64 },
    RiskUpdate { risk: RiskState },
    Log { level: String, message: String, fields: serde_json::Value },
    HealthUpdate { component: String, status: String },  // NEW
}
```

### Architecture
- `broadcast::channel` — single producer (engine events), multiple consumers (WS connections)
- Each WS connection gets a `broadcast::Receiver` and forwards events as JSON
- Backpressure: if a WS client falls behind, drop oldest events (lagging receiver)

### Verification
- Open terminal UI → live updates without page refresh
- BTC price, positions, logs update in real-time
- Open 3 browser tabs → all receive updates independently
- Slow client doesn't block fast clients

---

## Acceptance Criteria (BE-7 Complete)

- [ ] Terminal UI loads at `http://localhost:3000`
- [ ] All 8 API endpoints return correct JSON
- [ ] Health endpoint correctly reports component status
- [ ] WebSocket pushes events in real-time
- [ ] No database queries on any API endpoint (in-memory reads only)
- [ ] Multiple simultaneous WebSocket connections supported
