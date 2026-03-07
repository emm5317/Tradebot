# BE-6: Settlement Scanner + Latency Instrumentation

**Dependencies**: BE-2 (Kalshi WebSocket), BE-5 (execution engine)
**Blocks**: BE-7 (metrics endpoint), BE-9 (integration)
**Language**: Rust

---

## Overview

Two critical infrastructure modules: (1) detecting when contracts enter the tradeable window, and (2) measuring every millisecond of the critical path.

---

## BE-6.1: Settlement Window Scanner

### Deliverable
`rust/src/signal/scanner.rs`

### Specification

```rust
pub struct SettlementScanner {
    // Sorted by settlement time — O(log n) range queries
    contracts: RwLock<BTreeMap<DateTime<Utc>, Vec<ContractInfo>>>,
    nats: async_nats::Client,
    config: ScannerConfig,
}

pub struct ScannerConfig {
    pub window_start_minutes: f64,  // 18 min before settlement
    pub window_end_minutes: f64,    // 5 min before settlement (stop scanning)
    pub rescan_interval: Duration,  // 1 second
}

impl SettlementScanner {
    /// Called on every Kalshi WS market update
    pub fn on_market_update(&self, market: &Market) {
        // Insert/update in BTreeMap
    }

    /// Main scan loop — checks every second what's in the window
    pub async fn run(&self) {
        loop {
            let now = Utc::now();
            let window_start = now + Duration::minutes(self.config.window_end_minutes as i64);
            let window_end = now + Duration::minutes(self.config.window_start_minutes as i64);

            // BTreeMap range query: O(log n + k) where k = contracts in window
            let in_window: Vec<_> = self.contracts.read()
                .range(window_start..=window_end)
                .flat_map(|(_, contracts)| contracts)
                .collect();

            for contract in in_window {
                self.request_evaluation(contract).await;
            }

            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    /// Send scan request to Python via NATS request/reply
    async fn request_evaluation(&self, contract: &ContractInfo) {
        // NATS request/reply pattern — Python responds with Signal or None
        let request = ScanRequest {
            ticker: contract.ticker.clone(),
            threshold: contract.threshold,
            station: contract.station.clone(),
            settlement_time: contract.settlement_time,
        };

        self.nats.publish(
            "tradebot.scan-requests",
            serde_json::to_vec(&request)?.into()
        ).await?;
    }
}
```

### Improvement over original plan
- **`BTreeMap` with range queries** — O(log n) instead of scanning all markets
- **NATS request/reply** — cleaner than dual Redis streams
- **1-second scan interval** — much better than 30-second Python polling (and the BTreeMap makes this cheap)

### Verification
- Subscribe to Kalshi WS, wait for contract to enter 18-minute window
- Verify scan request published to NATS within 1 second
- Verify contracts outside window are not scanned

---

## BE-6.2: Latency Instrumentation

### Deliverable
`rust/src/metrics/latency.rs`

### Specification

```rust
pub struct LatencyTracker {
    stages: DashMap<String, VecDeque<StageTimings>>,  // per-ticker
    aggregates: RwLock<LatencyAggregates>,
}

pub struct StageTimings {
    pub signal_received: Instant,
    pub deserialized: Instant,
    pub risk_checked: Instant,
    pub sized: Instant,
    pub http_sent: Instant,
    pub http_received: Instant,
}

pub struct LatencyAggregates {
    pub deser_p50: Duration,
    pub deser_p95: Duration,
    pub deser_p99: Duration,
    pub risk_p50: Duration,
    pub risk_p95: Duration,
    pub risk_p99: Duration,
    pub total_p50: Duration,
    pub total_p95: Duration,
    pub total_p99: Duration,
    pub last_updated: Instant,
}

impl LatencyTracker {
    pub fn record(&self, ticker: &str, timings: StageTimings);
    pub fn aggregates(&self) -> LatencyAggregates;
}
```

### What gets timed
| Stage | Start | End | Target |
|-------|-------|-----|--------|
| Deserialization | Signal arrives from NATS | Signal struct created | < 1ms |
| Risk check | Signal struct | Approval/rejection | < 1ms |
| Sizing | Approval | Size computed | < 1ms |
| Network | HTTP request sent | HTTP response received | < 200ms |
| **Total** | Signal arrives | Fill confirmed | **< 250ms** |

### Output
- Every order logged with full timing breakdown
- Per-minute aggregates stored in memory (last 60 minutes)
- Exposed via `GET /api/metrics` for the UI
- Optional Prometheus export (behind `prometheus` feature flag)

### Crates
- `metrics` — metrics facade
- `metrics-exporter-prometheus` — optional Prometheus export
- `tracing` — spans with timing (built-in with `tracing-subscriber`)

### Verification
- Place 10 paper orders, verify latency breakdown logged for each
- Verify total (excluding network) < 50ms
- `GET /api/metrics` returns valid JSON with percentiles

---

## Acceptance Criteria (BE-6 Complete)

- [ ] Scanner detects contracts entering 18-minute window within 1 second
- [ ] Scan requests published to NATS for Python evaluation
- [ ] BTreeMap range queries handle 1000+ active markets efficiently
- [ ] Every order has full latency breakdown logged
- [ ] Aggregated percentiles available via `/api/metrics`
- [ ] Total internal latency (signal → HTTP send) < 50ms
