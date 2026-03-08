-- Phase 4.5: Station-specific calibration for weather model.
-- Stores per-(station, month, hour) parameters for sigma, HRRR bias/skill,
-- rounding bias, and ensemble weights.

CREATE TABLE IF NOT EXISTS station_calibration (
    station        TEXT NOT NULL,
    month          INT NOT NULL,
    hour           INT NOT NULL,
    sigma_10min    REAL,
    hrrr_bias_f    REAL DEFAULT 0.0,
    hrrr_rmse_f    REAL,
    hrrr_skill     REAL DEFAULT 0.5,
    rounding_bias  REAL DEFAULT 0.0,
    weight_physics REAL DEFAULT 0.45,
    weight_hrrr    REAL DEFAULT 0.25,
    weight_trend   REAL DEFAULT 0.20,
    weight_climo   REAL DEFAULT 0.10,
    sample_size    INT DEFAULT 0,
    updated_at     TIMESTAMPTZ DEFAULT now(),
    PRIMARY KEY (station, month, hour)
);
