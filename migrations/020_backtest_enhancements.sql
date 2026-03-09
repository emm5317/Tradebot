-- Phase 8.0: Backtest enhancements — adds station column and advanced metrics.
-- Fixes calibrator Job 4 which references br.station (previously missing).
-- Phase 8.2/8.3: Adds transaction cost and advanced metric columns.

ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS station TEXT;
ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS sharpe_ratio DOUBLE PRECISION;
ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS sortino_ratio DOUBLE PRECISION;
ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS max_drawdown_cents BIGINT;
ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS max_drawdown_pct DOUBLE PRECISION;
ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS profit_factor DOUBLE PRECISION;
ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS ece DOUBLE PRECISION;
ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS fee_total_cents BIGINT DEFAULT 0;
ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS win_streak INT;
ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS loss_streak INT;
ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS time_decay_lambda DOUBLE PRECISION DEFAULT 0.0;

CREATE INDEX IF NOT EXISTS idx_backtest_runs_station
    ON backtest_runs (station, brier_score) WHERE station IS NOT NULL;
