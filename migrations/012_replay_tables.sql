-- 012_replay_tables.sql: Raw event capture and model evaluation history

-- Raw WS events for orderbook replay
CREATE TABLE IF NOT EXISTS kalshi_book_events (
    id              BIGSERIAL,
    ticker          TEXT NOT NULL,
    event_type      TEXT NOT NULL,       -- 'snapshot', 'delta', 'trade', 'ticker'
    payload         JSONB NOT NULL,
    received_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (id)
);

CREATE INDEX IF NOT EXISTS idx_book_events_ticker_time
    ON kalshi_book_events (ticker, received_at DESC);

-- Model evaluation snapshots for replay and source attribution
CREATE TABLE IF NOT EXISTS model_evaluations (
    id              BIGSERIAL,
    ticker          TEXT NOT NULL,
    signal_type     TEXT NOT NULL,       -- 'weather' or 'crypto'
    model_prob      REAL,
    market_price    REAL,
    edge            REAL,
    direction       TEXT,
    inputs          JSONB,              -- full input snapshot
    components      JSONB,              -- model component probabilities
    confidence      REAL,
    acted_on        BOOLEAN DEFAULT FALSE,
    evaluated_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (id)
);

CREATE INDEX IF NOT EXISTS idx_model_evals_ticker_time
    ON model_evaluations (ticker, evaluated_at DESC);

CREATE INDEX IF NOT EXISTS idx_model_evals_signal_type
    ON model_evaluations (signal_type, evaluated_at DESC);
