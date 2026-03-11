# Phase 11.2 — SGNL + EXEC Pages

## SGNL Page — Signal History & Analysis

### Layout
```
┌─────────────────────────────────────────────────────────────────────┐
│  FILTERS  [All ▾] [Weather ▾] [Crypto ▾]  [Fired ▾] [24h ▾]      │
├──────────────────────────────────┬──────────────────────────────────┤
│  SIGNAL HEATMAP (expanded)       │  REJECTION BREAKDOWN            │
│  ■■■■□□■■□□■■■□□□■■□□□□■□□□□□□□  │  ┌──────────────────┐          │
│  ■■■■□□■■□□■■■□□□■■□□□□■□□□□□□□  │  │ low_edge    42%  │ ▓▓▓▓▓▓▓ │
│  ■■■■□□■■□□■■■□□□■■□□□□■□□□□□□□  │  │ no_position 28%  │ ▓▓▓▓▓   │
│                                   │  │ risk_limit  18%  │ ▓▓▓      │
│                                   │  │ stale_feed  12%  │ ▓▓       │
│                                   │  └──────────────────┘          │
├──────────────────────────────────┴──────────────────────────────────┤
│  SIGNAL LOG (paginated, sortable, filterable)                      │
│  time     ticker              type    dir  prob   mkt    edge  ... │
│  14:32:07 KXBTCD-26MAR11-87K  crypto  YES  0.62   0.54   8.2% ... │
│  14:31:55 KXBTCD-26MAR11-88K  crypto  —    0.38   0.41   3.1% ... │
│  14:30:12 KTEMP-ORD-H-38      weathr  YES  0.71   0.65   6.0% ... │
└─────────────────────────────────────────────────────────────────────┘
```

### Features
- **Filter bar**: Signal type (all/weather/crypto), status (all/fired/rejected), time range (1h/6h/24h/7d)
- **Expanded heatmap**: Multi-row, last 200 signals, tooltip on hover
- **Rejection breakdown**: Horizontal bar chart of rejection reasons (last 24h), counts + percentages
- **Signal log**: Full table with pagination (50 per page), sortable columns
  - Columns: Time, Ticker, Type, Direction, Model Prob, Market Price, Edge, Kelly, Minutes, Status
  - Rejection reason shown inline with color coding
  - Click row to expand: observation_data JSONB, model_components

### New API Endpoints
```python
GET /api/signals?limit=50&offset=0&signal_type=crypto&acted_on=true&hours=24
GET /api/decision-breakdown?hours=24
  → [{"reason": "low_edge", "count": 42, "pct": 0.42}, ...]
```

---

## EXEC Page — Execution Quality

### Layout
```
┌──────────────────────────────────┬──────────────────────────────────┐
│  EXECUTION METRICS               │  LATENCY DISTRIBUTION           │
│  Fill rate:      87.2%           │  ▁▂▃▅▇█▇▅▃▂▁                   │
│  Avg latency:    42ms            │  P50: 38ms  P95: 112ms          │
│  Avg slippage:   -0.3c           │  P99: 234ms                     │
│  Orders today:   11              │                                  │
│  Cancel rate:    8.1%            │                                  │
├──────────────────────────────────┼──────────────────────────────────┤
│  ORDER STATE FLOW                │  MICROSTRUCTURE ADJUSTMENTS     │
│  Pending → Submit → Ack → Fill   │  Component     Last    Avg 1h   │
│     11  →   11   →  10  →  9     │  trade_flow    +0.012  +0.008   │
│                    →  Rej: 1      │  spread_adj    -0.005  -0.003   │
│                    →  Cancel: 1   │  depth_adj     +0.003  +0.002   │
│                                   │  vwap_signal   +0.008  +0.006   │
│                                   │  momentum      -0.002  +0.001   │
│                                   │  vol_surge     +0.000  +0.000   │
├──────────────────────────────────┴──────────────────────────────────┤
│  ORDER LOG (paginated)                                              │
│  time     ticker              dir  size  fill  status  lat   pnl   │
│  14:32:07 KXBTCD-26MAR11-87K  YES  $5    87c   filled  38ms  —    │
│  14:28:43 KXBTCD-26MAR11-86K  NO   $3    42c   filled  45ms  +$2  │
└─────────────────────────────────────────────────────────────────────┘
```

### Features
- **Execution metrics**: Fill rate, avg latency, slippage, cancel rate (computed from orders table)
- **Latency histogram**: Canvas-drawn histogram of order latencies (last 24h)
- **Order state flow**: Sankey-style flow from Pending → terminal states with counts
- **Microstructure adjustments**: Last and 1h-average of micro_* fields from decision_log
- **Order log**: Full order history with pagination, sortable

### New API Endpoints
```python
GET /api/execution-stats?hours=24
  → {"fill_rate": 0.872, "avg_latency_ms": 42, "avg_slippage_cents": -0.3,
     "total_orders": 11, "cancel_rate": 0.081,
     "state_counts": {"filled": 9, "cancelled": 1, "rejected": 1, "pending": 0},
     "latency_histogram": [0, 2, 5, 8, 12, 8, 5, 3, 1, 0]}

GET /api/microstructure?hours=1
  → [{"component": "trade_flow", "last": 0.012, "avg": 0.008}, ...]
```

## New/Modified Files
- `python/dashboard/templates/signals.html` — SGNL page
- `python/dashboard/templates/execution.html` — EXEC page
- `python/dashboard/app.py` — add 4 new endpoints + 2 page routes

## Acceptance Criteria
- [ ] SGNL page renders with filter bar, heatmap, rejection chart, signal log
- [ ] Filters update signal log via htmx partial swap
- [ ] Rejection breakdown shows horizontal bar chart
- [ ] EXEC page shows fill rate, latency histogram, state flow, micro adjustments
- [ ] Order log paginated and sortable
- [ ] Both pages keyboard-navigable (tab 2 and 3)
