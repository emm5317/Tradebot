-- Phase 6.1: Parameter sweep framework + settlement summary.
-- Tracks backtest runs with hyperparameters and results for optimization.
-- Also adds daily_settlement_summary for fast outcome lookups.

-- Backtest run tracking
CREATE TABLE IF NOT EXISTS backtest_runs (
    id              BIGSERIAL PRIMARY KEY,
    run_id          UUID NOT NULL DEFAULT gen_random_uuid(),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Run configuration
    signal_type     TEXT NOT NULL,               -- 'weather' or 'crypto'
    start_date      DATE NOT NULL,
    end_date        DATE NOT NULL,
    params          JSONB NOT NULL DEFAULT '{}', -- hyperparameters used
    description     TEXT,                        -- optional human label
    -- Aggregate results
    total_contracts INT NOT NULL DEFAULT 0,
    total_signals   INT NOT NULL DEFAULT 0,
    accuracy        DOUBLE PRECISION,
    brier_score     DOUBLE PRECISION,
    log_loss        DOUBLE PRECISION,
    simulated_pnl_cents BIGINT NOT NULL DEFAULT 0,
    win_count       INT NOT NULL DEFAULT 0,
    loss_count      INT NOT NULL DEFAULT 0,
    avg_edge        DOUBLE PRECISION,
    avg_kelly       DOUBLE PRECISION,
    -- Calibration by bucket
    calibration     JSONB,                       -- {"0-10%": {"count":N, "avg_predicted":P, "actual_win_rate":R}, ...}
    -- Per-signal detail (optional, for drill-down)
    signals_detail  JSONB,                       -- array of per-signal records
    -- Walk-forward metadata
    train_start     DATE,
    train_end       DATE,
    validation_start DATE,
    validation_end  DATE,
    -- Comparison
    baseline_run_id UUID,                        -- reference run for delta comparison
    UNIQUE(run_id)
);

CREATE INDEX IF NOT EXISTS idx_backtest_runs_signal_type ON backtest_runs (signal_type, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_backtest_runs_brier ON backtest_runs (signal_type, brier_score) WHERE brier_score IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_backtest_runs_params ON backtest_runs USING gin (params);

-- Daily settlement summary for fast backtesting lookups
CREATE TABLE IF NOT EXISTS daily_settlement_summary (
    station         TEXT NOT NULL,
    obs_date        DATE NOT NULL,
    tz              TEXT NOT NULL DEFAULT 'US/Central',
    final_max_f     DOUBLE PRECISION,
    final_min_f     DOUBLE PRECISION,
    metar_max_f     DOUBLE PRECISION,            -- from METAR 6hr groups
    metar_min_f     DOUBLE PRECISION,
    obs_count       INT NOT NULL DEFAULT 0,
    first_obs_at    TIMESTAMPTZ,
    last_obs_at     TIMESTAMPTZ,
    locked_max_at   TIMESTAMPTZ,                 -- when max was locked
    locked_min_at   TIMESTAMPTZ,
    contracts_settled INT NOT NULL DEFAULT 0,     -- how many contracts settled this day
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (station, obs_date)
);

CREATE INDEX IF NOT EXISTS idx_settlement_summary_date ON daily_settlement_summary (obs_date DESC);
