# Phase 3 — Event-Driven Evaluation

**Timeline:** Weeks 6–10
**Risk:** MEDIUM
**Goal:** Replace timer-based polling with event-driven evaluation. Crypto evaluates on every price update; weather evaluates on data arrival.

**Foundation from Phase 1 (already implemented):**
- `CryptoState` (RwLock) — canonical state, updated by all 4 feeds at 2Hz each
- `crypto_fv::compute_crypto_fair_value()` — full N(d2) + basis + funding + Kelly
- Python crypto signals already advisory (`tradebot.advisory.crypto`)
- Feeds dual-write to CryptoState + Redis

---

## 3.1 Split Weather/Crypto Evaluators

### Problem
Both weather and crypto share the same 10s evaluation loop in Python. Crypto needs sub-second response; weather can tolerate 60s cycles.

### Implementation

**Separate processes/tasks:**
- Crypto: event-driven in Rust (Phase 3.2)
- Weather: event-triggered in Python (Phase 3.3), retain 10s minimum cycle

**Python evaluator daemon changes:**
- Remove crypto evaluator registration (already advisory-only since Phase 1.3)
- Rename to `WeatherEvaluationDaemon`
- Keep 10s cycle but add event triggers
- Crypto evaluator can be fully removed once Phase 3.2 is validated

---

## 3.2 Event-Driven Crypto Evaluation in Rust

### Problem
The 10s polling cycle means crypto signals are always 0–10s stale. For a market that settles every 60 seconds, this is 10-17% of the remaining time.

### Implementation

> **Phase 1 foundation:** `CryptoState::snapshot()` and `crypto_fv::compute_crypto_fair_value()` already exist. This phase adds the event-driven trigger and contract discovery — the computation pipeline is ready.

**Trigger:** Evaluate on CryptoState update (any feed price change)

**Debounce:** Minimum 500ms between evaluations per ticker to prevent flooding

**Flow:**
1. Feed updates CryptoState (already implemented — 4 feeds write at 2Hz)
2. Add `tokio::sync::watch` channel to CryptoState — sender notifies on every write
3. Evaluation task receives notification, calls `crypto_state.snapshot()`
4. For each active crypto contract near settlement:
   - Call `crypto_fv::compute_crypto_fair_value(&snapshot, strike, minutes_remaining)`
   - Compare to last-known orderbook mid (from `OrderbookManager`)
   - If edge > threshold → generate signal directly (bypassing NATS for crypto)
5. Submit to order state machine (Phase 2) directly

**Contract discovery:**
- Periodically (every 60s) refresh active crypto contracts from DB
- Cache in-memory with expiry

**Key architectural note:** Crypto signals in this phase are generated and executed entirely within Rust — no NATS round-trip. The advisory comparison from Phase 1.3 continues to validate Rust vs Python agreement.

---

## 3.3 Weather Event Triggers

### Problem
Weather data arrives sporadically (METAR every ~60 min, HRRR every 15 min). Evaluating every 10s wastes cycles when no new data exists.

### Implementation

**Event triggers:**
1. **METAR arrival** → re-evaluate all contracts for that station
2. **HRRR refresh** → re-evaluate all contracts
3. **Running max/min lock** → immediately lock probability, publish signal
4. **Orderbook change** → re-evaluate if within entry window
5. **Fallback timer** → evaluate every 60s regardless (catch missed events)

**NATS events:**
- `tradebot.events.metar.{station}` → published by collector on new METAR
- `tradebot.events.hrrr.refresh` → published by collector on new HRRR data
- `tradebot.events.orderbook.{ticker}` → published by Rust on significant price change

---

## 3.4 Contract Lifecycle State Machine

### Problem
Contracts are treated as flat records. No formal lifecycle tracking.

### Implementation

```
Discovery → Active → InEntryWindow → Evaluated → Positioned → NearExpiry → Settled
```

**States:**
- `Discovery`: Contract found in DB, not yet near settlement
- `Active`: Within 30 min of settlement, eligible for evaluation
- `InEntryWindow`: Within entry window (8-18 min weather, 5-15 min crypto)
- `Evaluated`: Fair value computed, signal published
- `Positioned`: We hold a position in this contract
- `NearExpiry`: <2 min to settlement, exit-only mode
- `Settled`: Contract resolved, calculate P&L

Track in-memory with periodic DB sync.

---

## 3.5 Throttle and Cooldown Overhaul

### Problem
Current cooldown is a flat 300s per ticker. This is too conservative for crypto (60s settlement cycle) and too aggressive for weather near lock events.

### Implementation

**Strategy-specific cooldowns:**
- Crypto: 30s cooldown per ticker (contracts settle every 60s)
- Weather: 120s cooldown per ticker (but bypass on lock event)

**Signal priority:**
- `lock_detection` > `new_data` > `reeval` > `timer`
- Higher priority bypasses cooldown

**Edge decay tracking:**
- If edge is shrinking across consecutive evaluations → increase cooldown
- If edge is growing → decrease cooldown (adaptive)

---

## Verification Checklist

- [ ] Crypto signals generated within 500ms of price update
- [ ] Weather signals generated within 5s of METAR arrival
- [ ] Contract lifecycle tracked correctly through all states
- [ ] Cooldowns respect per-strategy configuration
- [ ] No signal flooding under rapid price changes
- [ ] Timer fallback catches missed events
