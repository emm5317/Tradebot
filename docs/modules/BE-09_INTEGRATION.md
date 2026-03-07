# BE-9: Integration + Paper Trading

**Dependencies**: BE-1 through BE-8 (all modules)
**Blocks**: BE-10 (production hardening)
**Language**: Rust + Python

---

## Overview

Integration testing validates that all components work together as a system. Paper trading validates the strategy against real markets with fake money. This is where theory meets reality.

---

## BE-9.1: End-to-End Paper Trading Pipeline

### Deliverable
All components running together against Kalshi demo API.

### Startup sequence

```just
# One command to start everything
paper:
    just db-up
    just migrate
    python -m collector.daemon &
    python -m signals.main &
    cargo run -- --paper
```

### What must work end-to-end
1. **Collector** stores observations and market snapshots continuously
2. **Scanner** (Rust) detects contracts entering 18-minute window
3. **Scanner** publishes scan requests to NATS
4. **Signal engine** (Python) receives scan requests, evaluates with ensemble model
5. **Signal engine** publishes qualified signals to NATS
6. **Consumer** (Rust) receives signals from NATS
7. **Risk manager** approves/rejects based on all checks (including correlation)
8. **Sizing** computes quarter-Kelly position size
9. **Execution** selects market vs. limit strategy based on orderbook
10. **Order** placed against Kalshi demo API
11. **Position manager** tracks open position
12. **Re-evaluation** monitors edge on open positions (exit if edge flips)
13. **Settlement** resolves position, computes PnL
14. **Calibration** records model accuracy for feedback loop
15. **UI** displays all state in real-time via WebSocket push
16. **Metrics** records latency breakdown for every stage

### Minimum paper run
- **3 full trading days** (or enough hours to see 10+ contract settlements)
- At least **10 signals generated**
- At least **5 orders placed**
- At least **3 settlements resolved**
- Zero crashes, zero data races
- Latency within targets

### Monitoring checklist during paper run
- [ ] Terminal UI shows live state without manual refresh
- [ ] BTC price updates in real-time
- [ ] Positions appear on fill, disappear on settlement
- [ ] PnL updates correctly on each settlement
- [ ] Risk state reflects current exposure
- [ ] Circuit breaker triggers if 3 losses occur (may need to observe naturally or test manually)
- [ ] Kill switch works from UI and API
- [ ] Logs are structured JSON with correct fields
- [ ] No memory leaks (RSS stable over hours)

---

## BE-9.2: Reconciliation Check

### Deliverable
`scripts/reconcile.py` — post-session verification script.

### Checks
```python
async def reconcile():
    # 1. Orders match
    local_orders = await db.get_orders(today)
    kalshi_orders = await kalshi.get_orders(since=today_start)
    assert set(o.kalshi_order_id for o in local_orders) == set(o.id for o in kalshi_orders)

    # 2. Positions match
    local_positions = await db.get_open_positions()
    kalshi_positions = await kalshi.get_positions()
    assert len(local_positions) == len(kalshi_positions)

    # 3. Balance matches
    local_balance = computed_balance  # starting balance + net PnL
    kalshi_balance = await kalshi.get_balance()
    assert abs(local_balance - kalshi_balance) < 1  # within 1 cent

    print("✓ Reconciliation passed")
```

### Any discrepancy is a bug
Fix before proceeding to production. Common causes:
- Missed settlement event (WebSocket disconnect during settlement)
- Duplicate order (idempotency key not working)
- PnL calculation error (rounding, fees)

---

## BE-9.3: Load Test / Stress Signals

### Deliverable
`scripts/stress_test.py` — publishes 100 signals in rapid succession.

### What it validates
```python
async def stress_test():
    # Publish 100 signals in 1 second
    signals = generate_test_signals(100)
    for signal in signals:
        await nats.publish("tradebot.signals", signal.json().encode())

    await asyncio.sleep(5)  # wait for processing

    # Verify
    orders = await db.get_orders(last_5_seconds)
    assert len(orders) <= config.max_positions  # risk manager capped correctly

    rejections = await db.get_rejected_signals(last_5_seconds)
    assert len(orders) + len(rejections) == 100  # all processed

    # Check latency didn't degrade
    metrics = await fetch_metrics()
    assert metrics.total_p99_ms < 50  # excluding network
```

### What must hold under stress
- [ ] Risk manager correctly caps at max positions (5)
- [ ] Exposure limit holds
- [ ] Correlation limits hold (max 2 per city)
- [ ] Circuit breaker triggers if appropriate
- [ ] No panics, no data races, no deadlocks
- [ ] Memory usage doesn't spike (no unbounded queues)
- [ ] Latency stays under 50ms (internal processing, excluding network)

---

## BE-9.4: Dry-Run Replay Test (NEW)

### Deliverable
`scripts/dry_run.py` — replay historical data through the full pipeline.

### Specification
```python
async def dry_run(date: str):
    """Replay a historical trading day through the full pipeline."""
    # 1. Load historical observations and market snapshots for the date
    observations = await db.get_observations(date)
    snapshots = await db.get_market_snapshots(date)
    contracts = await db.get_contracts_settled_on(date)

    # 2. Mock the Kalshi client to simulate fills
    mock_kalshi = MockKalshiClient(snapshots)

    # 3. Run the pipeline in accelerated time
    for timestamp in sorted(all_timestamps):
        # Feed observation data
        # Trigger scanner
        # Process signals
        # Place mock orders
        # Resolve mock settlements

    # 4. Compare results to what would have happened
    print(f"Signals generated: {len(signals)}")
    print(f"Orders placed: {len(orders)}")
    print(f"PnL: ${pnl/100:.2f}")
```

This catches integration bugs that backtesting misses because it runs the actual pipeline code, not a simulation loop.

---

## Acceptance Criteria (BE-9 Complete)

- [ ] Full pipeline runs for 3+ trading days without crashes
- [ ] 10+ signals generated, 5+ orders placed, 3+ settlements resolved
- [ ] Reconciliation check passes (orders, positions, balance match Kalshi)
- [ ] Stress test: 100 signals processed correctly, risk limits hold
- [ ] Dry-run replay produces results consistent with backtest
- [ ] Terminal UI displays correct state throughout
- [ ] Zero memory leaks (RSS stable)
