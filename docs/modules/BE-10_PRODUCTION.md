# BE-10: Production Hardening

**Dependencies**: BE-9 (integration passing)
**Blocks**: Live trading
**Language**: Rust + Python

---

## Overview

Production hardening ensures the system survives real-world conditions: crashes, network failures, misconfiguration, and extended unattended operation. Nothing in this module adds profitability — it prevents losing money due to bugs.

---

## BE-10.1: Graceful Shutdown

### Deliverable
Signal handler for CTRL+C / SIGTERM.

### Shutdown sequence
1. **Kill switch activated** — no new orders
2. **Drain NATS consumer** — finish processing in-flight signals (max 5s)
3. **Wait for in-flight HTTP requests** — orders being placed (max 5s)
4. **Cancel unfilled limit orders** — via Kalshi REST API
5. **Persist state to DB** — positions, daily summary, latency stats
6. **Close WebSocket connections** — Kalshi WS, Binance WS (Python), UI WS clients
7. **Flush logs** — ensure all structured logs are written
8. **Exit 0**

### Implementation
```rust
let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

tokio::spawn(async move {
    tokio::signal::ctrl_c().await.unwrap();
    tracing::info!("shutdown_initiated");
    kill_switch.activate();
    shutdown_tx.send(true).unwrap();
});

// In main loop
tokio::select! {
    _ = main_loop() => {},
    _ = shutdown_rx.changed() => {
        // Graceful shutdown sequence
        drain_and_persist().await;
    }
}
```

### Verification
- Start engine, place a paper order, send SIGTERM
- Verify: order completed or cancelled, state persisted, clean exit
- Restart, verify state recovered correctly

---

## BE-10.2: Crash Recovery

### Deliverable
Startup recovery logic detecting unclean shutdown.

### Recovery checks (in order)

1. **Stale pending orders**
   - Query: `orders WHERE status = 'pending' AND created_at < now() - interval '5 minutes'`
   - Action: fetch order status from Kalshi API, update accordingly
   - If filled: create position entry
   - If cancelled/expired: mark as cancelled
   - If unknown: mark as `unknown`, log at ERROR, alert

2. **Unresolved positions past settlement**
   - Query: `orders WHERE outcome = 'pending' AND settlement_time < now()`
   - Action: fetch settlement result from Kalshi API
   - Update outcome, compute PnL, update daily summary

3. **Daily loss counter reconciliation**
   - Compute: sum of today's realized losses from `orders` table
   - Compare: to `daily_loss` atomic (will be 0 on restart)
   - Action: set atomic to computed value

4. **WAL replay** (if using sled/WAL, from improvement 3.3 in PLAN_ANALYSIS)
   - Check for unprocessed WAL entries
   - Replay state mutations since last DB flush

### Logging
Every recovery action logged at INFO with details. Any discrepancy logged at WARN.

### Verification
- Start engine, kill -9 mid-execution (unclean crash)
- Restart, verify recovery runs and state is correct
- Verify no duplicate orders after recovery

---

## BE-10.3: Config for Live Trading

### Deliverable
Separate `.env.production` config.

```env
# Production Kalshi endpoints
KALSHI_BASE_URL=https://trading-api.kalshi.com
KALSHI_WS_URL=wss://trading-api.kalshi.com/trade-api/ws/v2

# Live mode
PAPER_MODE=false

# Tighter risk limits for initial live trading
MAX_TRADE_SIZE_CENTS=1000       # $10 max per trade (start conservative)
MAX_DAILY_LOSS_CENTS=5000       # $50 max daily loss
MAX_POSITIONS=3                  # 3 concurrent positions
MAX_EXPOSURE_CENTS=5000         # $50 max total exposure
```

### Live mode confirmation
On startup with `PAPER_MODE=false`:
```
⚠  LIVE TRADING MODE — Real money at risk
   Max trade size:  $10.00
   Max daily loss:  $50.00
   Max positions:   3
   Max exposure:    $50.00

   Type 'CONFIRM' to proceed, or Ctrl+C to abort:
```

Stdin confirmation required. No flags to skip it.

### Gradual rollout plan
1. **Week 1**: $10 max trade, $50 daily limit, weather only
2. **Week 2**: Review calibration, adjust sigma if needed
3. **Week 3**: Add crypto, same limits
4. **Week 4**: If profitable and calibrated, increase to $25 max trade
5. **Month 2+**: Continue scaling based on Sharpe ratio and calibration quality

---

## BE-10.4: Alert System (NEW)

### Deliverable
`rust/src/alerts.rs` — lightweight notification system.

### Specification

```rust
pub struct AlertManager {
    discord_webhook: Option<String>,
}

impl AlertManager {
    pub async fn alert(&self, level: AlertLevel, event: &str, details: &str) {
        // Always log
        match level {
            AlertLevel::Critical => tracing::error!(event, details),
            AlertLevel::Warning => tracing::warn!(event, details),
            AlertLevel::Info => tracing::info!(event, details),
        }

        // Send Discord webhook if configured
        if let Some(url) = &self.discord_webhook {
            let _ = self.send_discord(url, level, event, details).await;
        }
    }
}
```

### Alert events
| Event | Level | When |
|-------|-------|------|
| Kill switch activated | Critical | Manual or automatic activation |
| Circuit breaker tripped | Critical | 3 losses in 30 min |
| Daily loss 80% | Warning | Approaching limit |
| Daily loss 100% | Critical | Limit hit, trading paused |
| Feed disconnected > 60s | Warning | Any data feed down |
| Calibration drift > 10% | Warning | Model needs adjustment |
| Crash recovery ran | Warning | Unclean shutdown detected |
| Order fill | Info | Every fill (useful for mobile monitoring) |

### Verification
- Configure Discord webhook
- Trigger circuit breaker → verify Discord notification received
- Verify all critical events produce notifications

---

## BE-10.5: Operational Runbook (NEW)

### Deliverable
`docs/RUNBOOK.md` — operational procedures for common situations.

### Contents
1. **Starting the system** — `just paper` or `just live`
2. **Stopping the system** — Ctrl+C (graceful) or kill switch
3. **Investigating a bad trade** — query signals/orders tables, check calibration
4. **Adjusting sigma** — edit sigma table, restart signal engine
5. **Adding a new station** — update `stations.json`, restart collector
6. **Handling a crash** — check recovery log, run reconciliation
7. **Scaling up limits** — edit `.env`, restart with new limits
8. **Emergency procedures** — kill switch API call, Docker stop

---

## Acceptance Criteria (BE-10 Complete)

- [ ] Graceful shutdown persists all state, cancels unfilled orders
- [ ] Crash recovery correctly resolves pending orders and positions
- [ ] Live config requires stdin confirmation
- [ ] Alerts sent to Discord for all critical events
- [ ] Runbook covers all common operational scenarios
- [ ] Kill process, restart, reconcile — zero discrepancies
