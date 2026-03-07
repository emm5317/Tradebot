CREATE TABLE daily_summary (
    date            DATE PRIMARY KEY,
    total_signals   INTEGER NOT NULL DEFAULT 0,
    total_orders    INTEGER NOT NULL DEFAULT 0,
    wins            INTEGER NOT NULL DEFAULT 0,
    losses          INTEGER NOT NULL DEFAULT 0,
    gross_pnl_cents INTEGER NOT NULL DEFAULT 0,
    fees_cents      INTEGER NOT NULL DEFAULT 0,
    net_pnl_cents   INTEGER NOT NULL DEFAULT 0,
    max_drawdown    INTEGER NOT NULL DEFAULT 0,
    avg_edge        REAL,
    avg_latency_ms  REAL,
    notes           TEXT
);
