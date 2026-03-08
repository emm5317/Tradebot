-- Phase 5.1: Per-strategy performance analytics.
-- Separates weather and crypto P&L tracking for independent evaluation.

CREATE TABLE IF NOT EXISTS strategy_performance (
    id                BIGSERIAL PRIMARY KEY,
    strategy          TEXT NOT NULL,  -- 'weather', 'crypto'
    date              DATE NOT NULL,
    signals_generated INTEGER NOT NULL DEFAULT 0,
    signals_executed  INTEGER NOT NULL DEFAULT 0,
    win_count         INTEGER NOT NULL DEFAULT 0,
    loss_count        INTEGER NOT NULL DEFAULT 0,
    realized_pnl_cents INTEGER NOT NULL DEFAULT 0,
    avg_edge          REAL,
    avg_kelly         REAL,
    brier_score       REAL,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(strategy, date)
);

CREATE INDEX IF NOT EXISTS idx_strategy_performance_date
    ON strategy_performance(date DESC);
CREATE INDEX IF NOT EXISTS idx_strategy_performance_strategy
    ON strategy_performance(strategy, date DESC);

-- Dead letters table (Phase 5.6, created early for forward compatibility)
CREATE TABLE IF NOT EXISTS dead_letters (
    id          BIGSERIAL PRIMARY KEY,
    subject     TEXT NOT NULL,
    payload     BYTEA,
    error       TEXT,
    source      TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_dead_letters_created
    ON dead_letters(created_at DESC);

-- Reconciliation audit log (Phase 5.4)
CREATE TABLE IF NOT EXISTS reconciliation_log (
    id              BIGSERIAL PRIMARY KEY,
    discrepancy     TEXT NOT NULL,  -- 'missing_local', 'missing_exchange', 'quantity_mismatch'
    ticker          TEXT NOT NULL,
    exchange_qty    INTEGER,
    local_qty       INTEGER,
    action_taken    TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_reconciliation_log_created
    ON reconciliation_log(created_at DESC);
