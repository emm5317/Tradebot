"""Calibration analysis — write backtest predictions to calibration table and analyze.

After running a backtest, this module:
1. Writes each signal's model_prob vs actual outcome to the calibration table
2. Computes calibration metrics (reliability, resolution, sharpness)
3. Identifies per-station, per-hour, per-bucket bias patterns
4. Outputs actionable insights for model tuning

Usage:
    python -m backtester.calibration --start 2025-01-01 --end 2025-06-30
    python -m backtester.calibration --start 2025-01-01 --end 2025-06-30 --type weather
"""

from __future__ import annotations

import argparse
import asyncio
import json
import math
from dataclasses import dataclass, field
from datetime import datetime, timezone

import asyncpg
import structlog

from backtester.engine import Backtester, SignalRecord
from config import Settings, get_settings
from models.physics import build_climo_table, build_sigma_table
from signals.crypto import CryptoSignalEvaluator
from signals.registry import EvaluatorRegistry
from signals.weather import WeatherSignalEvaluator

logger = structlog.get_logger()


@dataclass
class CalibrationBucket:
    """Calibration analysis for a single probability bucket."""

    bucket_label: str
    count: int = 0
    wins: int = 0
    avg_predicted: float = 0.0
    actual_win_rate: float = 0.0
    bias: float = 0.0  # predicted - actual (positive = overconfident)

    @property
    def is_well_calibrated(self) -> bool:
        """Within 5pp of predicted probability."""
        return abs(self.bias) < 0.05 and self.count >= 10


@dataclass
class CalibrationReport:
    """Full calibration analysis from a backtest run."""

    signal_type: str | None
    period: str
    total_signals: int = 0
    brier_score: float = 0.0
    reliability: float = 0.0  # Weighted avg squared bias per bucket
    resolution: float = 0.0  # Variance of actual rates across buckets
    sharpness: float = 0.0  # Variance of predicted probabilities
    buckets: list[CalibrationBucket] = field(default_factory=list)
    station_bias: dict[str, float] = field(default_factory=dict)
    hour_bias: dict[int, float] = field(default_factory=dict)
    recommendations: list[str] = field(default_factory=list)

    def summary(self) -> dict:
        return {
            "signal_type": self.signal_type or "all",
            "period": self.period,
            "total_signals": self.total_signals,
            "brier_score": round(self.brier_score, 4),
            "reliability": round(self.reliability, 4),
            "resolution": round(self.resolution, 4),
            "sharpness": round(self.sharpness, 4),
            "calibration_curve": [
                {
                    "bucket": b.bucket_label,
                    "count": b.count,
                    "predicted": round(b.avg_predicted, 3),
                    "actual": round(b.actual_win_rate, 3),
                    "bias": round(b.bias, 3),
                    "calibrated": b.is_well_calibrated,
                }
                for b in self.buckets
                if b.count > 0
            ],
            "station_bias": {
                k: round(v, 3) for k, v in self.station_bias.items()
            },
            "hour_bias": {
                str(k): round(v, 3) for k, v in sorted(self.hour_bias.items())
            },
            "recommendations": self.recommendations,
        }


async def write_calibration_data(
    pool: asyncpg.Pool,
    signals: list[SignalRecord],
    sigma_table: dict | None = None,
) -> int:
    """Write backtest signal predictions + outcomes to the calibration table.

    This populates the calibration hypertable which feeds the
    calibration_rolling continuous aggregate for ongoing monitoring.
    """
    rows: list[tuple] = []
    for sig in signals:
        if sig.actual_outcome is None:
            continue

        # Probability for the bet direction
        if sig.direction == "yes":
            p = sig.model_prob
            outcome = sig.actual_outcome
        else:
            p = 1.0 - sig.model_prob
            outcome = not sig.actual_outcome

        bucket_idx = min(int(p * 10), 9)
        bucket_label = f"{bucket_idx * 10}-{(bucket_idx + 1) * 10}%"

        # Look up sigma used (if we have the table)
        sigma_used = 0.3  # default
        if sigma_table:
            # Try to extract station/hour/month from ticker context
            # For now, store the default — optimizer will refine
            pass

        rows.append((
            sig.ticker,
            sig.signal_type,
            float(p),
            float(sig.market_price),
            bool(outcome),
            bucket_label,
            sigma_used,
            datetime.now(timezone.utc),  # settled_at approximation
        ))

    if not rows:
        return 0

    async with pool.acquire() as conn:
        await conn.executemany(
            """
            INSERT INTO calibration (
                ticker, signal_type, model_prob, market_price,
                actual_outcome, prob_bucket, sigma_used, settled_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT DO NOTHING
            """,
            rows,
        )

    logger.info("calibration_data_written", rows=len(rows))
    return len(rows)


