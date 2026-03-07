# BE-4: Signal Engine — Models, Evaluation, Publishing

**Dependencies**: BE-1 (database), BE-3 (data feeds)
**Blocks**: BE-5.7 (signal consumer), BE-8 (backtesting)
**Language**: Python

---

## Overview

The signal engine evaluates whether a contract has tradeable edge. It runs probabilistic models, compares to market prices, applies entry filters, and publishes qualified signals. This is where profitability lives or dies.

---

## BE-4.1: Weather Physics Model

### Deliverable
`python/models/physics.py`

### Core function
```python
def compute_weather_probability(
    current_temp_f: float,
    threshold_f: float,
    minutes_remaining: float,
    sigma_per_10min: float = 0.3
) -> float:
    """P(temp exceeds threshold at settlement) using Gaussian diffusion."""
    if minutes_remaining <= 0:
        return 1.0 if current_temp_f >= threshold_f else 0.0

    delta = threshold_f - current_temp_f
    sigma_total = sigma_per_10min * math.sqrt(minutes_remaining / 10.0)
    z = delta / sigma_total
    return 1.0 - stats.norm.cdf(z)
```

### Improvement: Ensemble model (NEW)

```python
def compute_ensemble_probability(
    current_temp_f: float,
    threshold_f: float,
    minutes_remaining: float,
    station: str,
    hour: int,
    month: int,
    recent_temps: list[float],  # last 60 min of observations
    sigma_table: dict,          # per (station, hour, month) sigma
    weights: tuple[float, float, float] = (0.5, 0.25, 0.25)
) -> float:
    """Ensemble of physics, climatology, and trend models."""

    # 1. Physics model with station-specific sigma
    sigma = sigma_table.get((station, hour, month), 0.3)
    p_physics = compute_weather_probability(current_temp_f, threshold_f, minutes_remaining, sigma)

    # 2. Climatological prior — historical win rate for similar setups
    p_climo = climatological_probability(station, hour, month, threshold_f, current_temp_f)

    # 3. Trend extrapolation — linear fit on recent observations
    p_trend = trend_extrapolation_probability(recent_temps, threshold_f, minutes_remaining)

    # Weighted ensemble
    w1, w2, w3 = weights
    return w1 * p_physics + w2 * p_climo + w3 * p_trend
```

### Key packages
- `scipy.stats` — `norm.cdf` for Gaussian probability
- `numpy` — linear regression for trend extrapolation
- `scikit-learn` — `IsotonicRegression` for calibrating ensemble output

### Why ensemble matters
A single σ=0.3 model will be well-calibrated for "average" conditions but systematically wrong for:
- Hot summer afternoons (σ should be ~0.5 due to convective turbulence)
- Calm winter nights (σ should be ~0.15 under radiation inversions)
- Coastal stations (σ lower due to maritime moderation)

The ensemble approach lets each component capture different aspects of temperature evolution.

### Verification
Unit tests:
- At threshold, any time → P ≈ 0.50
- 6°F below, 12 min → P ≈ 2%
- 1°F below, 12 min → P ≈ 30%
- 1°F above, 12 min → P ≈ 70%
- Ensemble probability falls between min/max of component models

---

## BE-4.2: Black-Scholes Binary Option Model

### Deliverable
`python/models/binary_option.py`

### Core function
```python
def compute_binary_probability(
    spot: float,
    strike: float,
    minutes_remaining: float,
    sigma_annual: float,
    risk_free_rate: float = 0.05
) -> float:
    """N(d2) for near-expiry binary option."""
    if minutes_remaining <= 0:
        return 1.0 if spot >= strike else 0.0

    T = minutes_remaining / 525600  # years
    sigma = sigma_annual
    d2 = (math.log(spot / strike) + (risk_free_rate - 0.5 * sigma**2) * T) / (sigma * math.sqrt(T))
    return stats.norm.cdf(d2)
```

### Edge source
Uses **realized** 30-minute volatility from the Binance feed, not implied vol from options markets. Near expiry (<15 min), realized vol is a better predictor than implied vol because:
- Implied vol from Deribit updates slowly for near-term expirations
- Realized vol captures current market regime (trending vs. ranging)
- Market makers on Kalshi use stale implied vol → mispricing opportunity

### Verification
- BTC at $65,100 / $65,000 strike / 10 min / 60% vol → P > 0.50
- BTC at $64,000 / $65,000 strike / 5 min / 40% vol → P < 0.10
- At strike, any vol/time → P ≈ 0.50

---

## BE-4.3: Weather Signal Evaluator

### Deliverable
`python/signals/weather.py`

### Specification

