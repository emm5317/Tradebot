-- Performance indexes for dashboard, calibrator, and aggregator query patterns.

-- orders: signal_id FK join (used by dashboard, analytics, Brier scoring)
CREATE INDEX IF NOT EXISTS idx_orders_signal_id
    ON orders(signal_id) WHERE signal_id IS NOT NULL;

-- orders: outcome filtering (settlement queries, position counting)
CREATE INDEX IF NOT EXISTS idx_orders_outcome_created
    ON orders(outcome, created_at DESC);

-- orders: signal_type + time range (daily aggregation, execution stats)
CREATE INDEX IF NOT EXISTS idx_orders_signal_type_created
    ON orders(signal_type, created_at DESC) WHERE signal_type IS NOT NULL;

-- orders: order_state for dashboard execution stats (Phase 15 fix)
CREATE INDEX IF NOT EXISTS idx_orders_order_state
    ON orders(order_state) WHERE order_state IS NOT NULL;

-- signals: type + time range (aggregator daily performance, sweep queries)
CREATE INDEX IF NOT EXISTS idx_signals_type_created
    ON signals(signal_type, created_at DESC);

-- signals: acted_on filter (edge decay, calibration queries)
CREATE INDEX IF NOT EXISTS idx_signals_acted_on_type
    ON signals(signal_type, created_at DESC) WHERE acted_on = true;

-- calibration: dedup check in calibrator Job 2
CREATE INDEX IF NOT EXISTS idx_calibration_dedup
    ON calibration(ticker, signal_type, model_prob, settled_at);

-- decision_log: signal_type filtering for Grafana dashboards
CREATE INDEX IF NOT EXISTS idx_decision_log_signal_type
    ON decision_log(signal_type, created_at DESC);
