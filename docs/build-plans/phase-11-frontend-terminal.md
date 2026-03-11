# Phase 11 — Bloomberg Terminal Dashboard

## Overview

Redesign the Tradebot dashboard from a simple 2x2 grid into a Bloomberg-style multi-page trading terminal. Dense, utilitarian, information-maximalist. Every pixel earns its place.

**Current state**: FastAPI + htmx + SSE on port 8050. Two pages (index, calibration), 2x2 grid panels, JetBrains Mono, dark theme with CRT scan lines, polling-based updates.

**Target state**: 6-page tabbed terminal with persistent status bar, SSE-driven updates, sparklines, keyboard navigation, and comprehensive analytics views.

---

## Architecture Decisions

### Keep FastAPI + htmx + SSE (no React rewrite)
- Already works, no build tooling needed
- htmx handles partial page updates well for this density
- SSE already wired for real-time push
- Bloomberg terminals themselves are server-rendered with partial updates
- Add lightweight JS for sparklines (`<canvas>`) and Chart.js (single CDN include) for time-series

### Design Language
- **Font**: IBM Plex Mono (more readable at 11px than JetBrains, distinctly "terminal")
- **Primary color**: Bloomberg amber (#ff8c00) for data values
- **Accents**: Green (#00c853) positive, Red (#ff1744) negative, Blue (#448aff) info
- **Background**: Near-black (#0c0c0c) workspace, slightly lifted panels (#141414)
- **Density**: 11px base, tight spacing, no decorative elements
- **No CRT scan lines** (too gimmicky for dense data)

### Page Structure

| Page | Key | Route | Purpose |
|------|-----|-------|---------|
| MAIN | 1 | `/` | Live trading view — positions, active signals, P&L ticker, feed status |
| SGNL | 2 | `/signals` | Full signal history, heatmap, rejection breakdown, filtering |
| EXEC | 3 | `/execution` | Order execution: fill rates, latency, slippage, order states |
| ANAL | 4 | `/analytics` | Brier trends, edge decay, calibration curves, drawdown chart |
| RISK | 5 | `/risk` | Exposure, position limits, kill switches, feed health matrix |
| WEAT | 6 | `/weather` | Station data, HRRR skill, settlement outcomes, calibration grid |

### Persistent Chrome (all pages)
```
┌──────────────────────────────────────────────────────────────────────┐
│ TRADEBOT  │ BTC $87,234  │ ●●●●○ FEEDS  │ PNL +$42.30  │ 14:32 UTC│
├──[MAIN]──[SGNL]──[EXEC]──[ANAL]──[RISK]──[WEAT]────────────────────┤
│                                                                      │
│                         (page content)                               │
│                                                                      │
├──────────────────────────────────────────────────────────────────────┤
│ PAPER │ 3 positions │ 12 sgnl/hr │ Brier 0.18 │ Lat 42ms │ v10.1   │
└──────────────────────────────────────────────────────────────────────┘
```

---

## Phased Implementation

### Phase 11.0 — Terminal Framework (foundation)
See: `phase-11.0-terminal-framework.md`

### Phase 11.1 — MAIN Page (live trading view)
See: `phase-11.1-main-page.md`

### Phase 11.2 — SGNL + EXEC Pages (signals & execution)
See: `phase-11.2-signals-execution.md`

### Phase 11.3 — ANAL + RISK Pages (analytics & risk)
See: `phase-11.3-analytics-risk.md`

### Phase 11.4 — WEAT Page (weather station)
See: `phase-11.4-weather-page.md`

---

## New API Endpoints

| Endpoint | Phase | Page | Data Source |
|----------|-------|------|-------------|
| `GET /api/crypto-state` | 11.1 | MAIN, RISK | Proxy to Rust `/api/state` |
| `GET /api/execution-stats` | 11.2 | EXEC | Aggregate from orders table |
| `GET /api/decision-breakdown` | 11.2 | SGNL | Aggregate from decision_log |
| `GET /api/edge-decay` | 11.3 | ANAL | Scatter from signals table |
| `GET /api/calibration-curve` | 11.3 | ANAL | From calibration table/view |
| `GET /api/risk-summary` | 11.3 | RISK | Proxy Rust `/api/state` + DB |
| `GET /api/station-summary` | 11.4 | WEAT | Aggregate station data |
| `GET /api/microstructure` | 11.2 | EXEC | From decision_log micro_* fields |

## File Changes Summary

### New Files
- `python/dashboard/templates/base.html` — shared layout (header, tabs, status bar)
- `python/dashboard/templates/main.html` — MAIN page content
- `python/dashboard/templates/signals.html` — SGNL page content
- `python/dashboard/templates/execution.html` — EXEC page content
- `python/dashboard/templates/analytics.html` — ANAL page content
- `python/dashboard/templates/risk.html` — RISK page content
- `python/dashboard/templates/weather.html` — WEAT page content
- `python/dashboard/static/terminal.css` — new design system
- `python/dashboard/static/terminal.js` — sparklines, keyboard nav, SSE helpers

### Modified Files
- `python/dashboard/app.py` — page routes, new API endpoints, SSE enhancements
- `python/dashboard/static/style.css` — deprecated (replaced by terminal.css)
- `python/dashboard/templates/index.html` — deprecated (replaced by base.html + main.html)
- `python/dashboard/templates/calibration.html` — deprecated (replaced by analytics.html)

### Preserved
- All existing API endpoints remain functional
- SSE endpoint `/api/events` enhanced but backward compatible