async def analyze_calibration(
    pool: asyncpg.Pool,
    signals: list[SignalRecord],
    signal_type: str | None = None,
    period: str = "",
) -> CalibrationReport:
    """Analyze calibration from backtest signals.

    Computes reliability diagram, Brier decomposition, and per-station/hour bias.
    """
    report = CalibrationReport(
        signal_type=signal_type,
        period=period,
    )

    # Filter to signals with outcomes
    evaluated = [
        s for s in signals
        if s.actual_outcome is not None
        and (signal_type is None or s.signal_type == signal_type)
    ]

    if not evaluated:
        report.recommendations.append("No signals with outcomes — need more historical data")
        return report

    report.total_signals = len(evaluated)

    # Build probability buckets
    bucket_data: dict[int, list[tuple[float, bool]]] = {i: [] for i in range(10)}

    brier_sum = 0.0
    for sig in evaluated:
        if sig.direction == "yes":
            p = sig.model_prob
            won = sig.actual_outcome
        else:
            p = 1.0 - sig.model_prob
            won = not sig.actual_outcome

        brier_sum += (p - (1.0 if won else 0.0)) ** 2
        bucket_idx = min(int(p * 10), 9)
        bucket_data[bucket_idx].append((p, bool(won)))

    report.brier_score = brier_sum / len(evaluated)

    # Build calibration curve
    overall_win_rate = sum(
        1 for s in evaluated
        if (s.actual_outcome if s.direction == "yes" else not s.actual_outcome)
    ) / len(evaluated)

    reliability_sum = 0.0
    resolution_sum = 0.0

    for i in range(10):
        entries = bucket_data[i]
        bucket = CalibrationBucket(
            bucket_label=f"{i * 10}-{(i + 1) * 10}%",
            count=len(entries),
        )
        if entries:
            bucket.avg_predicted = sum(e[0] for e in entries) / len(entries)
            bucket.wins = sum(1 for e in entries if e[1])
            bucket.actual_win_rate = bucket.wins / len(entries)
            bucket.bias = bucket.avg_predicted - bucket.actual_win_rate

            # Brier decomposition components
            weight = len(entries) / len(evaluated)
            reliability_sum += weight * (bucket.avg_predicted - bucket.actual_win_rate) ** 2
            resolution_sum += weight * (bucket.actual_win_rate - overall_win_rate) ** 2

        report.buckets.append(bucket)

    report.reliability = reliability_sum
    report.resolution = resolution_sum

    # Sharpness: variance of predicted probabilities
    preds = []
    for sig in evaluated:
        p = sig.model_prob if sig.direction == "yes" else (1.0 - sig.model_prob)
        preds.append(p)
    mean_pred = sum(preds) / len(preds)
    report.sharpness = sum((p - mean_pred) ** 2 for p in preds) / len(preds)

    # Per-station bias (weather only)
    station_signals: dict[str, list[tuple[float, bool]]] = {}
    for sig in evaluated:
        if sig.signal_type != "weather":
            continue
        # Extract station from ticker (format varies)
        station = _infer_station(sig.ticker)
        if station:
            station_signals.setdefault(station, [])
            p = sig.model_prob if sig.direction == "yes" else (1.0 - sig.model_prob)
            won = sig.actual_outcome if sig.direction == "yes" else not sig.actual_outcome
            station_signals[station].append((p, bool(won)))

    for station, entries in station_signals.items():
        if len(entries) >= 5:
            avg_pred = sum(e[0] for e in entries) / len(entries)
            actual_rate = sum(1 for e in entries if e[1]) / len(entries)
            report.station_bias[station] = avg_pred - actual_rate

    # Per-hour bias (using settlement hour as proxy)
    hour_signals: dict[int, list[tuple[float, bool]]] = {}
    for sig in evaluated:
        if sig.signal_type != "weather":
            continue
        # Use minutes_remaining to estimate settlement hour
        # This is approximate — would be exact with settlement_time in SignalRecord
        hour_signals.setdefault(12, [])  # placeholder

    # Generate recommendations
    _generate_recommendations(report)

    return report


