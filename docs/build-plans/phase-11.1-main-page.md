# Phase 11.1 — MAIN Page (Live Trading View)

## Goal
Replace the current 2x2 index.html with a dense, information-rich live trading page. This is the default view — what you see when the terminal opens.

## Layout

```
┌─────────────────────────────────────────────────────────────────────┐
│                        [persistent chrome]                          │
├────────────────────────────┬────────────────────────────────────────┤
│  ACTIVE CONTRACTS          │  POSITIONS                            │
│  ┌────────────────────┐    │  ticker  dir  size  fill  upnl  st   │
│  │ KXBTCD-26MAR11-87K │    │  KXBT..  YES  $5    87c   +$2   FIL │
│  │ YES  P:62% E:8.2%  │    │  KXBT..  NO   $3    44c   -$1   FIL │
│  │ ▓▓▓▓▓▓▓░░░ MKT:54% │    │                                     │
│  │ ▓▓▓▓░░░░░░ K:12.1% │    ├────────────────────────────────────────┤
│  └────────────────────┘    │  DAILY SUMMARY              ▁▃▅▇▅▃▁  │
│  ┌────────────────────┐    │  date        sig  ord  W/L  P&L      │
│  │ KXBTCD-26MAR11-88K │    │  2026-03-11  45   8    6/2  +$12.30  │
│  │ NO   P:38% E:3.1%  │    │  2026-03-10  52   11   7/4  +$8.50   │
│  │ low_edge            │    │  2026-03-09  38   6    4/2  +$5.20   │
│  └────────────────────┘    │                                       │
├────────────────────────────┴────────────────────────────────────────┤
│  RECENT SIGNALS  ■■■■□□■■□□■■■□□□■■□□□□■□□□□□□□  12 fired / 30    │
│  age    ticker              dir   edge   kelly  status             │
│  12s    KXBTCD-26MAR11-87K  YES   8.2%   12.1%  FIRED             │
│  45s    KXBTCD-26MAR11-88K  —     3.1%   4.2%   low_edge          │
│  2m     KXBTCD-26MAR11-86K  NO    6.7%   9.8%   FIRED             │
└─────────────────────────────────────────────────────────────────────┘
```

**3-row layout**:
- Top-left: Active contract cards (current model state, vertical scroll)
- Top-right: Positions table + Daily summary with P&L sparkline
- Bottom full-width: Recent signals with heatmap strip

## Components

### Active Contracts Panel (top-left)
- Cards for each `model_state:*` from Redis
- Shows: ticker, direction, model_prob bar, edge bar with threshold, market price, minutes remaining
- Rejection reason if present (color-coded)
- Sorted by minutes_remaining ascending
- Live indicator (blinking dot) on panel header
- SSE-updated via `model_state` event

### Positions Panel (top-right upper)
- Table of open orders (filled + pending)
- Columns: Ticker, Direction, Size ($), Fill Price, Unrealized P&L, Status, Latency
- Unrealized P&L enriched from model state cache
- Color-coded: positive green, negative red
- Polling every 10s (or SSE if order events added)

### Daily Summary Panel (top-right lower)
- 7-day P&L sparkline (canvas, inline)
- Table: Date, Signals, Orders, W/L, Net P&L
- P&L color-coded
- Polling every 30s

### Recent Signals Panel (bottom full-width)
- Heatmap strip: last 30 signals as colored blocks (fired=green, low-edge=amber, rejected=dark)
- Table with sortable columns: Age, Ticker, Direction, Edge, Kelly, Status
- Age auto-updates every 10s with freshness coloring
- Polling every 5s

## New/Modified Files

### New: `python/dashboard/templates/main.html`
- Extends `base.html`
- Sets `{% block active_tab %}main{% endblock %}`
- Contains the 3-row grid layout with panel divs
- Inline `<script>` for MAIN-specific rendering (model cards, signals table, positions, summary)
- Sparkline canvas elements

### Modified: `python/dashboard/app.py`
- Update `/` route to render `main.html` instead of `index.html`
- Keep all existing API endpoints (they serve MAIN page data)
- Add `GET /api/crypto-state` — proxy to Rust binary's `/api/state` endpoint:
  ```python
  @app.get("/api/crypto-state")
  async def crypto_state():
      """Proxy to Rust binary's /api/state for crypto prices and feed status."""
      async with httpx.AsyncClient() as client:
          resp = await client.get(f"http://localhost:{settings.rust_port}/api/state")
          return resp.json()
  ```

## Data Flow
```
Redis model_state:*  ──→  /api/model-state  ──→  Active Contracts
Postgres orders      ──→  /api/positions    ──→  Positions Table
Postgres signals     ──→  /api/signals      ──→  Signals Table + Heatmap
Postgres daily_sum   ──→  /api/daily-summary──→  Daily Summary + Sparkline
NATS signals.live    ──→  SSE /api/events   ──→  Real-time signal push
Rust /api/state      ──→  /api/crypto-state ──→  Status bar BTC price
```

## Acceptance Criteria
- [ ] MAIN page renders with 3-row layout in base chrome
- [ ] Active contract cards show model state with probability/edge bars
- [ ] Positions table shows open orders with unrealized P&L
- [ ] Daily summary includes 7-day P&L sparkline
- [ ] Signal heatmap strip renders with color coding
- [ ] Signal table is sortable by all columns
- [ ] Age values auto-update every 10s
- [ ] Keyboard shortcuts (R=refresh, 1-6=tab switch) work
- [ ] SSE updates model state cards in real-time
