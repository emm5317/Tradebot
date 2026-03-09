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

-- Calibration rolling view (regular, not continuous aggregate to avoid txn issues)
CREATE VIEW calibration_rolling AS
SELECT
    signal_type,
    prob_bucket,
    date_trunc('day', settled_at) AS day,
    COUNT(*) AS total,
    SUM(CASE WHEN actual_outcome THEN 1 ELSE 0 END) AS wins,
    AVG(model_prob) AS avg_model_prob,
    AVG(CASE WHEN actual_outcome THEN 1.0 ELSE 0.0 END) AS actual_win_rate
FROM calibration
GROUP BY signal_type, prob_bucket, date_trunc('day', settled_at);