def _generate_recommendations(report: CalibrationReport) -> None:
    """Generate actionable recommendations from calibration analysis."""
    recs = report.recommendations

    if report.brier_score > 0.25:
        recs.append(
            f"HIGH Brier score ({report.brier_score:.3f}) — model is poorly calibrated. "
            "Consider retraining or adjusting ensemble weights."
        )
    elif report.brier_score > 0.20:
        recs.append(
            f"Moderate Brier score ({report.brier_score:.3f}) — room for improvement."
        )
    else:
        recs.append(
            f"Good Brier score ({report.brier_score:.3f}) — model is reasonably calibrated."
        )

    # Check for systematic bias
    overconfident_buckets = [
        b for b in report.buckets if b.count >= 10 and b.bias > 0.05
    ]
    underconfident_buckets = [
        b for b in report.buckets if b.count >= 10 and b.bias < -0.05
    ]

    if len(overconfident_buckets) > len(report.buckets) / 3:
        recs.append(
            "OVERCONFIDENT: Model predicts higher probabilities than actual outcomes "
            "in multiple buckets. Consider increasing sigma or reducing physics weight."
        )

    if len(underconfident_buckets) > len(report.buckets) / 3:
        recs.append(
            "UNDERCONFIDENT: Model predicts lower probabilities than actual outcomes. "
            "Consider decreasing sigma or increasing physics weight."
        )

    # Station-specific bias
    for station, bias in report.station_bias.items():
        if abs(bias) > 0.10:
            direction = "overconfident" if bias > 0 else "underconfident"
            recs.append(
                f"Station {station} is {direction} (bias={bias:+.3f}). "
                f"Consider station-specific sigma adjustment."
            )

    if report.sharpness < 0.01:
        recs.append(
            "LOW sharpness — model predictions cluster near 50%. "
            "Increase physics weight or use tighter entry window."
        )

    if report.resolution < 0.005:
        recs.append(
            "LOW resolution — model can't distinguish easy from hard contracts. "
            "Consider adding features or improving trend extrapolation."
        )


def _infer_station(ticker: str) -> str | None:
    """Infer ASOS station from ticker string."""
    ticker_lower = ticker.lower()
    station_map = {
        "chi": "KORD",
        "ord": "KORD",
        "nyc": "KJFK",
        "jfk": "KJFK",
        "den": "KDEN",
        "lax": "KLAX",
        "hou": "KIAH",
        "iah": "KIAH",
    }
    for keyword, station in station_map.items():
        if keyword in ticker_lower:
            return station
    return None


async def main() -> None:
    parser = argparse.ArgumentParser(description="Run calibration analysis")
    parser.add_argument("--start", required=True, help="Start date (YYYY-MM-DD)")
    parser.add_argument("--end", required=True, help="End date (YYYY-MM-DD)")
    parser.add_argument("--type", help="Signal type filter (weather, crypto)")
    args = parser.parse_args()

    start = datetime.strptime(args.start, "%Y-%m-%d").replace(tzinfo=timezone.utc)
    end = datetime.strptime(args.end, "%Y-%m-%d").replace(tzinfo=timezone.utc)
    signal_types = [args.type] if args.type else None

    settings = get_settings()
    pool = await asyncpg.create_pool(settings.database_url, min_size=1, max_size=3)

    try:
        # Build tables and evaluators
        sigma_table = await build_sigma_table(pool)
        climo_table = await build_climo_table(pool)

        registry = EvaluatorRegistry()
        registry.register(
            "weather",
            WeatherSignalEvaluator(sigma_table=sigma_table, climo_table=climo_table),
        )
        registry.register("crypto", CryptoSignalEvaluator())

        # Run backtest
        backtester = Backtester(pool, registry)
        result = await backtester.run(start, end, signal_types)

        if not result.signals:
            logger.warning("no_signals", msg="Backtest produced no signals. Need more historical data.")
            return

        # Write calibration data to DB
        await write_calibration_data(pool, result.signals, sigma_table)

        # Analyze calibration
        report = await analyze_calibration(
            pool,
            result.signals,
            signal_type=args.type,
            period=f"{args.start} to {args.end}",
        )

        # Print results
        print(json.dumps(report.summary(), indent=2))

    finally:
        await pool.close()


if __name__ == "__main__":
    asyncio.run(main())
