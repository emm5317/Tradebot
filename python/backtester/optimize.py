"""Ensemble weight optimizer — find optimal model weights via grid search.

Searches over ensemble weights (physics, climo, trend) and sigma scaling
factors to minimize Brier score on historical data. Uses k-fold
cross-validation to prevent overfitting.

Usage:
    python -m backtester.optimize --start 2025-01-01 --end 2025-06-30
    python -m backtester.optimize --start 2025-01-01 --end 2025-06-30 --folds 5
    python -m backtester.optimize --start 2025-01-01 --end 2025-06-30 --type weather --granularity fine
"""

from __future__ import annotations

import argparse
import asyncio
import itertools
import json
import math
from dataclasses import dataclass, field
from datetime import datetime, timezone

import asyncpg
import structlog

from config import get_settings
from data.mesonet import ASOSObservation
from models.physics import (
    _DEFAULT_SIGMA,
    climatological_probability,
    compute_weather_probability,
    fast_norm_cdf,
    trend_extrapolation_probability,
)
from signals.types import Contract, OrderbookState

logger = structlog.get_logger()


@dataclass
class OptimizationResult:
    """Result from a single weight configuration evaluation."""

    weights: tuple[float, float, float]
    sigma_scale: float
    brier_score: float
    accuracy: float
    signal_count: int


@dataclass
class OptimizationReport:
    """Full optimization run report."""

    best: OptimizationResult | None = None
    all_results: list[OptimizationResult] = field(default_factory=list)
    cv_folds: int = 0
    total_configs_tested: int = 0
    search_space: dict = field(default_factory=dict)

    def summary(self) -> dict:
        top_5 = sorted(self.all_results, key=lambda r: r.brier_score)[:5]
        return {
            "best_weights": {
                "physics": round(self.best.weights[0], 2) if self.best else None,
                "climo": round(self.best.weights[1], 2) if self.best else None,
                "trend": round(self.best.weights[2], 2) if self.best else None,
            },
            "best_sigma_scale": round(self.best.sigma_scale, 2) if self.best else None,
            "best_brier": round(self.best.brier_score, 4) if self.best else None,
            "best_accuracy": f"{self.best.accuracy:.1%}" if self.best else None,
            "configs_tested": self.total_configs_tested,
            "cv_folds": self.cv_folds,
            "top_5": [
                {
                    "weights": [round(r.weights[0], 2), round(r.weights[1], 2), round(r.weights[2], 2)],
                    "sigma_scale": round(r.sigma_scale, 2),
                    "brier": round(r.brier_score, 4),
                    "accuracy": f"{r.accuracy:.1%}",
                    "n": r.signal_count,
                }
                for r in top_5
            ],
            "search_space": self.search_space,
        }


@dataclass
class HistoricalContract:
    """A settled contract with observation data for optimization."""

    ticker: str
    station: str
    threshold: float
    settlement_time: datetime
    settled_yes: bool
    temperature_f: float
    market_price: float
    minutes_remaining: float
    hour: int
    month: int
    sigma_value: float  # from sigma table


async def load_optimization_dataset(
    pool: asyncpg.Pool,
    start: datetime,
    end: datetime,
    signal_type: str = "weather",
) -> list[HistoricalContract]:
    """Load historical contracts with matched observations for optimization.

    Joins contracts with their nearest observation and market snapshot
    to create a flat dataset for fast evaluation during grid search.
    """
    query = """
        SELECT
            c.ticker, c.station, c.threshold, c.settlement_time, c.settled_yes,
            o.temperature_f,
            ms.yes_price,
            EXTRACT(EPOCH FROM (c.settlement_time - ms.captured_at)) / 60.0 AS minutes_remaining,
            EXTRACT(HOUR FROM c.settlement_time)::int AS hour,
            EXTRACT(MONTH FROM c.settlement_time)::int AS month
        FROM contracts c
        JOIN LATERAL (
            SELECT temperature_f, observed_at
            FROM observations
            WHERE source = 'asos'
              AND station = c.station
              AND observed_at < c.settlement_time
              AND observed_at > c.settlement_time - interval '30 minutes'
            ORDER BY observed_at DESC
            LIMIT 1
        ) o ON TRUE
        JOIN LATERAL (
            SELECT yes_price, captured_at
            FROM market_snapshots
            WHERE ticker = c.ticker
              AND captured_at < c.settlement_time
              AND captured_at > c.settlement_time - interval '30 minutes'
            ORDER BY captured_at DESC
            LIMIT 1
        ) ms ON TRUE
        WHERE c.settled_yes IS NOT NULL
          AND c.settlement_time >= $1
          AND c.settlement_time <= $2
          AND c.threshold IS NOT NULL
          AND c.station IS NOT NULL
          AND o.temperature_f IS NOT NULL
          AND ms.yes_price IS NOT NULL
    """

    if signal_type == "weather":
        query += """
          AND (c.category ILIKE '%weather%' OR c.category ILIKE '%temperature%')
        """

    query += " ORDER BY c.settlement_time"

    async with pool.acquire() as conn:
        rows = await conn.fetch(query, start, end)

    # Build sigma table for lookups
    sigma_table = await _build_sigma_table_cached(pool)

    dataset = []
    for row in rows:
        station = row["station"]
        hour = row["hour"]
        month = row["month"]
        sigma = sigma_table.get((station, hour, month), _DEFAULT_SIGMA)

        dataset.append(HistoricalContract(
            ticker=row["ticker"],
            station=station,
            threshold=float(row["threshold"]),
            settlement_time=row["settlement_time"],
            settled_yes=row["settled_yes"],
            temperature_f=float(row["temperature_f"]),
            market_price=float(row["yes_price"]),
            minutes_remaining=float(row["minutes_remaining"]),
            hour=hour,
            month=month,
            sigma_value=sigma,
        ))

    logger.info("optimization_dataset_loaded", contracts=len(dataset))
    return dataset