```python
class WeatherSignalEvaluator:
    def evaluate(
        self,
        contract: Contract,
        observation: ASOSObservation,
        orderbook: OrderbookState,   # NEW: microstructure data
        recent_temps: list[float],   # NEW: for trend model
    ) -> Signal | None:
        """Evaluate a weather contract. Returns Signal if edge found, else None."""

        # 1. Check time window
        minutes = (contract.settlement_time - now()).total_minutes()
        if not (8 <= minutes <= 18):
            return None

        # 2. Check observation freshness
        if observation.is_stale:
            return None

        # 3. Compute ensemble probability
        model_prob = compute_ensemble_probability(
            current_temp_f=observation.temperature_f,
            threshold_f=contract.threshold,
            minutes_remaining=minutes,
            station=contract.station,
            ...
        )

        # 4. Get market price from orderbook
        market_price = orderbook.mid_price

        # 5. Compute raw edge
        if model_prob > market_price:
            direction = "yes"
            edge = model_prob - market_price
        else:
            direction = "no"
            edge = market_price - model_prob

        # 6. Apply microstructure adjustments (NEW)
        spread_cost = orderbook.spread / 2
        effective_edge = edge - spread_cost

        if orderbook.spread > 0.10:
            effective_edge *= 0.85  # discount for wide spread uncertainty

        # 7. Check edge threshold
        if effective_edge < 0.05:
            return None

        # 8. Compute Kelly
        if direction == "yes":
            win_prob, win_payout = model_prob, (1.0 - market_price)
            lose_prob, lose_payout = (1 - model_prob), market_price
        else:
            win_prob, win_payout = (1 - model_prob), market_price
            lose_prob, lose_payout = model_prob, (1.0 - market_price)

        kelly = (win_prob * win_payout - lose_prob * lose_payout) / win_payout

        if kelly < 0.04:
            return None

        return Signal(
            ticker=contract.ticker,
            direction=direction,
            model_prob=model_prob,
            market_price=market_price,
            edge=effective_edge,
            kelly_fraction=kelly,
            minutes_remaining=minutes,
            spread=orderbook.spread,          # NEW
            order_imbalance=orderbook.imbalance,  # NEW
        )
```

### Improvement over original plan
- **Ensemble model** instead of single physics model
- **Spread-adjusted edge** — accounts for execution cost in signal quality
- **Microstructure features** included in signal for execution strategy selection
- **Orderbook state** used for mid-price instead of last trade price

### Verification
- Feed historical contract + observation pairs with known outcomes
- Verify signals generated when expected, suppressed when conditions not met
- Verify spread adjustment reduces false signals in wide-spread markets

---

## BE-4.4: Crypto Signal Evaluator

### Deliverable
`python/signals/crypto.py`

### Specification
Same structure as weather but:
- Uses Black-Scholes model
- Higher edge threshold: `effective_edge > 0.06`
- Tighter time window: `5 <= minutes <= 15`
- Additional check: no active blackout event (FOMC, CPI, etc.)
- Additional check: Binance feed is live (not stale)

### Blackout events
Loaded from `config/blackout_events.json`:
```json
[
  {"event": "FOMC", "start": "2026-03-18T14:00:00Z", "end": "2026-03-18T15:00:00Z"},
  {"event": "CPI", "start": "2026-04-10T12:30:00Z", "end": "2026-04-10T13:00:00Z"}
]
```

During blackout windows, crypto signals are suppressed. BTC can move 3-5% in seconds during FOMC — the model can't handle that.

---

## BE-4.5: Signal Publisher

### Deliverable
`python/signals/publisher.py`

### Specification

```python
class SignalPublisher:
    async def publish(self, signal: Signal):
        """Publish signal to NATS and persist to database."""
        # 1. Validate with pydantic
        validated = SignalSchema.model_validate(signal)

        # 2. Publish to NATS JetStream
        await self.nats.publish(
            "tradebot.signals",
            validated.model_dump_json().encode()
        )

        # 3. Persist to signals table
        await self.db.insert_signal(validated)

        # 4. Log
        logger.info("signal_published", ticker=signal.ticker, edge=signal.edge)
```

### Improvement over original plan
- **NATS JetStream** instead of Redis Streams — simpler delivery guarantees
- **Pydantic validation** before publish — catches schema errors early
- **Dual write** (NATS + DB) — signals are both actionable and auditable

### Signal schema (pydantic)
```python
class SignalSchema(BaseModel):
    ticker: str
    signal_type: Literal["weather", "crypto"]
    direction: Literal["yes", "no"]
    model_prob: float = Field(ge=0, le=1)
    market_price: float = Field(ge=0, le=1)
    edge: float = Field(ge=0)
    kelly_fraction: float = Field(ge=0, le=1)
    minutes_remaining: float = Field(ge=0)
    spread: float = Field(ge=0)
    order_imbalance: float = Field(ge=0, le=1)
    published_at: datetime = Field(default_factory=datetime.utcnow)
```

### Verification
- Publish 10 test signals
- All appear in NATS stream (`nats stream view tradebot-signals`)
- All persisted in `signals` table
- Invalid signal (e.g., `edge=-0.1`) raises validation error, not published

---

## Acceptance Criteria (BE-4 Complete)

- [ ] Weather physics model passes all unit tests
- [ ] Ensemble model produces better-calibrated probabilities than physics alone
- [ ] Black-Scholes model passes all unit tests
- [ ] Weather evaluator generates signals with correct edge/kelly
- [ ] Crypto evaluator respects blackout windows
- [ ] Signals published to NATS and persisted to database
- [ ] All signals validated via pydantic schema before publish
