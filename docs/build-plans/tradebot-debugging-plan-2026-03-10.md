# Tradebot Multi-Module Debugging Plan (Kalshi + Context7 Aligned, March 10, 2026)

## Summary
Build a deterministic, end-to-end debugging workflow that isolates failures across infrastructure, Kalshi REST/WS integration, Rust execution, Python ingestion/evaluation, and observability.

This plan is explicitly aligned to current Kalshi docs and Context7 extracts, with special handling for near-term API changes scheduled for March 12, 2026 (fixed-point hard cut and related schema removals).

Baseline from repo validation:
1. Rust Kalshi websocket parser tests pass (7/7).
2. Python Kalshi history tests pass (6/6).
3. Existing risk: code paths still depend on legacy integer fields and `type` in order payloads, which are changelog-sensitive.

## Implementation Debug Plan (Decision-Complete)

### 1. Phase 0: Safe Debug Envelope (before any live probing)
- Enforce `PAPER_MODE=true` for all active debug runs.
- Use demo endpoints by default unless explicitly testing prod parity.
- Record one immutable baseline bundle per run:
  - service startup logs (`tradebot`, `collector`, `evaluator`, `contract-sync`)
  - `/health`, `/health/detail`, `/api/state`
  - SQL freshness snapshot (`docs/sql-reference.md` section 1 queries)
  - Redis key snapshot for `orderbook:*`, `crypto:*`, `feed:status:*`, `model_state:*`
- Success criteria: baseline artifacts exist and timestamps align within 2 minutes across all services.

### 2. Phase 1: Kalshi Contract-Compatibility Gate (highest priority)
- Validate auth/signing invariant against docs:
  - message must be `timestamp + METHOD + path_without_query`
  - required headers must be `KALSHI-ACCESS-KEY`, `KALSHI-ACCESS-SIGNATURE`, `KALSHI-ACCESS-TIMESTAMP`
- REST schema probe matrix (read-only first):
  - `GET /trade-api/v2/markets?status=open`
  - `GET /trade-api/v2/markets/{ticker}`
  - `GET /trade-api/v2/portfolio/orders` (auth)
- Order submission dry contract check:
  - verify request builder behavior when `type` is present vs omitted
  - explicitly test/record 400/409/429 paths and response body parsing
- WS schema probe:
  - subscribe flow + ack/error handling
  - parse `orderbook_snapshot`, `orderbook_delta`, `trade`, `ticker`
  - verify parsing works for `_dollars` + `_fp` and legacy fallback fields
- Hard-stop criteria:
  - any auth mismatch
  - any parser panic/deserialization failure on fixed-point-only payload
  - `POST /portfolio/orders` rejects payload shape used by runtime
- Deliverable: Kalshi Compatibility Report with pass/fail per endpoint/channel and exact failing field names.

### 3. Phase 2: Contract Universe and Ingestion Integrity (Python modules)
- Validate active-contract ingestion behavior:
  - compare `status=open` and `status=active` pulls (current code uses both)
  - detect duplicates or missing contracts in `contracts` table
- Validate market snapshot extraction:
  - ensure snapshot logic tolerates `_dollars` fields and missing legacy integer fields
  - verify `yes_price`/`no_price` assumptions vs actual market payload
- Validate settlement history ingestion:
  - verify `result`, `close_time`, `last_price` mappings against API payload reality
- Success criteria:
  - contracts in next 30m window are stable across 3 consecutive sync cycles
  - no ingestion exceptions, no silent drops from parse failures.

### 4. Phase 3: Real-Time Market Data Path (Rust WS -> Redis -> Python)
- Trace one ticker end-to-end:
  - WS message arrival
  - in-memory orderbook update
  - Redis flush (`orderbook:{ticker}`)
  - evaluator consumption
- Validate orderbook reciprocity logic:
  - NO bids -> implied YES asks transformation remains correct under fixed-point payloads
- Validate staleness behavior:
  - feed-health thresholds
  - stale key behavior and dashboard reflection
- Success criteria:
  - same ticker shows coherent mid/spread/depth across Rust logs, Redis, dashboard API.

