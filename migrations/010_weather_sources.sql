-- 010_weather_sources.sql: METAR observations and HRRR forecasts for weather fair-value engine

-- METAR observations with 6-hourly max/min groups (feed into NWS CLI settlement)
CREATE TABLE IF NOT EXISTS metar_observations (
    station         TEXT NOT NULL,
    observed_at     TIMESTAMPTZ NOT NULL,
    temp_c          REAL,
    dewpoint_c      REAL,
    wind_speed_kts  REAL,
    wind_gust_kts   REAL,
    altimeter_inhg  REAL,
    visibility_mi   REAL,
    wx_string       TEXT,
    max_temp_6hr_c  REAL,           -- 1xxxx group (6-hr max)
    min_temp_6hr_c  REAL,           -- 2xxxx group (6-hr min)
    max_temp_24hr_c REAL,           -- 4xxxx group (24-hr max, part 1)
    min_temp_24hr_c REAL,           -- 4xxxx group (24-hr min, part 2)
    raw_metar       TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (station, observed_at)
);

CREATE INDEX IF NOT EXISTS idx_metar_station_time
    ON metar_observations (station, observed_at DESC);

-- HRRR forecast snapshots (15-min resolution, updated hourly)
CREATE TABLE IF NOT EXISTS hrrr_forecasts (
    station         TEXT NOT NULL,
    forecast_time   TIMESTAMPTZ NOT NULL,   -- valid time of forecast
    run_time        TIMESTAMPTZ NOT NULL,    -- model initialization time
    temp_2m_f       REAL,                    -- 2m temperature in Fahrenheit
    temp_2m_c       REAL,                    -- 2m temperature in Celsius
    wind_10m_kts    REAL,                    -- 10m wind speed
    precip_mm       REAL,                    -- precipitation
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (station, forecast_time, run_time)
);

CREATE INDEX IF NOT EXISTS idx_hrrr_station_forecast
    ON hrrr_forecasts (station, forecast_time DESC);

-- Running daily max/min tracking for weather settlement
CREATE TABLE IF NOT EXISTS weather_daily_extremes (
    station         TEXT NOT NULL,
    obs_date        DATE NOT NULL,
    tz              TEXT NOT NULL,           -- IANA timezone used for day boundaries
    running_max_f   REAL,
    running_min_f   REAL,
    max_locked      BOOLEAN DEFAULT FALSE,   -- strike already exceeded
    min_locked      BOOLEAN DEFAULT FALSE,
    obs_count       INTEGER DEFAULT 0,
    last_updated    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (station, obs_date)
);
