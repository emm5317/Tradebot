# RISK.md
Risk framework for tradebot. These rules are structural, not configurable.
---
## Hard Limits
Every limit below is enforced in `rust/src/risk/manager.rs` with atomic operations. They are checked on every signal before an order is placed. Bypassing them requires code changes, not config changes. That is by design.
| Limit | Value | Type | Code |
|-------|-------|------|------|
| Max loss per trade | $25 | Per-order check | `approve_order()` rejects if `size_cents > 2500` |
| Max daily loss | $100 | Running total | `AtomicI64`, incremented on each fill, checked pre-order |
| Max open positions | 4 | Count check | `DashMap::len() >= 4` blocks new orders |
| Max total exposure | $60 | Sum check | `AtomicI64`, sum of open sizes, checked pre-order |
| Min time to settlement | 2.5 min | Time gate | `process_signal()` rejects outside window |
| Max time to settlement | 18 min | Time gate | `process_signal()` rejects outside window |
| Kill switch | Instant | AtomicBool | Checked first, before all other logic |
| Circuit breaker | 3 losses / 30 min | Time-windowed | Auto-pauses signal intake for 1 hour |
## Kill Switch
The kill switch is an `AtomicBool` that is the very first check on every signal. When activated:
- All incoming signals are immediately discarded
- No new orders are placed
- Existing positions are NOT liquidated (they settle naturally)
- The switch can be activated via the UI button or a direct API call
The kill switch does not require a reason. If something feels wrong, pull it. Positions settle on their own within 18 minutes.
## Circuit Breaker
Automatic protection against model failure or adverse conditions.
**Trigger**: 3 losing trades within a rolling 30-minute window.
**Action**: Signal intake is paused for 1 hour. The system logs the trigger event, records all three losing trades, and resumes automatically after the cooldown.
**Rationale**: Three losses in 30 minutes at near-expiry time scales likely means one of: the model is mispricing, the data feed is stale, or market conditions have shifted. None of these are fixed by continuing to trade. The 1-hour pause gives conditions time to normalize and gives the operator time to investigate.
The circuit breaker cannot be overridden without a code change.
## Position Sizing: Quarter-Kelly
All positions are sized using the Kelly criterion at 25% of the computed fraction:
```
raw_kelly = kelly_fraction * current_balance
quarter_kelly = raw_kelly * 0.25
final_size = min(quarter_kelly, $25)
```
**Why quarter-Kelly**: Full Kelly maximizes geometric growth rate but produces drawdowns of 50%+ that are catastrophic for a $500 bankroll. Quarter-Kelly retains ~75% of the growth rate with dramatically reduced variance. The $25 hard cap provides an additional ceiling regardless of Kelly output.
**Why not half-Kelly**: At $500 starting capital, even half-Kelly can produce uncomfortable concentration. Quarter-Kelly is the conservative choice that still allows meaningful compounding.
## Time Windows
Near-expiry trading has a narrow operating window. Orders are only placed when:
```
2.5 minutes < time_to_settlement < 18 minutes
```
**Below 2.5 minutes**: Order fills may not process before settlement. The Kalshi matching engine needs time to execute. Placing orders too close to settlement risks being stuck with an unfilled order or getting a fill at a price that already reflects the settled outcome.
**Above 18 minutes**: The observation advantage weakens. At 18+ minutes, weather can still change meaningfully, and BTC volatility makes binary pricing unreliable. The edge hypothesis is specifically about the final approach to settlement.
## Edge Thresholds
Trades are only generated when the model probability diverges sufficiently from the market price:
| Category | Minimum Edge | Rationale |
|----------|-------------|-----------|
| Weather | 5 cents (5%) | Accounts for Kalshi spread + model uncertainty |
| Crypto | 6 cents (6%) | Higher threshold due to BTC's faster price moves |
These thresholds incorporate Kalshi's fee structure. A 3-cent edge after fees might be 1 cent — not worth the execution risk.
## Economic Event Blackout
Crypto signals are suppressed when a major macroeconomic event is scheduled within 30 minutes:
- FOMC rate decisions
- CPI releases
- Non-Farm Payrolls (NFP)
- GDP reports
- PCE inflation data
These events can cause BTC to move 3-5% in seconds, invalidating the realized volatility estimate that the Black-Scholes model relies on. Weather signals are unaffected.
The blackout calendar is maintained in `config/blackout_events.json` and checked by `python/utils/blackout.py` before publishing crypto signals.
## Daily Loss Tracking
The daily loss counter (`AtomicI64`) resets at midnight UTC. It tracks realized losses from settled positions, not unrealized mark-to-market. The max daily loss of $100 means:
- At $500 bankroll: maximum 20% drawdown per day
- At the $25/trade cap: 4 consecutive max-loss trades hits the daily limit
- Recovery from a max-loss day requires ~25% return — aggressive but survivable
If `max_daily_loss` is hit, the system behaves like a kill switch for the remainder of the trading day. It automatically resets at midnight UTC.
## Testing Requirements
Every limit must have corresponding tests in `rust/src/risk/`:
1. **approve_order rejects oversized trade** — Submit an order for $30, verify rejection
2. **approve_order rejects when daily loss exceeded** — Set daily loss to $99, submit $5 trade, verify rejection
3. **approve_order rejects at position cap** — Insert 4 positions, submit new order, verify rejection
4. **approve_order rejects at exposure cap** — Set exposure to $55, submit $10 order, verify rejection
5. **time gate rejects early signal** — Signal at 20 min to settlement, verify rejection
6. **time gate rejects late signal** — Signal at 2 min to settlement, verify rejection
7. **kill switch blocks all signals** — Set kill switch, submit valid signal, verify rejection
8. **circuit breaker triggers on 3 losses** — Record 3 losses in 10 minutes, verify pause
9. **circuit breaker auto-resets** — Trigger breaker, advance clock 61 min, verify acceptance
These tests are not optional. They are the proof that the risk framework works.
## Monitoring Checklist
Operators should review daily:
- Total trades placed vs. signals generated (conversion rate)
- Win/loss ratio vs. backtest prediction
- Average edge at entry vs. realized PnL
- Circuit breaker trigger count (should be rare; frequent = model problem)
- Max drawdown for the day
- Any kills witch activations
- Data feed uptime (ASOS staleness, Binance WS disconnects)