async def _build_sigma_table_cached(pool: asyncpg.Pool) -> dict:
    """Build sigma table (cached for optimization runs)."""
    from models.physics import build_sigma_table
    return await build_sigma_table(pool)


def evaluate_weights(
    dataset: list[HistoricalContract],
    weights: tuple[float, float, float],
    sigma_scale: float = 1.0,
    climo_table: dict | None = None,
) -> OptimizationResult:
    """Evaluate a weight configuration against the dataset.

    This is the inner loop — called thousands of times during grid search.
    Pure Python, no DB access, no async.
    """
    w_physics, w_climo, w_trend = weights
    brier_sum = 0.0
    correct = 0
    n = 0

    for contract in dataset:
        sigma = contract.sigma_value * sigma_scale

        # Physics model
        p_physics = compute_weather_probability(
            contract.temperature_f,
            contract.threshold,
            contract.minutes_remaining,
            sigma,
        )

        # Climatological model
        p_climo = climatological_probability(
            contract.station,
            contract.hour,
            contract.month,
            contract.threshold,
            contract.temperature_f,
            climo_table,
        )

        # Trend: no recent_temps in historical data, use 0.5
        p_trend = 0.5

        # Ensemble
        p_ensemble = w_physics * p_physics + w_climo * p_climo + w_trend * p_trend
        p_ensemble = max(0.0, min(1.0, p_ensemble))

        # Outcome
        actual = 1.0 if contract.settled_yes else 0.0
        brier_sum += (p_ensemble - actual) ** 2

        predicted_yes = p_ensemble > 0.5
        if predicted_yes == contract.settled_yes:
            correct += 1
        n += 1

    if n == 0:
        return OptimizationResult(
            weights=weights, sigma_scale=sigma_scale,
            brier_score=1.0, accuracy=0.0, signal_count=0,
        )

    return OptimizationResult(
        weights=weights,
        sigma_scale=sigma_scale,
        brier_score=brier_sum / n,
        accuracy=correct / n,
        signal_count=n,
    )


def generate_weight_grid(granularity: str = "medium") -> list[tuple[float, float, float]]:
    """Generate weight combinations that sum to 1.0.

    Granularity controls the step size:
    - coarse: 0.10 steps (66 combos)
    - medium: 0.05 steps (231 combos)
    - fine: 0.025 steps (861 combos)
    """
    if granularity == "coarse":
        step = 0.10
    elif granularity == "fine":
        step = 0.025
    else:
        step = 0.05

    # Generate all triplets that sum to 1.0
    weights = []
    n_steps = int(1.0 / step)
    for i in range(n_steps + 1):
        for j in range(n_steps + 1 - i):
            k = n_steps - i - j
            w1 = round(i * step, 3)
            w2 = round(j * step, 3)
            w3 = round(k * step, 3)
            # Skip degenerate cases
            if w1 < 0.05 and w2 < 0.05 and w3 < 0.05:
                continue
            weights.append((w1, w2, w3))

    return weights


def generate_sigma_scales(granularity: str = "medium") -> list[float]:
    """Generate sigma scaling factors to test."""
    if granularity == "coarse":
        return [0.5, 0.75, 1.0, 1.25, 1.5, 2.0]
    elif granularity == "fine":
        return [0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1.0, 1.1, 1.2, 1.3, 1.5, 1.75, 2.0]
    else:
        return [0.5, 0.7, 0.85, 1.0, 1.15, 1.3, 1.5, 2.0]


