# Phase 5 — Calibration & Production Readiness

**Timeline:** Weeks 12–18
**Risk:** LOW
**Goal:** Analytics, monitoring, and operational hardening for sustained live trading

---

## 5.1 Per-Strategy Analytics

### Problem
No way to evaluate strategy performance independently. Weather and crypto P&L are commingled.

### Implementation

**New migration:** `015_strategy_analytics.sql`
```sql
CREATE TABLE strategy_performance (
    id              BIGSERIAL PRIMARY KEY,
    strategy        TEXT NOT NULL,  -- 'weather', 'crypto'
    date            DATE NOT NULL,
    signals_generated INTEGER NOT NULL DEFAULT 0,
    signals_executed  INTEGER NOT NULL DEFAULT 0,
    win_count       INTEGER NOT NULL DEFAULT 0,
    loss_count      INTEGER NOT NULL DEFAULT 0,
    realized_pnl_cents INTEGER NOT NULL DEFAULT 0,
    avg_edge        REAL,
    avg_kelly       REAL,
    brier_score     REAL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(strategy, date)
);
```

**Daily aggregation job:**
- Run at midnight UTC (or after last settlement)
- Aggregate from `signals` + `orders` tables
- Compute Brier score from model_prob vs actual settlement

---

## 5.2 Calibration Dashboard

### Problem
The existing dashboard shows real-time state but no historical performance or model calibration metrics.

### Implementation

**New dashboard pages:**
- `/calibration` — Brier score over time, per-strategy and per-station
- `/calibration/weather/{station}` — station-specific model performance
- `/calibration/crypto` — RTI accuracy, edge quality, fill rate
- `/performance` — P&L curve, drawdown, Sharpe ratio

**Data sources:**
- `strategy_performance` table for daily metrics
- `signals` table for signal-level analysis
- `calibration` table for model parameter history

---

## 5.3 P&L Attribution

### Problem
Cannot attribute P&L to specific model components (which ensemble weight contributed to the edge?).

### Implementation

**Extend signal logging:**
- Add `model_components` JSONB column to `signals` table
- Weather: `{physics: 0.35, hrrr: 0.25, trend: 0.20, climo: 0.20, prob_each: [...]}`
- Crypto: `{shadow_rti: X, basis_adj: Y, funding_adj: Z, vol_source: "dvol"}`

**Attribution analysis:**
- For each settled signal: decompose edge into component contributions
- Track which components are most/least accurate over time
- Feed back into ensemble weight optimization

---

## 5.4 Reconciliation Loop

### Problem
Position tracker can drift from exchange reality (network errors, missed fills, restart gaps).

### Implementation

**Periodic reconciliation (every 5 min):**
1. Query Kalshi REST API for all open positions
2. Compare with in-memory position tracker
3. Discrepancies:
   - Position on exchange but not in tracker → add to tracker, log WARNING
   - Position in tracker but not on exchange → remove, log WARNING
   - Quantity mismatch → update tracker, log WARNING
4. Reconciliation results written to DB for audit

**Startup reconciliation** (already in Phase 2.5) runs before this periodic loop begins.

---

## 5.5 Clock Discipline

### Problem
Settlement times are precise to the second. Clock drift could cause signals at wrong times.

### Implementation

- On startup: log system clock offset from NTP
- If offset > 500ms: warn
- If offset > 2s: refuse to start in live mode
- Periodic check every 5 min during operation
- Use `chrono::Utc::now()` consistently (already the case)

---

## 5.6 Dead-Letter Handling

### Problem
Failed NATS messages, unparseable signals, or rejected orders currently get logged and dropped. No retry or investigation mechanism.

### Implementation

**NATS JetStream dead-letter subject:** `tradebot.deadletter`

**Route to dead-letter on:**
- Signal deserialization failure (malformed JSON)
- Risk check rejection (logged for analysis, not retried)
- Order submission failure after 3 retries
- Unknown signal types

**Dead-letter consumer:**
- Separate lightweight process that reads dead-letter subject
- Writes to `dead_letters` DB table
- Discord alert on accumulation (>5 in 10 min)

---

## 5.7 Integration Tests for Exchange Edge Cases

### Problem
Unit tests mock exchange responses. Need integration tests that verify handling of real exchange behaviors.

### Implementation

**Test scenarios (mock exchange server):**
1. WebSocket disconnect mid-stream → reconnects and resumes
2. Rate limit 429 response → backs off correctly
3. Partial fill response → position tracker updated correctly
4. Order rejected (insufficient funds) → state machine transitions to Rejected
5. Market closed response → no retry
6. Stale orderbook (no updates for 10s) → execution blocked
7. Kill switch toggle → all pending orders cancelled
8. Clock skew >2s → startup refused

**Framework:** Use `axum` to build mock exchange server in test harness.

---

## 5.8 Per-Feed Health Scoring

### Problem
Phase 0.4 implemented binary health (healthy/stale). Need more nuanced scoring.

### Implementation

**Health score (0.0–1.0) per feed:**
- `1.0`: receiving data, latency < P50 threshold
- `0.75`: receiving data, latency > P50 but < P95
- `0.50`: receiving data, but intermittent gaps
- `0.25`: last update > threshold but < 2x threshold
- `0.0`: last update > 2x threshold or connection lost

**Aggregate health:**
- Crypto health = min(binance_spot, coinbase) (must have both)
- Weather health = kalshi_ws health
- System health = min(crypto, weather, nats, redis, postgres)

**Expose via:**
- `GET /health` endpoint (already from Phase 0.2)
- `GET /health/detail` — per-feed breakdown
- Redis `system:health` key for dashboard

---

## Verification Checklist

- [ ] Strategy performance aggregated daily
- [ ] Calibration dashboard shows Brier scores and trends
- [ ] P&L attributed to model components
- [ ] Reconciliation catches simulated discrepancies
- [ ] Clock drift >2s prevents live startup
- [ ] Dead letters captured and alerted
- [ ] All 8 integration test scenarios pass
- [ ] Health scoring correctly degrades on feed issues
