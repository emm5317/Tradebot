"""Phase 5.1: Daily strategy performance aggregation.

Computes per-strategy metrics from signals + orders tables and upserts
into strategy_performance. Designed to run at midnight UTC or after
last settlement.
"""

from __future__ import annotations

from datetime import UTC, date, datetime
from typing import TYPE_CHECKING

import structlog

if TYPE_CHECKING:
    import asyncpg

logger = structlog.get_logger()


async def aggregate_daily_performance(
    pool: asyncpg.Pool,
    target_date: date | None = None,
) -> dict[str, dict]:
    """Aggregate strategy performance for a given date.

    Args:
        pool: Database connection pool.
        target_date: Date to aggregate. Defaults to yesterday UTC.

    Returns:
        Dict of strategy -> metrics dict.
    """
    if target_date is None:
        target_date = datetime.now(UTC).date()

    results = {}

    for strategy in ("weather", "crypto"):
        metrics = await _compute_strategy_metrics(pool, strategy, target_date)
        await _upsert_performance(pool, strategy, target_date, metrics)
        results[strategy] = metrics
        logger.info(
            "strategy_aggregated",
            strategy=strategy,
            date=str(target_date),
            signals=metrics["signals_generated"],
            executed=metrics["signals_executed"],
            pnl=metrics["realized_pnl_cents"],
            brier=metrics.get("brier_score"),
        )

    return results


async def _compute_strategy_metrics(
    pool: asyncpg.Pool,
    strategy: str,
    target_date: date,
) -> dict:
    """Compute all metrics for one strategy on one date."""
    async with pool.acquire() as conn:
        # Signal counts
        sig_row = await conn.fetchrow(
            """
            SELECT
                COUNT(*) AS total,
                COUNT(*) FILTER (WHERE acted_on = true) AS executed,
                AVG(edge) FILTER (WHERE acted_on = true) AS avg_edge,
                AVG(kelly_fraction) FILTER (WHERE acted_on = true) AS avg_kelly
            FROM signals
            WHERE signal_type = $1
              AND created_at::date = $2
            """,
            strategy,
            target_date,
        )

        # Order outcomes
        order_row = await conn.fetchrow(
            """
            SELECT
                COUNT(*) FILTER (WHERE outcome = 'win') AS wins,
                COUNT(*) FILTER (WHERE outcome = 'loss') AS losses,
                COALESCE(SUM(pnl_cents) FILTER (WHERE outcome IN ('win', 'loss')), 0) AS realized_pnl
            FROM orders
            WHERE signal_type = $1
              AND created_at::date = $2
              AND outcome IN ('win', 'loss')
            """,
            strategy,
            target_date,
        )

        # Brier score: mean((model_prob - actual)^2) for settled signals
        # actual = 1.0 if the predicted direction won, 0.0 otherwise
        brier_row = await conn.fetchrow(
            """
            SELECT AVG(
                POWER(s.model_prob - CASE WHEN o.outcome = 'win' THEN 1.0 ELSE 0.0 END, 2)
            ) AS brier_score
            FROM signals s
            JOIN orders o ON o.signal_id = s.id
            WHERE s.signal_type = $1
              AND s.created_at::date = $2
              AND s.acted_on = true
              AND o.outcome IN ('win', 'loss')
            """,
            strategy,
            target_date,
        )

    return {
        "signals_generated": sig_row["total"] if sig_row else 0,
        "signals_executed": sig_row["executed"] if sig_row else 0,
        "avg_edge": float(sig_row["avg_edge"]) if sig_row and sig_row["avg_edge"] else None,
        "avg_kelly": float(sig_row["avg_kelly"]) if sig_row and sig_row["avg_kelly"] else None,
        "win_count": order_row["wins"] if order_row else 0,
        "loss_count": order_row["losses"] if order_row else 0,
        "realized_pnl_cents": order_row["realized_pnl"] if order_row else 0,
        "brier_score": float(brier_row["brier_score"]) if brier_row and brier_row["brier_score"] else None,
    }


async def _upsert_performance(
    pool: asyncpg.Pool,
    strategy: str,
    target_date: date,
    metrics: dict,
) -> None:
    """Upsert strategy_performance row."""
    async with pool.acquire() as conn:
        await conn.execute(
            """
            INSERT INTO strategy_performance (
                strategy, date, signals_generated, signals_executed,
                win_count, loss_count, realized_pnl_cents,
                avg_edge, avg_kelly, brier_score
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            ON CONFLICT (strategy, date) DO UPDATE SET
                signals_generated = EXCLUDED.signals_generated,
                signals_executed = EXCLUDED.signals_executed,
                win_count = EXCLUDED.win_count,
                loss_count = EXCLUDED.loss_count,
                realized_pnl_cents = EXCLUDED.realized_pnl_cents,
                avg_edge = EXCLUDED.avg_edge,
                avg_kelly = EXCLUDED.avg_kelly,
                brier_score = EXCLUDED.brier_score
            """,
            strategy,
            target_date,
            metrics["signals_generated"],
            metrics["signals_executed"],
            metrics["win_count"],
            metrics["loss_count"],
            metrics["realized_pnl_cents"],
            metrics["avg_edge"],
            metrics["avg_kelly"],
            metrics["brier_score"],
        )
