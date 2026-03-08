-- Paper trade logging table (Phase 0.3)
-- Records all signals that would have been executed in paper mode
-- with full signal parameters for backtesting and analysis.

CREATE TABLE paper_trades (
    id              BIGSERIAL PRIMARY KEY,
    ticker          TEXT NOT NULL,
    direction       TEXT NOT NULL CHECK (direction IN ('yes', 'no')),
    action          TEXT NOT NULL CHECK (action IN ('buy', 'sell')),
    size_cents      INTEGER NOT NULL,
    model_prob      REAL NOT NULL,
    market_price    REAL NOT NULL,
    edge            REAL NOT NULL,
    kelly_fraction  REAL NOT NULL,
    signal_type     TEXT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_paper_trades_created ON paper_trades(created_at);
CREATE INDEX idx_paper_trades_ticker ON paper_trades(ticker);
