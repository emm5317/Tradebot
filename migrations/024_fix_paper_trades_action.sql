-- Fix paper_trades action CHECK constraint to accept 'entry'/'exit' values
-- from crypto_evaluator signals (previously only allowed 'buy'/'sell').
ALTER TABLE paper_trades DROP CONSTRAINT IF EXISTS paper_trades_action_check;
ALTER TABLE paper_trades ADD CONSTRAINT paper_trades_action_check
    CHECK (action IN ('buy', 'sell', 'entry', 'exit'));
