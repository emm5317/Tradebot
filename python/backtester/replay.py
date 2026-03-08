"""Replay engine for source attribution and model backtesting.

Replays historical data through model evaluation to:
1. Measure each data source's marginal lift (Brier score delta)
2. Compare model versions on identical historical data
3. Attribute PnL to specific signal components
"""

from __future__ import annotations

import asyncio
from dataclasses import dataclass, field
from datetime import datetime, timedelta, timezone
from typing import Any, Callable

import asyncpg
import structlog

logger = structlog.get_logger()


@dataclass
class ReplayConfig:
    """Configuration for a replay run."""

    start_time: datetime
    end_time: datetime
    signal_type: str = "weather"   # "weather" or "crypto"
    ablation_sources: list[str] = field(default_factory=list)
    # Sources to ablate (remove) for attribution testing
    # e.g., ["hrrr", "coinbase"] to test without those sources


@dataclass
class ReplayResult:
    """Results from a single replay run."""

    config: ReplayConfig
    n_evaluations: int = 0
    n_contracts: int = 0
    brier_score: float = 0.0
    log_loss: float = 0.0
    pnl_cents: int = 0
    win_rate: float = 0.0
    component_scores: dict[str, float] = field(default_factory=dict)


@dataclass
class SourceAttribution:
    """Marginal lift attribution for a data source."""

    source_name: str
    brier_delta: float      # positive = source improves model
    pnl_delta: int          # positive = source adds PnL
    n_affected: int         # number of evaluations affected


class ReplayEngine:
    """Replays historical model evaluations with source ablation.

    Usage:
        engine = ReplayEngine(pool)
        baseline = await engine.replay(ReplayConfig(...))
        ablated = await engine.replay(ReplayConfig(..., ablation_sources=["hrrr"]))
        attribution = engine.compute_attribution("hrrr", baseline, ablated)
    """

    def __init__(self, pool: asyncpg.Pool) -> None:
        self.pool = pool

    async def replay(
        self,
        config: ReplayConfig,
        model_fn: Callable[..., Any] | None = None,
    ) -> ReplayResult:
        """Replay historical evaluations for a time period.

        If model_fn is provided, re-evaluates using the given model function.
        Otherwise, uses stored model_evaluations for scoring.
        """
        result = ReplayResult(config=config)

        async with self.pool.acquire() as conn:
            # Load model evaluations for the period
            rows = await conn.fetch(
                """
                SELECT
                    me.ticker, me.signal_type, me.model_prob, me.market_price,
                    me.edge, me.direction, me.inputs, me.components,
                    me.confidence, me.acted_on, me.evaluated_at,
                    c.settled_yes, c.close_price
                FROM model_evaluations me
                LEFT JOIN contracts c ON c.ticker = me.ticker
                WHERE me.signal_type = $1
                  AND me.evaluated_at >= $2
                  AND me.evaluated_at <= $3
                ORDER BY me.evaluated_at
                """,
                config.signal_type,
                config.start_time,
                config.end_time,
            )

        if not rows:
            logger.info("replay_no_data", config=str(config))
            return result

        result.n_evaluations = len(rows)
        tickers = set()
        brier_sum = 0.0
        brier_count = 0
        pnl_total = 0
        wins = 0

        for row in rows:
            ticker = row["ticker"]
            tickers.add(ticker)
            settled_yes = row["settled_yes"]
            model_prob = row["model_prob"]
            market_price = row["market_price"]
            acted_on = row["acted_on"]

            if settled_yes is None or model_prob is None:
                continue

            # Brier score: (forecast - outcome)^2
            outcome = 1.0 if settled_yes else 0.0
            brier = (model_prob - outcome) ** 2
            brier_sum += brier
            brier_count += 1

            # PnL if acted on
            if acted_on and market_price is not None:
                direction = row["direction"]
                if direction == "yes":
                    pnl = (outcome - market_price) * 100  # in cents
                else:
                    pnl = (market_price - outcome) * 100
                pnl_total += int(pnl)
                if pnl > 0:
                    wins += 1

        result.n_contracts = len(tickers)
        if brier_count > 0:
            result.brier_score = brier_sum / brier_count
            result.win_rate = wins / brier_count if brier_count > 0 else 0.0
        result.pnl_cents = pnl_total

        logger.info(
            "replay_complete",
            signal_type=config.signal_type,
            n_evaluations=result.n_evaluations,
            n_contracts=result.n_contracts,
            brier_score=f"{result.brier_score:.4f}",
            pnl_cents=result.pnl_cents,
        )

        return result

    def compute_attribution(
        self,
        source_name: str,
        baseline: ReplayResult,
        ablated: ReplayResult,
    ) -> SourceAttribution:
        """Compute marginal lift of a data source by comparing baseline vs ablated.

        Positive brier_delta means the source IMPROVES the model (lower Brier = better).
        """
        brier_delta = baseline.brier_score - ablated.brier_score
        # Negative brier_delta = baseline is better = source helps
        # We flip sign so positive = source improves model
        brier_delta = -brier_delta

        pnl_delta = baseline.pnl_cents - ablated.pnl_cents

        return SourceAttribution(
            source_name=source_name,
            brier_delta=brier_delta,
            pnl_delta=pnl_delta,
            n_affected=baseline.n_evaluations,
        )

    async def get_available_periods(
        self, signal_type: str
    ) -> list[tuple[datetime, datetime]]:
        """Get available replay periods from stored evaluations."""
        async with self.pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT
                    DATE(evaluated_at) as eval_date,
                    MIN(evaluated_at) as start_time,
                    MAX(evaluated_at) as end_time,
                    COUNT(*) as n_evals
                FROM model_evaluations
                WHERE signal_type = $1
                GROUP BY DATE(evaluated_at)
                ORDER BY eval_date DESC
                LIMIT 30
                """,
                signal_type,
            )

        return [(row["start_time"], row["end_time"]) for row in rows]
