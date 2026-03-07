CREATE TABLE calibration (
    id             BIGSERIAL,
    ticker         TEXT NOT NULL,
    signal_type    TEXT NOT NULL,
    model_prob     REAL NOT NULL,
    market_price   REAL NOT NULL,
    actual_outcome BOOLEAN NOT NULL,
    prob_bucket    TEXT NOT NULL,
    sigma_used     REAL NOT NULL,
    settled_at     TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (id, settled_at)
);

SELECT create_hypertable('calibration', 'settled_at', if_not_exists => TRUE);
CREATE INDEX idx_cal_type_bucket ON calibration(signal_type, prob_bucket);

CREATE MATERIALIZED VIEW calibration_rolling
WITH (timescaledb.continuous) AS
SELECT
    signal_type,
    prob_bucket,
    time_bucket('1 day', settled_at) AS day,
    COUNT(*) AS total,
    SUM(CASE WHEN actual_outcome THEN 1 ELSE 0 END) AS wins,
    AVG(model_prob) AS avg_model_prob,
    AVG(CASE WHEN actual_outcome THEN 1.0 ELSE 0.0 END) AS actual_win_rate
FROM calibration
GROUP BY signal_type, prob_bucket, time_bucket('1 day', settled_at);

-- Refresh policy: update daily, covering the last 2 days, with a 1-hour lag
SELECT add_continuous_aggregate_policy('calibration_rolling',
    start_offset    => INTERVAL '2 days',
    end_offset      => INTERVAL '1 hour',
    schedule_interval => INTERVAL '1 day'
);
