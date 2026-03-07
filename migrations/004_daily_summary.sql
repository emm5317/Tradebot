-- Daily P&L and performance summary
CREATE TABLE IF NOT EXISTS daily_summary (
    id            SERIAL PRIMARY KEY,
    trade_date    DATE NOT NULL UNIQUE,
    total_trades  INT DEFAULT 0,
    wins          INT DEFAULT 0,
    losses        INT DEFAULT 0,
    gross_pnl     REAL DEFAULT 0,
    fees          REAL DEFAULT 0,
    net_pnl       REAL DEFAULT 0,
    created_at    TIMESTAMPTZ DEFAULT now()
);

CREATE INDEX idx_daily_summary_date ON daily_summary(trade_date);
