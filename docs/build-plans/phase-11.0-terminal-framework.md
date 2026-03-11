# Phase 11.0 — Terminal Framework

## Goal
Build the shared chrome (header, tab bar, status bar) and design system that all 6 pages inherit. Replace the current standalone templates with a Jinja2 base template + per-page content blocks.

## Files

### New: `python/dashboard/templates/base.html`
Jinja2 base template with:
- **Top bar**: Logo, BTC price (SSE-updated), feed health dots, daily P&L, UTC clock
- **Tab bar**: 6 tabs (MAIN, SGNL, EXEC, ANAL, RISK, WEAT), keyboard shortcut hints, active state via `{% block active_tab %}`
- **Content area**: `{% block content %}` — filled by each page template
- **Status bar**: Paper/live mode, open positions count, signal rate, Brier score, avg latency, version
- SSE connection for status bar updates (BTC price, feed dots, P&L — shared across all pages)
- `<link>` to terminal.css, `<script>` for terminal.js

### New: `python/dashboard/static/terminal.css`
Complete design system:
- CSS custom properties (colors, spacing, typography)
- IBM Plex Mono from Google Fonts
- Layout: fixed header + tabs, scrollable content, fixed status bar
- Panel system: `.t-panel`, `.t-panel-header`, `.t-panel-body`
- Table system: `.t-table` with sticky headers, sort indicators, row hover
- Data coloring: `.t-positive`, `.t-negative`, `.t-amber`, `.t-muted`
- Direction: `.t-yes`, `.t-no`
- Bar charts: `.t-bar-track`, `.t-bar-fill`
- Sparkline canvas placeholder styles
- Feed dot indicators (green/amber/red pulsing)
- Responsive: single-column below 1200px
- Tab active/hover states
- Status bar styling

### New: `python/dashboard/static/terminal.js`
Shared JavaScript:
- `Terminal` namespace with init, SSE connection, keyboard handlers
- SSE client: connects to `/api/events`, dispatches to registered handlers
- Status bar updater: parses SSE `system_status` events, updates BTC/feeds/P&L/clock
- Keyboard navigation: `1-6` switches tabs via htmx navigation, `R` refreshes current page
- `Terminal.sparkline(canvas, data, opts)` — draws mini line chart on `<canvas>` element
- `Terminal.fetchJSON(url)` — shared fetch helper with error handling
- `Terminal.ageText(date)` / `Terminal.ageClass(date)` — shared age formatting
- `Terminal.formatPnl(cents)` — shared P&L formatting with color class
- `Terminal.formatPct(decimal)` — percentage formatting
- `Terminal.sortTable(tableId, col, dir)` — shared sort helper

### Modified: `python/dashboard/app.py`
- Add page routes: `/signals`, `/execution`, `/analytics`, `/risk`, `/weather`
  - Each returns `templates.TemplateResponse("page.html", ctx)` (page extends base.html)
- Add `GET /api/system-status` endpoint for status bar data:
  ```python
  {
    "btc_price": 87234.50,        # from Redis crypto:coinbase
    "feeds": {                     # from Redis feed:status:*
      "coinbase": {"score": 1.0, "age_ms": 234},
      "binance_spot": {"score": 0.75, "age_ms": 1200},
      ...
    },
    "daily_pnl_cents": 4230,      # from strategy_performance today
    "positions_count": 3,          # from orders WHERE status='filled'
    "signal_rate_1h": 12,          # COUNT signals last hour
    "brier_score": 0.18,           # latest from strategy_performance
    "avg_latency_ms": 42,          # AVG from orders last hour
    "paper_mode": true             # from settings or Rust API
  }
  ```
- Enhance SSE `/api/events` to include `system_status` event type (sent every 5s)

## Implementation Order

1. Create `terminal.css` (design system, no functional dependencies)
2. Create `terminal.js` (shared utilities)
3. Create `base.html` (layout template)
4. Add page routes to `app.py` + system-status endpoint
5. Create `main.html` stub (extends base, minimal content to verify framework)
6. Test: all 6 tab routes render, status bar updates via SSE, keyboard nav works

## Acceptance Criteria
- [ ] All 6 routes return 200 with base chrome
- [ ] Status bar shows live BTC price, feed dots, P&L
- [ ] Tab switching works via click and keyboard (1-6)
- [ ] UTC clock ticks in status bar
- [ ] SSE connection established on page load, reconnects on drop
- [ ] Old `/` route redirects or serves new MAIN page
- [ ] Old `/calibration` route redirects to `/analytics`
