-- Phase 9: Decision audit log + feed health log for Grafana observability.
-- Written by both Rust crypto evaluator and Python weather evaluator.

CREATE TABLE IF NOT EXISTS decision_log (
    id              BIGSERIAL PRIMARY KEY,
    ticker          TEXT NOT NULL,
    signal_type     TEXT NOT NULL,
    source          TEXT NOT NULL,
    outcome         TEXT NOT NULL,
    rejection_reason TEXT,
    model_prob      REAL,
    market_price    REAL,
    edge            REAL,
    adjusted_edge   REAL,
    direction       TEXT,
    minutes_remaining REAL,
    confidence      REAL,
    micro_total     REAL,
    micro_trade     REAL,
    micro_spread    REAL,
    micro_depth     REAL,
    micro_vwap      REAL,
    micro_momentum  REAL,
    micro_vol_surge REAL,
    signal_id       BIGINT,
    eval_latency_ms REAL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_decision_log_created
    ON decision_log (created_at DESC);

CREATE INDEX IF NOT EXISTS idx_decision_log_ticker_time
    ON decision_log (ticker, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_decision_log_outcome
    ON decision_log (outcome, signal_type, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_decision_log_rejection
    ON decision_log (rejection_reason, created_at DESC)
    WHERE rejection_reason IS NOT NULL;

-- Feed health snapshots (written every 60s by Rust crypto evaluator)
CREATE TABLE IF NOT EXISTS feed_health_log (
    id              BIGSERIAL PRIMARY KEY,
    feed_name       TEXT NOT NULL,
    score           REAL NOT NULL,
    last_msg_age_ms REAL,
    is_stale        BOOLEAN NOT NULL DEFAULT false,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_feed_health_log_time
    ON feed_health_log (created_at DESC);

CREATE INDEX IF NOT EXISTS idx_feed_health_log_feed
    ON feed_health_log (feed_name, created_at DESC);
