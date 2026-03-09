-- Phase 7: Rolling calibration metrics computed by calibration agent.

CREATE TABLE IF NOT EXISTS calibration_metrics (
    id              BIGSERIAL PRIMARY KEY,
    strategy        TEXT NOT NULL,
    station         TEXT,
    hour            SMALLINT,
    month           SMALLINT,
    period_start    DATE NOT NULL,
    period_end      DATE NOT NULL,
    brier_score     REAL,
    avg_predicted   REAL,
    avg_actual      REAL,
    signal_count    INTEGER NOT NULL DEFAULT 0,
    avg_slippage    REAL,
    p95_slippage    REAL,
    avg_edge_realized REAL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_cal_metrics_strategy_period
    ON calibration_metrics (strategy, period_end DESC);
