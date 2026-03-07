# BE-8: Backtesting + Model Calibration

**Dependencies**: BE-3 (historical data), BE-4 (models)
**Blocks**: BE-9 (integration — calibration runs in production)
**Language**: Python

---

## Overview

Backtesting validates the strategy against historical data. Calibration keeps the model honest in production. Together, they form the feedback loop that drives long-term profitability. This is the module that determines whether the bot makes money or loses it.

---

## BE-8.1: Weather Backtest Runner

### Deliverable
`python/backtest/weather_backtest.py`

### Specification

```python
class WeatherBacktester:
    async def run(self, months: int = 6) -> BacktestReport:
        """Run full weather backtest against historical data."""
        results = []
        for contract in settled_contracts:
            for minutes_before in [15, 10, 5]:
                observation = await self.get_observation(
                    contract.station,
                    contract.settlement_time - timedelta(minutes=minutes_before)
                )
                if observation is None:
                    continue

                market_price = await self.get_market_snapshot(
                    contract.ticker,
                    contract.settlement_time - timedelta(minutes=minutes_before)
                )
                if market_price is None:
                    continue

                # Evaluate with ensemble model
                signal = self.evaluator.evaluate(contract, observation, market_price)
                if signal:
                    pnl = self.simulate_trade(signal, contract.settled_yes)
                    results.append(pnl)

        return self.compile_report(results)
```

### Report output

```
╔══════════════════════════════════════════════╗
║         WEATHER BACKTEST REPORT              ║
╠══════════════════════════════════════════════╣
║ Period:         6 months                     ║
║ Total signals:  342                          ║
║ Qualifying:     187 (passed entry filters)   ║
║ Win rate:       62.3%                        ║
║ Avg edge:       7.2 cents                    ║
║ Net PnL:        $1,247 (quarter-Kelly)       ║
║ Sharpe (daily): 1.84                         ║
║ Max drawdown:   $312                         ║
╠══════════════════════════════════════════════╣
║ BY STATION                                   ║
║ KORD (Chicago):    41 trades, 63% win, +$287 ║
║ KJFK (NYC):        38 trades, 58% win, +$142 ║
║ KDEN (Denver):     29 trades, 66% win, +$234 ║
║ KPHX (Phoenix):    22 trades, 68% win, +$198 ║
║ ...                                          ║
╠══════════════════════════════════════════════╣
║ BY TIME OF DAY                               ║
║ Morning (6-12):    45 trades, 60% win        ║
║ Afternoon (12-18): 82 trades, 64% win        ║
║ Evening (18-24):   38 trades, 58% win        ║
║ Night (0-6):       22 trades, 68% win        ║
╚══════════════════════════════════════════════╝
```

### Key packages
- `polars` — faster than pandas for large dataset analytics (10-100x on groupby/aggregation)
- `numpy` — statistical calculations
- `tabulate` or `rich` — formatted output

### Improvements over original plan
- **Breakdown by time of day** — validates whether σ varies diurnally (it does)
- **Breakdown by station** — identifies which markets have the most edge
- **Ensemble model** tested instead of single physics model
- **`polars`** instead of pandas — significantly faster for backtesting loops
- **Multiple evaluation points** per contract (T-15, T-10, T-5) — finds optimal entry timing

### Pass criteria
- Average edge > 5 cents
- Win rate > 58%
- Minimum 50 qualifying setups
- Sharpe ratio > 1.5 (daily)

### Verification
- Run against 6+ months of data
- If criteria not met, output which σ values per station/hour would improve results

---

## BE-8.2: Crypto Backtest Runner

### Deliverable
`python/backtest/crypto_backtest.py`

### Specification
Same structure as weather but:
- Uses BTC historical klines from Binance REST API (1-minute candles)
- Uses Black-Scholes model
- Additional breakdown by **volatility regime**:
  - Low vol (< 40% annualized): hypothesis is edge is larger here
  - Medium vol (40-80%)
  - High vol (> 80%): hypothesis is edge disappears due to actual uncertainty

### Pass criteria
- Average edge > 6 cents
- Win rate > 56%
- Minimum 30 qualifying setups
- Low-vol regime shows larger edge than high-vol (validates thesis)

