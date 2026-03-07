# soul.md
The philosophical and strategic foundation of tradebot. Read this before writing code.
---
## Core Thesis
We are not predicting the future. We are reading the present faster than the market does.
Near-expiry settlement trading is an observation race, not a forecasting exercise. When a weather contract settles in 12 minutes and the temperature is already 6°F below the threshold, the outcome is determined by physics. The only question is whether the market knows it yet.
This distinction matters because it defines the entire system design: we need speed and accuracy of observation, not sophistication of prediction. A simple physics model that runs 2 minutes ahead of the market beats a complex ML model that runs 2 minutes behind.
## Design Principles
### 1. Safety Is a Hard Gate, Not a Dial
Risk limits are not parameters to tune for performance. They are structural invariants — the same way trust accounting rules work in legal practice. The daily loss limit, position cap, and kill switch exist to guarantee survival. They are enforced at the lowest level (atomic operations in Rust) and cannot be circumvented by any higher-level logic.
The circuit breaker is not a suggestion. Three losses in thirty minutes means something is wrong with the model, the data feed, or the market. Pausing for an hour is the correct response.
### 2. Prove the Edge Before Risking Capital
Every hypothesis must be falsifiable through backtest before a single dollar is deployed. The success criteria are explicit: average edge >5 cents, win rate >58%, minimum 50 qualifying historical setups. If the data doesn't support the edge, the strategy doesn't trade.
Paper trading is the second gate. The system must demonstrate correct behavior — risk limits enforced, orders sized correctly, signals generated at the right times — before touching real money.
This is not overcaution. This is how you avoid learning expensive lessons.
### 3. Speed Serves the Edge
Latency optimization exists to capture a real, measured advantage — not as an end in itself. The priority order is clear:
1. WebSocket market data (eliminates 98% of price detection latency)
2. Persistent HTTP/2 connections (eliminates TCP handshake per order)
3. Fresh ASOS observations (1-minute vs. hourly data)
4. Pre-warmed connections and cached state
5. Lock-free hot path (atomics over mutexes)
Each optimization has a measurable impact. If it doesn't measurably improve the signal-to-order pipeline, it doesn't belong on the critical path.
### 4. Simplicity Over Cleverness
The weather model is a cumulative normal distribution over temperature drift. The crypto model is textbook Black-Scholes adapted for a binary option. Neither is novel. Both are well-understood, testable, and debuggable.
Complexity is the enemy of reliability in a system that executes financial transactions automatically. Every additional parameter, every clever trick, every edge case handler is a place where bugs hide. The system should be simple enough that any failure mode is obvious from the logs.
### 5. Two Languages, Clear Boundary
Python owns signal generation and backtesting — domains where iteration speed matters and the math libraries are unmatched. Rust owns execution — the domain where latency, correctness, and type safety matter.
Redis Streams is the boundary. Signals flow one direction: Python → Redis → Rust. This is not a microservices architecture. It's two programs that talk through a queue. Keep it that simple.
### 6. Observations Over Predictions
The entire edge rests on having fresher, more accurate observations than market participants. This means:
- Iowa State Mesonet 1-minute ASOS data, not NWS hourly
- Binance WebSocket tick stream, not periodic REST polls
- Kalshi WebSocket orderbook, not REST market snapshots
- Realized 30-minute vol from actual returns, not stale implied vol from Deribit
Every data source decision optimizes for freshness and accuracy of what is happening *right now*.
### 7. Quarter-Kelly Is the Right Size
Full Kelly criterion maximizes long-run geometric growth but produces drawdowns that would blow a $500 account on a bad week. Quarter-Kelly captures ~75% of the growth rate with dramatically less variance. Combined with the $25 hard cap per trade, this ensures no single trade is existential.
Position sizing is a survival strategy, not a growth strategy. Survive long enough to let the edge compound.
## What Success Looks Like
**Phase 1 success**: The system can authenticate with Kalshi, fetch live markets, pull real-time ASOS observations, stream BTC prices, and store everything in PostgreSQL. No trading. Just plumbing that works.
**Phase 2 success**: Backtests confirm at least one hypothesis with statistically meaningful edge. If neither hypothesis holds, the project pivots or pauses — not pushes forward on hope.
**Phase 3 success**: The signal engine correctly identifies qualifying setups in real-time and publishes well-formed signals to Redis. Measured against historical setups, it would have generated the same signals the backtest identified.
**Phase 4 success**: Paper trading demonstrates correct end-to-end behavior. Risk limits enforced. Orders placed at the right time. Positions tracked accurately. PnL reconciles.
**Live success**: Positive expected value over a 30-day window with risk limits never breached. Not necessarily profitable every day — but profitable in aggregate with the edge matching backtest predictions within a reasonable confidence interval.
## What Failure Looks Like
- Deploying live before backtests confirm edge
- Relaxing risk limits after a losing streak
- Optimizing latency for problems that don't exist yet
- Adding ML complexity before the simple model is proven
- Ignoring the circuit breaker because "this time is different"
- Treating paper trading as a formality instead of a gate
## On Patience
The build plan is 9 weeks. The temptation will be to skip ahead — to place live orders in week 3 because "the signal looks good." Resist this. The plan exists because the failure modes of automated trading are financial, not technical. A bug in a SaaS app shows a wrong number on screen. A bug in a trading system loses real money.
Every phase gate exists to catch a category of error before it costs capital. Respect the gates.
