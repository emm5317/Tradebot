CREATE TABLE orders (
    id              BIGSERIAL PRIMARY KEY,
    kalshi_order_id TEXT UNIQUE,
    idempotency_key TEXT UNIQUE NOT NULL,
    signal_id       BIGINT REFERENCES signals(id),
    ticker          TEXT NOT NULL REFERENCES contracts(ticker),
    direction       TEXT NOT NULL,
    order_type      TEXT NOT NULL,
    size_cents      INTEGER NOT NULL,
    limit_price     REAL,
    fill_price      REAL,
    status          TEXT NOT NULL DEFAULT 'pending',
    outcome         TEXT DEFAULT 'pending',
    pnl_cents       INTEGER,
    latency_ms      REAL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    filled_at       TIMESTAMPTZ,
    settled_at      TIMESTAMPTZ
);

CREATE INDEX idx_orders_created ON orders(created_at);
CREATE INDEX idx_orders_status ON orders(status);
CREATE INDEX idx_orders_ticker ON orders(ticker);
