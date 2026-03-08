CREATE TABLE contract_rules (
    series_ticker         TEXT NOT NULL,
    market_ticker         TEXT PRIMARY KEY,
    contract_type         TEXT NOT NULL CHECK (
        contract_type IN ('crypto_binary', 'weather_max', 'weather_min')
    ),

    -- Settlement source
    settlement_source     TEXT NOT NULL,          -- 'cfb_rti', 'nws_cli_dsm'
    settlement_station    TEXT,                   -- ASOS station for weather, NULL for crypto
    settlement_tz         TEXT,                   -- IANA timezone, e.g. 'America/Chicago'

    -- Strike / threshold
    strike                REAL NOT NULL,          -- BTC price or temperature in °F

    -- Timing
    expiry_time           TIMESTAMPTZ NOT NULL,
    settlement_window_start TIMESTAMPTZ,          -- CFB RTI: start of 60s averaging window
    settlement_window_end   TIMESTAMPTZ,          -- CFB RTI: end of 60s averaging window
    day_boundary_start    TIMESTAMPTZ,            -- weather: local-standard-time day start
    day_boundary_end      TIMESTAMPTZ,            -- weather: local-standard-time day end

    -- Metadata
    underlying            TEXT,                   -- 'BTCUSD' or station code
    constituent_exchanges TEXT[],                 -- CFB RTI: ['coinbase', 'bitstamp', ...]

    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_rules_series ON contract_rules(series_ticker);
CREATE INDEX idx_rules_type ON contract_rules(contract_type);
CREATE INDEX idx_rules_expiry ON contract_rules(expiry_time);
CREATE INDEX idx_rules_station ON contract_rules(settlement_station);