---

## BE-8.3: Model Calibration Evaluator

### Deliverable
`python/calibration/evaluator.py`

### Specification

```python
class CalibrationEvaluator:
    def record_outcome(self, entry: CalibrationEntry):
        """Record model prediction vs actual outcome after settlement."""

    def compute_calibration(self, entries: list[CalibrationEntry]) -> CalibrationReport:
        """Group by probability bucket, compute actual win rate per bucket."""

    def detect_drift(self, entries: list[CalibrationEntry], window: int = 50) -> list[DriftAlert]:
        """Detect if any bucket drifts > 10% from expected over rolling window."""

    def recommend_adjustment(self, drift: DriftAlert) -> Adjustment:
        """Recommend sigma change direction and magnitude."""
```

### Probability buckets
```
0-10%, 10-20%, 20-30%, 30-40%, 40-50%,
50-60%, 60-70%, 70-80%, 80-90%, 90-100%
```

### Calibration report
```
Bucket   | Predictions | Actual Win% | Expected% | Drift
---------|-------------|-------------|-----------|------
0-10%    |     12      |    8.3%     |   5.0%    | +3.3%
10-20%   |     18      |   16.7%    |  15.0%    | +1.7%
...
70-80%   |     34      |   64.7%    |  75.0%    | -10.3% ⚠
80-90%   |     28      |   78.6%    |  85.0%    | -6.4%
90-100%  |     15      |   93.3%    |  95.0%    | -1.7%
```

### Drift detection
- Rolling window of last 50 observations per bucket
- Alert if |actual_win_rate - bucket_midpoint| > 10%
- Recommend: if model overestimates (drift negative), increase σ. If underestimates, decrease σ.

### Key package: `scikit-learn`
- `IsotonicRegression` — recalibrate ensemble probabilities
- `calibration_curve` — visualize calibration quality

### Verification
- Feed 100 synthetic outcomes where model is 10% miscalibrated
- Verify evaluator detects drift in correct bucket
- Verify adjustment recommendation is correct direction

---

## BE-8.4: Calibration Drift Monitor

### Deliverable
`python/calibration/drift.py`

### Specification

```python
class DriftMonitor:
    async def run_daily_check(self):
        """Run after each trading day or after every N settlements."""
        entries = await self.db.get_recent_calibration(days=30)
        report = self.evaluator.compute_calibration(entries)
        alerts = self.evaluator.detect_drift(entries)

        # Persist report
        await self.db.insert_calibration_report(report)

        # Alert on drift
        for alert in alerts:
            logger.warning(
                "calibration_drift",
                bucket=alert.bucket,
                expected=alert.expected,
                actual=alert.actual,
                drift=alert.drift,
                recommended_sigma_change=alert.recommendation
            )

            # Send Discord notification if configured
            if self.discord_webhook:
                await self.notify_discord(alert)

    async def auto_adjust_sigma(self, alerts: list[DriftAlert]):
        """Optionally auto-adjust sigma table based on drift.
        Only applies adjustments < 10% to avoid overcorrection.
        Requires manual confirmation for larger adjustments.
        """
```

### TimescaleDB continuous aggregate
The `calibration_rolling` continuous aggregate (from BE-1.2) pre-computes daily calibration stats. The drift monitor reads from this materialized view instead of scanning raw data — queries are instant.

### Verification
- Inject calibration data showing 15% drift in the 70-80% bucket
- Verify alert raised at WARN level
- Verify Discord notification sent (if configured)
- Verify recommended adjustment is correct

---

## Acceptance Criteria (BE-8 Complete)

- [ ] Weather backtest runs against 6+ months of data
- [ ] Weather backtest meets pass criteria (edge > 5¢, win rate > 58%)
- [ ] Crypto backtest meets pass criteria (edge > 6¢, win rate > 56%)
- [ ] Calibration evaluator detects intentional 10% drift
- [ ] Drift monitor runs daily and produces reports
- [ ] Sigma adjustment recommendations are directionally correct
- [ ] Backtest results broken down by station, time of day, and vol regime
