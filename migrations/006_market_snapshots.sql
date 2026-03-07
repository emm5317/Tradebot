CREATE TABLE market_snapshots (
    id                    BIGSERIAL,
    ticker                TEXT NOT NULL,
    yes_price             REAL NOT NULL,
    no_price              REAL NOT NULL,
    spread                REAL NOT NULL,
    best_bid              REAL,
    best_ask              REAL,
    bid_depth             INTEGER,
    ask_depth             INTEGER,
    minutes_to_settlement REAL NOT NULL,
    captured_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (id, captured_at)
);

SELECT create_hypertable('market_snapshots', 'captured_at', if_not_exists => TRUE);
CREATE INDEX idx_snapshots_ticker_time ON market_snapshots(ticker, captured_at);
