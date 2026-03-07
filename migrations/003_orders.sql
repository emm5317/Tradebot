-- Orders placed on Kalshi
CREATE TABLE IF NOT EXISTS orders (
    id            SERIAL PRIMARY KEY,
    order_id      TEXT NOT NULL UNIQUE,
    ticker        TEXT NOT NULL REFERENCES contracts(ticker),
    signal_id     INT REFERENCES signals(id),
    side          TEXT NOT NULL,
    price         REAL NOT NULL,
    quantity      INT NOT NULL,
    filled_qty    INT DEFAULT 0,
    status        TEXT NOT NULL DEFAULT 'pending',
    pnl           REAL,
    created_at    TIMESTAMPTZ DEFAULT now(),
    updated_at    TIMESTAMPTZ DEFAULT now()
);

CREATE INDEX idx_orders_ticker ON orders(ticker);
CREATE INDEX idx_orders_status ON orders(status);
CREATE INDEX idx_orders_created ON orders(created_at);