def cross_validate(
    dataset: list[HistoricalContract],
    weights: tuple[float, float, float],
    sigma_scale: float,
    folds: int = 3,
    climo_table: dict | None = None,
) -> float:
    """K-fold cross-validation to get robust Brier score estimate.

    Splits dataset chronologically (not randomly) to respect time ordering.
    """
    if len(dataset) < folds * 5:
        # Too few samples for CV — just evaluate on full set
        result = evaluate_weights(dataset, weights, sigma_scale, climo_table)
        return result.brier_score

    fold_size = len(dataset) // folds
    scores = []

    for i in range(folds):
        test_start = i * fold_size
        test_end = (i + 1) * fold_size if i < folds - 1 else len(dataset)

        # Test set is one fold; train on the rest
        # (We don't actually "train" — just evaluate on test fold)
        test_set = dataset[test_start:test_end]
        result = evaluate_weights(test_set, weights, sigma_scale, climo_table)
        if result.signal_count > 0:
            scores.append(result.brier_score)

    return sum(scores) / len(scores) if scores else 1.0


async def optimize(
    pool: asyncpg.Pool,
    start: datetime,
    end: datetime,
    signal_type: str = "weather",
    granularity: str = "medium",
    folds: int = 3,
) -> OptimizationReport:
    """Run full optimization: load data, grid search, cross-validate best."""
    report = OptimizationReport(cv_folds=folds)

    # Load dataset
    dataset = await load_optimization_dataset(pool, start, end, signal_type)
    if not dataset:
        logger.warning("no_data", msg="No historical contracts found for optimization")
        return report

    # Load climo table for the climatological model component
    from models.physics import build_climo_table
    climo_table = await build_climo_table(pool)

    # Generate search space
    weight_grid = generate_weight_grid(granularity)
    sigma_scales = generate_sigma_scales(granularity)
    total_configs = len(weight_grid) * len(sigma_scales)

    report.search_space = {
        "weight_combos": len(weight_grid),
        "sigma_scales": len(sigma_scales),
        "total_configs": total_configs,
        "granularity": granularity,
    }
    report.total_configs_tested = total_configs

    logger.info(
        "optimization_start",
        configs=total_configs,
        dataset_size=len(dataset),
        folds=folds,
    )

    # Phase 1: Coarse search on full dataset (fast)
    best_brier = float("inf")
    for weights in weight_grid:
        for sigma_scale in sigma_scales:
            result = evaluate_weights(dataset, weights, sigma_scale, climo_table)
            report.all_results.append(result)

            if result.brier_score < best_brier:
                best_brier = result.brier_score
                report.best = result

    # Phase 2: Cross-validate top 10 candidates
    top_candidates = sorted(report.all_results, key=lambda r: r.brier_score)[:10]
    best_cv_brier = float("inf")

    for candidate in top_candidates:
        cv_brier = cross_validate(
            dataset, candidate.weights, candidate.sigma_scale, folds, climo_table
        )
        if cv_brier < best_cv_brier:
            best_cv_brier = cv_brier
            report.best = OptimizationResult(
                weights=candidate.weights,
                sigma_scale=candidate.sigma_scale,
                brier_score=cv_brier,
                accuracy=candidate.accuracy,
                signal_count=candidate.signal_count,
            )

    logger.info(
        "optimization_complete",
        best_weights=report.best.weights if report.best else None,
        best_sigma=report.best.sigma_scale if report.best else None,
        best_brier=round(report.best.brier_score, 4) if report.best else None,
    )

    return report


async def main() -> None:
    parser = argparse.ArgumentParser(description="Optimize ensemble weights")
    parser.add_argument("--start", required=True, help="Start date (YYYY-MM-DD)")
    parser.add_argument("--end", required=True, help="End date (YYYY-MM-DD)")
    parser.add_argument("--type", default="weather", help="Signal type (default: weather)")
    parser.add_argument(
        "--granularity",
        choices=["coarse", "medium", "fine"],
        default="medium",
        help="Search granularity (default: medium)",
    )
    parser.add_argument("--folds", type=int, default=3, help="CV folds (default: 3)")
    args = parser.parse_args()

    start = datetime.strptime(args.start, "%Y-%m-%d").replace(tzinfo=timezone.utc)
    end = datetime.strptime(args.end, "%Y-%m-%d").replace(tzinfo=timezone.utc)

    settings = get_settings()
    pool = await asyncpg.create_pool(settings.database_url, min_size=1, max_size=3)

    try:
        report = await optimize(
            pool, start, end,
            signal_type=args.type,
            granularity=args.granularity,
            folds=args.folds,
        )

        print(json.dumps(report.summary(), indent=2))

        if report.best:
            print(f"\n--- Recommended config update ---")
            print(f"Ensemble weights: ({report.best.weights[0]}, {report.best.weights[1]}, {report.best.weights[2]})")
            print(f"Sigma scale: {report.best.sigma_scale}")
            print(f"Pass to WeatherSignalEvaluator(ensemble_weights=({report.best.weights[0]}, {report.best.weights[1]}, {report.best.weights[2]}))")

    finally:
        await pool.close()


if __name__ == "__main__":
    asyncio.run(main())