### 5. Phase 4: Signal -> Risk Gate -> Order Lifecycle
- Trace one actionable signal through:
  - signal publish (`tradebot.signals`)
  - execution consumer deserialize
  - `check_risk` gate outcomes
  - order manager state transitions
  - DB `orders` persistence and reconciliation updates
- Explicitly test failure classes:
  - kill-switch block
  - stale feed block
  - cooldown/rate-limit block
  - Kalshi 400/401/409/429/5xx mapping
- Validate quantity semantics:
  - verify internal `size_cents` to API `count/count_fp` mapping is intentional and documented
  - detect unit mismatch (contracts vs cents budget)
- Success criteria:
  - every rejected signal has deterministic reason in logs/DB
  - every accepted signal reaches terminal order state or typed retry path.

### 6. Phase 5: Observability and Alert Fidelity
- Reconcile dashboard, Grafana, and DB truth:
  - `decision_log`, `feed_health_log`, `dead_letters`, `reconciliation_log`, `strategy_performance`
- Validate alert rules fire on controlled fault injection:
  - stale feed
  - no signals for 2h (simulated window)
  - high rejection rate
  - calibrator stale
- Success criteria:
  - each injected fault appears in DB + dashboard + expected alert channel.

### 7. Phase 6: Regression Suite and Exit Criteria
- Add/execute targeted regression packs:
  - auth/signature regression (path-with-query exclusion)
  - REST market/order schema drift tests (legacy removed fields)
  - WS payload replay tests (`_dollars/_fp` only frames)
  - order lifecycle error mapping tests
- Exit criteria:
  - zero critical compatibility failures
  - zero parser panics
  - deterministic rejection taxonomy
  - full artifact bundle attached for future incidents.

## Public APIs / Interfaces / Types
- External/public API changes: none required for runtime behavior in this debugging phase.
- Debug instrumentation additions (internal only):
  1. Structured compatibility logs for Kalshi request/response schema mismatches.
  2. Parser counters for legacy-vs-fixed-point field usage by endpoint/channel.
  3. One compatibility summary artifact per run (machine-readable JSON + human summary).

## Test Plan (Scenarios to Run)
1. Auth signing correctness with and without query params.
2. Fixed-point-only REST payload parsing for markets/orders.
3. Fixed-point-only WS payload replay for `orderbook_snapshot`, `orderbook_delta`, `ticker`, `trade`.
4. Order create payload shape validation (`type` present/absent).
5. 429 retry/backoff correctness with `Retry-After`.
6. End-to-end signal to order lifecycle including rejection paths.
7. Feed staleness plus alert propagation.
8. Contract sync consistency (`open` vs `active`) across repeated cycles.

## Assumptions and Defaults
1. Default debug target is demo environment plus paper trading.
2. Current date anchor for compatibility decisions: March 10, 2026.
3. Treat Kalshi changelog upcoming items for March 12, 2026 as imminent and gate release readiness against them now.
4. No BudgetBox skills are applied; available skills in this repo context are not relevant to Tradebot debugging.

## Sources Used
- Context7 library: `/openapi/kalshi_openapi_yaml` (OpenAPI schema extraction)
- Context7 library: `/websites/kalshi_websockets` (WebSocket docs extraction)
- Kalshi API Keys (signing + headers): https://docs.kalshi.com/getting_started/api_keys
- Kalshi WebSocket quickstart (channels, subscribe format, auth behavior): https://docs.kalshi.com/getting_started/quick_start_websockets
- Kalshi Fixed-Point Migration (legacy field removals, REST/WS replacements, dated changes): https://docs.kalshi.com/getting_started/fixed_point_migration
- Kalshi Rate Limits and Tiers: https://docs.kalshi.com/getting_started/rate_limits
- Kalshi order quickstart (current order examples): https://docs.kalshi.com/getting_started/quick_start_create_order
- Kalshi Changelog (upcoming changes including `POST /portfolio/orders` `type` removal, channel changes): https://docs.kalshi.com/changelog
