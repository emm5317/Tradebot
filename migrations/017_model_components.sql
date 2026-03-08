-- Phase 5.3: P&L attribution via model component logging.
-- Tracks which ensemble components contributed to each signal.

ALTER TABLE signals
    ADD COLUMN IF NOT EXISTS model_components JSONB;

-- Index for component-level analysis queries
CREATE INDEX IF NOT EXISTS idx_signals_components
    ON signals USING gin (model_components)
    WHERE model_components IS NOT NULL;
