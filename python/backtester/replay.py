"""Replay engine for source attribution and model backtesting.

Replays historical data through model evaluation to:
1. Measure each data source's marginal lift (Brier score delta)
2. Compare model versions on identical historical data
3. Attribute PnL to specific signal components

Usage:
    python -m backtester.replay --start 2026-01-01 --end 2026-03-01 --type weather
    python -m backtester.replay --start 2026-01-01 --end 2026-03-01 --type weather --ablate hrrr
    python -m backtester.replay --start 2026-01-01 --end 2026-03-01 --attribution
"""

from __future__ import annotations

import argparse
import asyncio
import json
import math
from dataclasses import dataclass, field
from datetime import date, datetime, timedelta, timezone
from typing import Any, Callable

import asyncpg
import structlog

from backtester.costs import FeeModel

logger = structlog.get_logger()

# Known model component sources for attribution
WEATHER_SOURCES = ["physics", "hrrr", "trend", "climo"]
CRYPTO_SOURCES = ["n_d2", "levy", "basis", "funding"]


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

    def __init__(
        self,
        pool: asyncpg.Pool,
        fee_model: FeeModel | None = None,
    ) -> None:
        self.pool = pool
        self.fee_model = fee_model or FeeModel()

    async def replay(
        self,
        config: ReplayConfig,
        model_fn: Callable[..., Any] | None = None,
    ) -> ReplayResult:
        """Replay historical evaluations for a time period.

        If ablation_sources is non-empty, re-blends the model probability
        without those sources to measure their marginal contribution.
        """
        result = ReplayResult(config=config)

        async with self.pool.acquire() as conn:
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
        log_loss_sum = 0.0
        brier_count = 0
        pnl_total = 0
        wins = 0

        for row in rows:
            ticker = row["ticker"]
            tickers.add(ticker)
            settled_yes = row["settled_yes"]
            model_prob = float(row["model_prob"]) if row["model_prob"] is not None else None
            market_price = float(row["market_price"]) if row["market_price"] is not None else None
            acted_on = row["acted_on"]
            components = row["components"]

            if settled_yes is None or model_prob is None:
                continue

            # Apply model_fn override if provided
            if model_fn is not None:
                model_prob = model_fn(row)

            # Apply source ablation: re-blend without ablated sources
            if config.ablation_sources and components:
                model_prob = ablate_and_reblend(components, config.ablation_sources)

            # Brier score: (forecast - outcome)^2
            outcome = 1.0 if settled_yes else 0.0
            brier = (model_prob - outcome) ** 2
            brier_sum += brier

            # Log-loss
            eps = 1e-15
            p = max(eps, min(1.0 - eps, model_prob))
            log_loss_sum += -(outcome * math.log(p) + (1.0 - outcome) * math.log(1.0 - p))

            brier_count += 1

            # PnL if acted on
            if acted_on and market_price is not None:
                direction = row["direction"]
                fee = self.fee_model.round_trip_cost(market_price)
                if direction == "yes":
                    pnl = (outcome - market_price) * 100 - fee
                else:
                    pnl = (market_price - outcome) * 100 - fee
                pnl_total += int(pnl)
                if pnl > 0:
                    wins += 1

        result.n_contracts = len(tickers)
        if brier_count > 0:
            result.brier_score = brier_sum / brier_count
            result.log_loss = log_loss_sum / brier_count
            result.win_rate = wins / brier_count
        result.pnl_cents = pnl_total

        logger.info(
            "replay_complete",
            signal_type=config.signal_type,
            n_evaluations=result.n_evaluations,
            n_contracts=result.n_contracts,
            brier_score=f"{result.brier_score:.4f}",
            log_loss=f"{result.log_loss:.4f}",
            pnl_cents=result.pnl_cents,
            ablated=config.ablation_sources or "none",
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
        # baseline.brier < ablated.brier means source helps → positive delta
        brier_delta = ablated.brier_score - baseline.brier_score

        pnl_delta = baseline.pnl_cents - ablated.pnl_cents

        return SourceAttribution(
            source_name=source_name,
            brier_delta=brier_delta,
            pnl_delta=pnl_delta,
            n_affected=baseline.n_evaluations,
        )

    async def run_full_attribution(
        self,
        start_time: datetime,
        end_time: datetime,
        signal_type: str = "weather",
    ) -> list[SourceAttribution]:
        """Run baseline + ablation for each source and rank by marginal lift."""
        sources = WEATHER_SOURCES if signal_type == "weather" else CRYPTO_SOURCES

        baseline_config = ReplayConfig(
            start_time=start_time,
            end_time=end_time,
            signal_type=signal_type,
        )
        baseline = await self.replay(baseline_config)

        if baseline.n_evaluations == 0:
            return []

        attributions = []
        for source in sources:
            ablated_config = ReplayConfig(
                start_time=start_time,
                end_time=end_time,
                signal_type=signal_type,
                ablation_sources=[source],
            )
            ablated = await self.replay(ablated_config)
            attr = self.compute_attribution(source, baseline, ablated)
            attributions.append(attr)

        # Sort by brier_delta descending (most helpful source first)
        attributions.sort(key=lambda a: a.brier_delta, reverse=True)
        return attributions

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


# ── Source ablation logic ────────────────────────────────────────

def ablate_and_reblend(
    components: dict | str | None,
    ablation_sources: list[str],
) -> float:
    """Re-compute blended probability with specified sources zeroed out.

    The components dict maps source names to their probability contributions,
    e.g. {"physics": 0.65, "hrrr": 0.70, "trend": 0.60, "climo": 0.55,
           "weights": [0.45, 0.25, 0.20, 0.10]}

    When a source is ablated, its weight is redistributed proportionally
    among remaining sources.
    """
    if components is None:
        return 0.5

    if isinstance(components, str):
        components = json.loads(components)

    # Extract weights and source probabilities
    weights = components.get("weights")
    if not weights:
        # No weight info — can't ablate, return stored prob
        prob = components.get("probability") or components.get("model_prob")
        return float(prob) if prob is not None else 0.5

    # Map source names to their indices
    source_names = ["physics", "hrrr", "trend", "climo"]
    # Support crypto sources too
    if "n_d2" in components:
        source_names = ["n_d2", "levy", "basis", "funding"]

    remaining_weight = 0.0
    blended = 0.0

    for i, name in enumerate(source_names):
        if i >= len(weights):
            break

        w = float(weights[i])

        if name in ablation_sources:
            continue  # skip this source

        prob = components.get(name)
        if prob is None:
            continue

        remaining_weight += w
        blended += w * float(prob)

    if remaining_weight > 0:
        return max(0.0, min(1.0, blended / remaining_weight))

    return 0.5  # all sources ablated


# ── CLI entry point ──────────────────────────────────────────────

async def main() -> None:
    parser = argparse.ArgumentParser(description="Replay engine for source attribution")
    parser.add_argument("--start", required=True, help="Start date (YYYY-MM-DD)")
    parser.add_argument("--end", required=True, help="End date (YYYY-MM-DD)")
    parser.add_argument("--type", default="weather", help="Signal type (weather, crypto)")
    parser.add_argument("--ablate", nargs="+", default=[], help="Sources to ablate (e.g., --ablate hrrr)")
    parser.add_argument("--attribution", action="store_true", help="Run full attribution analysis")
    parser.add_argument("--no-fees", action="store_true", help="Disable transaction cost modeling")
    args = parser.parse_args()

    from config import get_settings
    settings = get_settings()
    pool = await asyncpg.create_pool(settings.database_url, min_size=1, max_size=3)

    fee_model = FeeModel() if not args.no_fees else FeeModel(fee_type="flat", flat_fee_cents=0)
    engine = ReplayEngine(pool, fee_model=fee_model)

    start = datetime.combine(
        date.fromisoformat(args.start), datetime.min.time(), tzinfo=timezone.utc
    )
    end = datetime.combine(
        date.fromisoformat(args.end), datetime.max.time(), tzinfo=timezone.utc
    )

    if args.attribution:
        attributions = await engine.run_full_attribution(start, end, args.type)
        if not attributions:
            print("No model evaluations found for the given period.")
        else:
            print(f"\n{'='*60}")
            print(f"  Source Attribution: {args.type} ({args.start} to {args.end})")
            print(f"{'='*60}")
            print(f"  {'Source':<12}  {'Brier Delta':>12}  {'PnL Delta':>10}  {'Affected':>8}")
            print(f"  {'-'*12}  {'-'*12}  {'-'*10}  {'-'*8}")
            for attr in attributions:
                sign = "+" if attr.brier_delta > 0 else ""
                pnl_sign = "+" if attr.pnl_delta > 0 else ""
                print(
                    f"  {attr.source_name:<12}  "
                    f"{sign}{attr.brier_delta:>11.4f}  "
                    f"{pnl_sign}${attr.pnl_delta/100:>8.2f}  "
                    f"{attr.n_affected:>8}"
                )
            print()
    else:
        config = ReplayConfig(
            start_time=start,
            end_time=end,
            signal_type=args.type,
            ablation_sources=args.ablate,
        )
        result = await engine.replay(config)
        print(f"\nReplay Results:")
        print(f"  Evaluations: {result.n_evaluations}")
        print(f"  Contracts:   {result.n_contracts}")
        print(f"  Brier Score: {result.brier_score:.4f}")
        print(f"  Log Loss:    {result.log_loss:.4f}")
        print(f"  Win Rate:    {result.win_rate:.1%}")
        print(f"  P&L:         ${result.pnl_cents/100:.2f}")
        if args.ablate:
            print(f"  Ablated:     {', '.join(args.ablate)}")
        print()

    await pool.close()


if __name__ == "__main__":
    asyncio.run(main())
