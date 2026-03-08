-- 011_crypto_sources.sql: Crypto exchange tick data for shadow RTI estimation

CREATE TABLE IF NOT EXISTS crypto_ticks (
    source          TEXT NOT NULL,       -- 'coinbase', 'binance_spot', 'binance_futures', 'deribit'
    symbol          TEXT NOT NULL,       -- 'BTCUSD', 'BTCUSDT'
    price           REAL NOT NULL,
    bid             REAL,
    ask             REAL,
    funding_rate    REAL,
    dvol            REAL,
    observed_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (source, observed_at)
);

CREATE INDEX IF NOT EXISTS idx_crypto_ticks_source_time
    ON crypto_ticks (source, observed_at DESC);
