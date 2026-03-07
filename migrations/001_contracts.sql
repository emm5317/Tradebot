CREATE TABLE contracts (
    ticker          TEXT PRIMARY KEY,
    title           TEXT NOT NULL,
    category        TEXT NOT NULL,
    city            TEXT,
    station         TEXT,
    threshold       REAL,
    settlement_time TIMESTAMPTZ NOT NULL,
    status          TEXT NOT NULL DEFAULT 'active',
    settled_yes     BOOLEAN,
    close_price     REAL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_contracts_settlement ON contracts(settlement_time);
CREATE INDEX idx_contracts_status ON contracts(status);
CREATE INDEX idx_contracts_category ON contracts(category);
