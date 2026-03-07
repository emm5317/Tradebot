# BE-2: Kalshi Client — Auth, REST, WebSocket, Orderbook

**Dependencies**: BE-1 (config, logging)
**Blocks**: BE-5.5 (limit orders), BE-6.1 (scanner), BE-7.2 (orderbook endpoint)
**Language**: Rust

---

## Overview

The Kalshi client is the most complex single integration. It handles authentication, REST API calls, WebSocket streaming, and in-memory orderbook state. Built incrementally — each sub-module is independently testable.

---

## BE-2.1: RSA-SHA256 Authentication

### Deliverable
`rust/src/kalshi/auth.rs`

### Specification
- Loads PEM private key from file path in config
- Signs each request: `timestamp_seconds + "\n" + http_method + "\n" + path` → RSA-SHA256 → Base64
- Produces three headers: `KALSHI-ACCESS-KEY`, `KALSHI-ACCESS-SIGNATURE`, `KALSHI-ACCESS-TIMESTAMP`
- Stateless — no token caching, each request independently signed

### Crates
- `openssl` or `aws-lc-rs` — RSA key loading and signing (fastest options; pure-Rust `rsa` crate is significantly slower and matters since every request is signed)
- `base64` — encoding
- `chrono` — timestamp generation

### Key design decisions
- **No token refresh** — Kalshi v2 API uses per-request signing, not session tokens
- **Key loaded once at startup** — stored in `Arc<RsaPrivateKey>`, cloned into client

### Verification
- Authenticated `GET /trade-api/v2/portfolio/balance` returns balance from demo API
- Signature matches Kalshi's expected format (test against known test vectors if available)

---

## BE-2.2: REST Client — Markets + Orders

### Deliverable
`rust/src/kalshi/client.rs`

### Specification

Single `reqwest::Client` instance created at startup with:
- HTTP/2 enabled (`reqwest::Client::builder().http2_prior_knowledge()`)
- Connection pool (default — reqwest handles this)
- 10-second timeout per request
- Retry on 5xx with 3 attempts, exponential backoff (1s, 2s, 4s)

### Methods

```rust
impl KalshiClient {
    pub async fn get_markets(&self, status: &str, category: Option<&str>) -> Result<Vec<Market>>;
    pub async fn get_market(&self, ticker: &str) -> Result<Market>;
    pub async fn get_balance(&self) -> Result<Balance>;
    pub async fn place_order(&self, req: OrderRequest) -> Result<OrderResponse>;
    pub async fn cancel_order(&self, order_id: &str) -> Result<CancelResponse>;
    pub async fn get_positions(&self) -> Result<Vec<Position>>;
    pub async fn get_orders(&self, params: OrderQueryParams) -> Result<Vec<Order>>;
    pub async fn get_settlements(&self, since: DateTime<Utc>) -> Result<Vec<Settlement>>;
}
```

### Error handling
- Parse Kalshi error responses into typed enum:
  ```rust
  enum KalshiError {
      RateLimit { retry_after: Duration },
      InsufficientFunds,
      MarketClosed,
      InvalidOrder { reason: String },
      AuthFailure,
      ServerError(u16),
      NetworkError(reqwest::Error),
  }
  ```
- Rate limit: respect `Retry-After` header, log at WARN

### Improvement over original plan
- **Typed error enum** — callers can match on specific failure modes
- **Automatic retry on 5xx** — prevents transient failures from killing trades
- **HTTP/2 multiplexing** — multiple concurrent requests over one connection

### Verification
- Fetch all active weather markets, print tickers
- Place a $1 paper YES order on a weather contract, confirm fill
- Place a limit order, then cancel it, verify cancellation

---

## BE-2.3: WebSocket — Market Data Feed

### Deliverable
`rust/src/kalshi/websocket.rs`

### Specification
- Uses `tokio-tungstenite` (battle-tested, thread-safe; `fastwebsockets` has soundness issues)
- Persistent connection to `KALSHI_WS_URL`
- Subscribes to `orderbook_delta` channel for tracked markets
- Auto-reconnect: exponential backoff (1s, 2s, 4s, 8s, max 30s)
- Heartbeat: ping every 30s, expect pong within 5s
- On reconnect: re-subscribe to current market set, request snapshot

### Architecture
```
[Kalshi WS] → [websocket.rs] → channel → [orderbook.rs] (state updates)
                                       → [scanner.rs]    (settlement detection)
```

Messages parsed with `simd-json` for speed on the hot path.

### Subscription management
- Dynamic subscription: as contracts enter/leave the 30-minute settlement window, subscribe/unsubscribe
- Track subscription state to avoid duplicate subscriptions
- On reconnect, re-subscribe to the full current set

### Improvement over original plan
- **`tokio-tungstenite`** confirmed as best choice — thread-safe, well-benchmarked, actively maintained
- **`simd-json`** parsing on the hot path
- **Dynamic subscription management** — only subscribe to contracts near settlement, not all markets

### Verification
- Subscribe to 5 weather markets, log orderbook updates for 10 minutes
- Kill network connection, verify reconnect + re-subscribe within 30s
- Measure: price updates should arrive within ~10ms of Kalshi's REST snapshot

---

## BE-2.4: Orderbook State Manager

### Deliverable
`rust/src/kalshi/orderbook.rs`

### Specification

```rust
pub struct OrderbookManager {
    books: DashMap<String, Orderbook>,
}

pub struct Orderbook {
    pub ticker: String,
    pub bids: BTreeMap<Decimal, i64>,  // price → size (sorted descending)
    pub asks: BTreeMap<Decimal, i64>,  // price → size (sorted ascending)
    pub last_update: Instant,
}

impl OrderbookManager {
    pub fn best_bid(&self, ticker: &str) -> Option<(Decimal, i64)>;
    pub fn best_ask(&self, ticker: &str) -> Option<(Decimal, i64)>;
    pub fn mid_price(&self, ticker: &str) -> Option<Decimal>;
    pub fn spread(&self, ticker: &str) -> Option<Decimal>;
    pub fn depth_at_price(&self, ticker: &str, side: Side, price: Decimal) -> i64;
    pub fn estimated_fill_price(&self, ticker: &str, side: Side, size: i64) -> Option<Decimal>;
    pub fn order_imbalance(&self, ticker: &str) -> Option<f64>;  // NEW
    pub fn is_stale(&self, ticker: &str, max_age: Duration) -> bool;  // NEW
}
```

### Improvements over original plan
- **`order_imbalance()`** — bid_volume / total_volume, used by signal evaluator for microstructure features
- **`is_stale()`** — detects if orderbook hasn't been updated recently (feed disconnect)
- **`BTreeMap`** for price levels — naturally sorted, efficient range queries

### Verification
- Compare `mid_price()` to REST-fetched market price — within 1 cent
- `estimated_fill_price()` for small order matches best bid/ask
- `estimated_fill_price()` for large order shows slippage

---

## Acceptance Criteria (BE-2 Complete)

- [ ] Authenticated requests succeed against Kalshi demo API
- [ ] REST client can fetch markets, place orders, cancel orders
- [ ] WebSocket connects, receives orderbook deltas, auto-reconnects
- [ ] Orderbook state matches REST snapshots within 1 cent
- [ ] All methods have typed error handling
- [ ] Latency logged for every REST call
