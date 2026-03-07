-- Kalshi contracts / markets
CREATE TABLE IF NOT EXISTS contracts (
    id            SERIAL PRIMARY KEY,
    ticker        TEXT NOT NULL UNIQUE,
    title         TEXT NOT NULL,
    category      TEXT,
    close_time    TIMESTAMPTZ NOT NULL,
    settlement    REAL,
    created_at    TIMESTAMPTZ DEFAULT now()
);

CREATE INDEX idx_contracts_ticker ON contracts(ticker);
CREATE INDEX idx_contracts_close_time ON contracts(close_time);
