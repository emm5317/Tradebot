# Phase 11.4 — WEAT Page (Weather Station Dashboard)

## Layout
```
┌──────────────────────────────────┬──────────────────────────────────┐
│  STATION OVERVIEW                │  HRRR SKILL MATRIX               │
│  ┌──────┬──────┬──────┬──────┐  │  station  00  03  06  09  12 ... │
│  │ KORD │ KJFK │ KDEN │ KLAX │  │  KORD    .92 .88 .85 .91 .94    │
│  │ 34°F │ 41°F │ 28°F │ 62°F │  │  KJFK    .89 .85 .82 .88 .91    │
│  │ 3 ct │ 2 ct │ 1 ct │ 0 ct │  │  KDEN    .78 .75 .71 .80 .83    │
│  └──────┴──────┴──────┴──────┘  │  Color: green >0.85, amber, red  │
│  ┌──────┬──────┬──────┬──────┐  │                                   │
│  │ KIAH │ KATL │ KSFO │ ...  │  │                                   │
│  └──────┴──────┴──────┴──────┘  │                                   │
├──────────────────────────────────┼──────────────────────────────────┤
│  SETTLEMENT OUTCOMES (7d)        │  CALIBRATION BROWSER             │
│  date    station  max   min  ct  │  Station: [KORD ▾]              │
│  Mar 11  KORD     38°F  22°F 4   │  Month:   [March ▾]            │
│  Mar 11  KJFK     44°F  32°F 3   │                                 │
│  Mar 10  KORD     35°F  20°F 5   │  hour  sigma  bias   skill  n   │
│  Mar 10  KJFK     42°F  30°F 2   │  00    0.42   -0.3   0.92  120  │
│  Mar 10  KDEN     30°F  15°F 3   │  03    0.38   -0.5   0.88  115  │
│                                   │  06    0.45   -0.2   0.85  108  │
└──────────────────────────────────┴──────────────────────────────────┘
```

## Features

### Station Overview
- Grid of station cards (dynamically loaded from DB, not hardcoded)
- Each card: station code, current temp (from latest observation), active contract count
- Click station → selects it in calibration browser
- Card border color indicates HRRR skill quality

### HRRR Skill Matrix
- Heatmap table: stations × hours (0-23)
- Cell value: HRRR skill score (from station_calibration)
- Color intensity: green (>0.85), amber (0.7-0.85), red (<0.7)
- Hover: shows sigma, bias, sample_size

### Settlement Outcomes
- Last 7 days from `daily_settlement_summary`
- Columns: Date, Station, Final Max°F, Final Min°F, Contracts Settled
- Sortable, filterable by station

### Calibration Browser
- Dropdown selectors for station and month (dynamic from DB)
- Table: hour, sigma_10min, hrrr_bias_f, hrrr_skill, weight distribution, sample_size
- Replaces the old hardcoded 5-station calibration page

## New API Endpoints
```python
GET /api/station-summary
  → [{"station": "KORD", "latest_temp_f": 34.2, "active_contracts": 3,
      "avg_skill": 0.87}, ...]

GET /api/settlement-outcomes?days=7
  → [{"station": "KORD", "obs_date": "2026-03-11", "final_max_f": 38.0,
      "final_min_f": 22.0, "contracts_settled": 4}, ...]

GET /api/hrrr-skill-matrix
  → [{"station": "KORD", "hour": 0, "skill": 0.92, "sigma": 0.42,
      "bias": -0.3, "samples": 120}, ...]

GET /api/calibration/stations
  → ["KORD", "KJFK", "KDEN", "KLAX", "KIAH", "KATL", ...]
```

## New/Modified Files
- `python/dashboard/templates/weather.html` — WEAT page
- `python/dashboard/app.py` — 4 new endpoints + 1 page route

## Acceptance Criteria
- [ ] Station overview loads dynamically (not hardcoded stations)
- [ ] HRRR skill matrix renders as colored heatmap
- [ ] Settlement outcomes table shows last 7 days
- [ ] Calibration browser replaces old calibration page
- [ ] Station click cross-links to calibration browser
- [ ] Page keyboard-navigable (tab 6)
