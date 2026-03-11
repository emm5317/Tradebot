# Phase 11.3 — ANAL + RISK Pages

## ANAL Page — Analytics & Calibration

### Layout
```
┌──────────────────────────────────┬──────────────────────────────────┐
│  BRIER SCORE TREND (Chart.js)    │  CALIBRATION CURVE (Chart.js)   │
│  ┌──────────────────────────┐    │  ┌──────────────────────────┐   │
│  │     ╱╲    ╱╲             │    │  │        ╱                 │   │
│  │    ╱  ╲╱╱  ╲   weather  │    │  │      ╱   ● ●            │   │
│  │   ╱         ╲  crypto   │    │  │    ╱  ●                  │   │
│  │  ╱           ╲──────    │    │  │  ╱ ●        predicted    │   │
│  └──────────────────────────┘    │  │╱●          vs actual    │   │
│  30-day trend per strategy       │  └──────────────────────────┘   │
├──────────────────────────────────┼──────────────────────────────────┤
│  DRAWDOWN CHART (Chart.js)       │  EDGE DECAY SCATTER             │
│  ┌──────────────────────────┐    │  ┌──────────────────────────┐   │
│  │ ▓▓▓▓▓▓▓▓▓▓▓▓▓▓          │    │  │  ·  ·                   │   │
│  │              ▓▓▓▓        │    │  │    · · ·  ·             │   │
│  │                  ▓▓▓▓▓   │    │  │  ·  · ···  ···         │   │
│  │  cum P&L         max DD  │    │  │    ·····  ·····  ····   │   │
│  └──────────────────────────┘    │  │  edge vs minutes_remain  │   │
│                                   │  └──────────────────────────┘   │
├──────────────────────────────────┴──────────────────────────────────┤
│  STRATEGY PERFORMANCE TABLE (30 days)                               │
│  date    strat   signals  exec  W/L   edge   kelly  brier   P&L   │
│  Mar 11  crypto  45       8     6/2   5.2%   8.1%   0.182   +$12  │
│  Mar 11  weathr  12       3     2/1   6.8%   10.2%  0.156   +$5   │
└─────────────────────────────────────────────────────────────────────┘
```

### Features
- **Brier score trend**: Chart.js line chart, per-strategy, 30-day, threshold lines at 0.2/0.3
- **Calibration curve**: Scatter — predicted probability buckets vs actual outcome rate (from `calibration_rolling` view)
- **Drawdown chart**: Dual area chart — cumulative P&L (green fill) + drawdown (red fill below)
- **Edge decay scatter**: Edge value vs minutes_remaining at signal time, color by outcome
- **Strategy performance table**: Full 30-day table from existing `/api/strategy-performance`

### New API Endpoints
```python
GET /api/edge-decay?days=30
  → [{"edge": 0.082, "minutes_remaining": 12.5, "outcome": "win", "signal_type": "crypto"}, ...]

GET /api/calibration-curve?signal_type=crypto
  → [{"bucket": 0.1, "predicted_avg": 0.08, "actual_avg": 0.06, "count": 45}, ...]
```

### Dependencies
- Chart.js CDN loaded in base.html
- Existing endpoints: `/api/performance`, `/api/strategy-performance`, `/api/calibration/brier`

---

## RISK Page — Risk Dashboard

### Layout
```
┌──────────────────────────────────┬──────────────────────────────────┐
│  EXPOSURE                        │  KILL SWITCHES                   │
│  Positions: 3 / 10 max          │  ┌─────────────────────────────┐ │
│  Daily P&L: +$42.30             │  │ [●] ALL TRADING      active │ │
│  Daily Loss: -$12.50            │  │ [●] CRYPTO           active │ │
│  Max Loss:   -$50.00            │  │ [●] WEATHER          active │ │
│  ▓▓▓▓▓▓▓▓▓▓▓▓░░░░░░░░ 62%     │  └─────────────────────────────┘ │
│  Exposure:   $42 / $200 max     │                                   │
├──────────────────────────────────┼──────────────────────────────────┤
│  FEED HEALTH MATRIX              │  POSITION DETAIL                 │
│  Feed           Score  Age  St   │  ticker              dir  size  │
│  coinbase       ●1.00  0.2s ✓    │  KXBTCD-26MAR11-87K  YES  $5   │
│  binance_spot   ●0.75  1.2s ✓    │    prob:0.62 mkt:0.54 entry:87c│
│  binance_fut    ●1.00  0.3s ✓    │  KXBTCD-26MAR11-86K  NO   $3   │
│  deribit        ●0.25  8.1s !    │    prob:0.38 mkt:0.42 entry:42c│
│  kalshi_ws      ●1.00  0.5s ✓    │                                 │
│                                   │                                 │
│  Strategy Health                 │                                 │
│  crypto:  0.88  weather: 1.00    │                                 │
└──────────────────────────────────┴──────────────────────────────────┘
```

### Features
- **Exposure panel**: Position count vs max, daily P&L vs loss limit (visual bar), total exposure
- **Kill switches**: Status of all/crypto/weather kill switches from Rust `/api/state`
- **Feed health matrix**: Per-feed health score, last message age, staleness indicator
  - Color: 1.0=green, 0.75=amber, 0.25=red, 0.0=gray
  - Pulsing dot animation for healthy feeds
- **Position detail**: Each open position with model_prob, market_price, entry_price
- **Strategy health**: Aggregated crypto/weather health scores

### New API Endpoints
```python
GET /api/risk-summary
  → {
      "positions": [...],
      "position_count": 3, "max_positions": 10,
      "daily_pnl_cents": 4230, "daily_loss_cents": -1250,
      "max_daily_loss_cents": -5000,
      "exposure_cents": 4200, "max_exposure_cents": 20000,
      "kill_switches": {"all": false, "crypto": false, "weather": false},
      "feeds": [
        {"name": "coinbase", "score": 1.0, "age_ms": 200, "healthy": true},
        ...
      ],
      "crypto_health": 0.88, "weather_health": 1.0
    }
```
Data source: Proxy to Rust `/api/state` + `/health/detail`

## New/Modified Files
- `python/dashboard/templates/analytics.html` — ANAL page with Chart.js charts
- `python/dashboard/templates/risk.html` — RISK page
- `python/dashboard/app.py` — 4 new endpoints + 2 page routes

## Acceptance Criteria
- [ ] ANAL page shows 4 charts (Brier trend, calibration curve, drawdown, edge decay)
- [ ] Charts render via Chart.js with proper dark theme styling
- [ ] Strategy performance table paginated and sortable
- [ ] RISK page shows exposure bars, kill switches, feed health matrix
- [ ] Feed health dots pulse/animate based on score
- [ ] Position detail shows per-position model context
- [ ] Both pages keyboard-navigable (tab 4 and 5)
