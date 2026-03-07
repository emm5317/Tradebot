CREATE TABLE signals (
    id                BIGSERIAL PRIMARY KEY,
    ticker            TEXT NOT NULL REFERENCES contracts(ticker),
    signal_type       TEXT NOT NULL CHECK (signal_type IN ('weather', 'crypto')),
    direction         TEXT NOT NULL CHECK (direction IN ('yes', 'no')),
    model_prob        REAL NOT NULL,
    market_price      REAL NOT NULL,
    edge              REAL NOT NULL,
    kelly_fraction    REAL NOT NULL,
    minutes_remaining REAL NOT NULL,
    observation_data  JSONB,
    acted_on          BOOLEAN NOT NULL DEFAULT false,
    rejection_reason  TEXT,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_signals_created ON signals(created_at);
CREATE INDEX idx_signals_ticker ON signals(ticker);

SELECT create_hypertable('signals', 'created_at', if_not_exists => TRUE);
