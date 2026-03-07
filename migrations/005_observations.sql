CREATE TABLE observations (
    id            BIGSERIAL,
    source        TEXT NOT NULL,
    station       TEXT,
    observed_at   TIMESTAMPTZ NOT NULL,
    temperature_f REAL,
    wind_speed_kts REAL,
    wind_gust_kts REAL,
    precip_inch   REAL,
    btc_spot      REAL,
    btc_vol_30m   REAL,
    raw_data      JSONB,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (id, observed_at)
);

SELECT create_hypertable('observations', 'observed_at', if_not_exists => TRUE);
CREATE INDEX idx_obs_station_time ON observations(station, observed_at);
CREATE INDEX idx_obs_source ON observations(source);
