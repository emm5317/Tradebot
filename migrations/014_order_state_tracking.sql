-- Phase 2: Order state machine tracking
-- Extends orders table with lifecycle state, fill tracking, and audit trail.

ALTER TABLE orders
    ADD COLUMN IF NOT EXISTS client_order_id TEXT,
    ADD COLUMN IF NOT EXISTS signal_type TEXT,
    ADD COLUMN IF NOT EXISTS order_state TEXT DEFAULT 'pending'
        CHECK (order_state IN (
            'pending', 'submitting', 'acknowledged', 'partial_fill',
            'filled', 'cancel_pending', 'cancelled', 'replacing',
            'rejected', 'unknown'
        )),
    ADD COLUMN IF NOT EXISTS requested_qty INTEGER,
    ADD COLUMN IF NOT EXISTS filled_qty INTEGER DEFAULT 0,
    ADD COLUMN IF NOT EXISTS transitions JSONB DEFAULT '[]'::jsonb,
    ADD COLUMN IF NOT EXISTS model_prob REAL,
    ADD COLUMN IF NOT EXISTS market_price_at_order REAL,
    ADD COLUMN IF NOT EXISTS crypto_snapshot JSONB;

-- Client order IDs should be unique when present
CREATE UNIQUE INDEX IF NOT EXISTS idx_orders_client_order_id
    ON orders(client_order_id) WHERE client_order_id IS NOT NULL;

-- Index for reconciliation queries
CREATE INDEX IF NOT EXISTS idx_orders_state
    ON orders(order_state) WHERE order_state NOT IN ('filled', 'cancelled', 'rejected');
