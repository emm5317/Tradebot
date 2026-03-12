"""Backtesting engine — replays historical data through evaluators.

Queries settled contracts with known outcomes, simulates time progression,
and computes model accuracy, calibration, and simulated P&L.
"""

from __future__ import annotations

import argparse
import asyncio
import json
from dataclasses import dataclass, field
from datetime import UTC, datetime

import asyncpg
import structlog

from backtester.costs import FeeModel
from config import get_settings
from data.mesonet import ASOSObservation
from models.physics import build_climo_table, build_sigma_table
from rules.ticker_parser import _extract_crypto_strike
from signals.crypto import CryptoSignalEvaluator
from signals.registry import EvaluatorRegistry
from signals.types import Contract, OrderbookState
from signals.weather import WeatherSignalEvaluator

logger = structlog.get_logger()


@dataclass
class SignalRecord:
    """A signal that would have fired during backtesting."""

    ticker: str
    signal_type: str
    direction: str
    model_prob: float
    market_price: float
    edge: float
    kelly_fraction: float
    minutes_remaining: float
    actual_outcome: bool | None = None


@dataclass
class BacktestResult:
    """Aggregate results from a backtest run."""

    start: datetime
    end: datetime
    total_contracts: int = 0
    total_signals: int = 0
    total_rejections: int = 0
    correct_signals: int = 0
    accuracy: float = 0.0
    brier_score: float = 0.0
    simulated_pnl_cents: float = 0.0
    signals: list[SignalRecord] = field(default_factory=list)
    calibration_buckets: dict[str, dict] = field(default_factory=dict)

    def summary(self) -> dict:
        return {
            "period": f"{self.start.date()} to {self.end.date()}",
            "contracts_evaluated": self.total_contracts,
            "signals_fired": self.total_signals,
            "rejections": self.total_rejections,
            "accuracy": f"{self.accuracy:.1%}",
            "brier_score": f"{self.brier_score:.4f}",
            "simulated_pnl": f"${self.simulated_pnl_cents / 100:.2f}",
            "calibration": self.calibration_buckets,
        }


class Backtester:
    """Replays historical data through registered evaluators."""

    def __init__(
        self,
        pool: asyncpg.Pool,
        registry: EvaluatorRegistry,
        fee_model: FeeModel | None = None,
        multi_signal: bool = False,
    ) -> None:
        self.pool = pool
        self.registry = registry
        self.fee_model = fee_model or FeeModel()
        self.multi_signal = multi_signal

    async def run(
        self,
        start: datetime,
        end: datetime,
        signal_types: list[str] | None = None,
    ) -> BacktestResult:
        """Run backtest over the given time range."""
        result = BacktestResult(start=start, end=end)

        # Fetch settled contracts with known outcomes
        contracts = await self._fetch_settled_contracts(start, end, signal_types)
        result.total_contracts = len(contracts)

        logger.info("backtest_start", contracts=len(contracts), start=str(start), end=str(end))

        _no_snap = 0
        _no_btc = 0
        _rejections: dict[str, int] = {}
        for idx, contract in enumerate(contracts):
            if (idx + 1) % 500 == 0 or idx == 0:
                logger.info(
                    "backtest_progress",
                    contract=idx + 1,
                    total=len(contracts),
                    signals_so_far=result.total_signals,
                    rejections_so_far=result.total_rejections,
                    no_snapshots=_no_snap,
                    no_btc=_no_btc,
                )

            signal_type = self._infer_signal_type(contract)
            evaluator = self.registry.get(signal_type)
            if evaluator is None:
                continue

            # Fetch time-aligned data for this contract
            snapshots = await self._fetch_snapshots(contract["ticker"], contract["settlement_time"])
            if not snapshots:
                _no_snap += 1
                continue

            for snap in snapshots:
                orderbook = OrderbookState(
                    mid_price=float(snap["yes_price"] or 0.5),
                    spread=float(snap["spread"] or 0.0),
                )

                # Parse strike from ticker if threshold not in DB
                threshold = contract["threshold"]
                if threshold is None and signal_type == "crypto":
                    threshold = _extract_crypto_strike(contract["ticker"])

                contract_obj = Contract(
                    ticker=contract["ticker"],
                    category=contract["category"] or "",
                    city=contract["city"],
                    station=contract["station"],
                    threshold=threshold,
                    settlement_time=contract["settlement_time"],
                )

                try:
                    if signal_type == "weather":
                        obs = await self._fetch_nearest_observation(
                            contract["station"] or "KORD",
                            snap["captured_at"],
                        )
                        if obs is None:
                            continue
                        sig, rej, state = evaluator.evaluate(
                            contract=contract_obj,
                            observation=obs,
                            orderbook=orderbook,
                            as_of=snap["captured_at"],
                        )
                    elif signal_type == "crypto":
                        btc = await self._fetch_nearest_btc(snap["captured_at"])
                        if btc is None:
                            _no_btc += 1
                            continue
                        sig, rej, state = evaluator.evaluate(
                            contract=contract_obj,
                            spot_price=float(btc["btc_spot"]),
                            realized_vol=float(btc["btc_vol_30m"]) if btc["btc_vol_30m"] else None,
                            btc_last_updated=btc["observed_at"],
                            as_of=snap["captured_at"],
                            orderbook=orderbook,
                        )
                    else:
                        continue

                    if sig is not None:
                        actual = contract["settled_yes"]
                        record = SignalRecord(
                            ticker=sig.ticker,
                            signal_type=sig.signal_type,
                            direction=sig.direction,
                            model_prob=sig.model_prob,
                            market_price=sig.market_price,
                            edge=sig.edge,
                            kelly_fraction=sig.kelly_fraction,
                            minutes_remaining=sig.minutes_remaining,
                            actual_outcome=actual,
                        )
                        result.signals.append(record)
                        result.total_signals += 1

                        # In default mode, take only first signal per contract
                        if not self.multi_signal:
                            break
                    elif rej is not None:
                        result.total_rejections += 1
                        reason = rej.rejection_reason.split(" ")[0] if rej.rejection_reason else "unknown"
                        _rejections[reason] = _rejections.get(reason, 0) + 1

                except Exception:
                    logger.exception("backtest_eval_error", ticker=contract["ticker"])

        logger.info(
            "backtest_done",
            signals=result.total_signals,
            rejections=result.total_rejections,
            no_snapshots=_no_snap,
            no_btc=_no_btc,
            rejection_reasons=_rejections,
        )
        # Compute aggregate metrics
        self._compute_metrics(result)
        return result

    def _compute_metrics(self, result: BacktestResult) -> None:
        """Compute accuracy, Brier score, calibration, and simulated P&L."""
        if not result.signals:
            return

        brier_sum = 0.0
        correct = 0
        pnl = 0.0

        # Calibration buckets: 0-10%, 10-20%, ..., 90-100%
        buckets: dict[str, list[tuple[float, bool]]] = {}
        for i in range(10):
            buckets[f"{i * 10}-{(i + 1) * 10}%"] = []

        for sig in result.signals:
            if sig.actual_outcome is None:
                continue

            outcome = 1.0 if sig.actual_outcome else 0.0

            # Model's implied probability for the bet direction
            if sig.direction == "yes":
                p = sig.model_prob
                won = sig.actual_outcome
            else:
                p = 1.0 - sig.model_prob
                won = not sig.actual_outcome

            # Brier score (lower is better)
            brier_sum += (p - outcome) ** 2

            # Transaction cost
            fee = self.fee_model.round_trip_cost(sig.market_price)

            if won:
                correct += 1
                pnl += sig.edge * sig.kelly_fraction * 10000 - fee  # cents
            else:
                pnl -= sig.kelly_fraction * 10000 + fee

            # Calibration bucket
            bucket_idx = min(int(p * 10), 9)
            bucket_key = f"{bucket_idx * 10}-{(bucket_idx + 1) * 10}%"
            buckets[bucket_key].append((p, bool(won)))

        n = len([s for s in result.signals if s.actual_outcome is not None])
        if n > 0:
            result.accuracy = correct / n
            result.brier_score = brier_sum / n

        result.simulated_pnl_cents = pnl
        result.correct_signals = correct

        # Calibration summary
        for key, entries in buckets.items():
            if entries:
                avg_pred = sum(e[0] for e in entries) / len(entries)
                win_rate = sum(1 for e in entries if e[1]) / len(entries)
                result.calibration_buckets[key] = {
                    "count": len(entries),
                    "avg_predicted": round(avg_pred, 3),
                    "actual_win_rate": round(win_rate, 3),
                }

    async def _fetch_settled_contracts(
        self, start: datetime, end: datetime, signal_types: list[str] | None
    ) -> list[dict]:
        async with self.pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT ticker, category, city, station, threshold,
                       settlement_time, settled_yes, close_price
                FROM contracts
                WHERE settlement_time >= $1
                  AND settlement_time <= $2
                  AND settled_yes IS NOT NULL
                ORDER BY settlement_time
                """,
                start,
                end,
            )
        contracts = [dict(r) for r in rows]
        if signal_types:
            contracts = [c for c in contracts if self._infer_signal_type(c) in signal_types]
        return contracts

    async def _fetch_snapshots(self, ticker: str, settlement_time: datetime) -> list[dict]:
        async with self.pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT yes_price, no_price, spread, captured_at
                FROM market_snapshots
                WHERE ticker = $1
                  AND captured_at < $2
                  AND captured_at > $2 - interval '30 minutes'
                ORDER BY captured_at
                """,
                ticker,
                settlement_time,
            )
        return [dict(r) for r in rows]

    async def _fetch_nearest_observation(self, station: str, at: datetime) -> ASOSObservation | None:
        async with self.pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                SELECT station, observed_at, temperature_f, wind_speed_kts,
                       wind_gust_kts, precip_inch
                FROM observations
                WHERE source = 'asos' AND station = $1
                  AND observed_at <= $2
                  AND observed_at > $2 - interval '10 minutes'
                ORDER BY observed_at DESC
                LIMIT 1
                """,
                station,
                at,
            )
        if row is None:
            return None

        staleness = (at - row["observed_at"]).total_seconds()
        return ASOSObservation(
            station=row["station"],
            observed_at=row["observed_at"],
            temperature_f=float(row["temperature_f"]) if row["temperature_f"] else None,
            wind_speed_kts=float(row["wind_speed_kts"]) if row["wind_speed_kts"] else None,
            wind_gust_kts=float(row["wind_gust_kts"]) if row["wind_gust_kts"] else None,
            precip_inch=float(row["precip_inch"]) if row["precip_inch"] else None,
            staleness_seconds=staleness,
            is_stale=staleness > 300,
        )

    async def _fetch_nearest_btc(self, at: datetime) -> dict | None:
        async with self.pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                SELECT observed_at, btc_spot, btc_vol_30m
                FROM observations
                WHERE source = 'binance'
                  AND observed_at <= $1
                  AND observed_at > $1 - interval '15 minutes'
                ORDER BY observed_at DESC
                LIMIT 1
                """,
                at,
            )
        return dict(row) if row else None

    def _infer_signal_type(self, contract: dict) -> str:
        cat = (contract.get("category") or "").lower()
        if any(kw in cat for kw in ("temperature", "weather", "wind", "rain", "snow")):
            return "weather"
        if any(kw in cat for kw in ("bitcoin", "btc", "crypto")):
            return "crypto"
        return "unknown"


async def main() -> None:
    parser = argparse.ArgumentParser(description="Run backtest")
    parser.add_argument("--start", required=True, help="Start date (YYYY-MM-DD)")
    parser.add_argument("--end", required=True, help="End date (YYYY-MM-DD)")
    parser.add_argument("--type", help="Signal type filter (weather, crypto)")
    args = parser.parse_args()

    start = datetime.strptime(args.start, "%Y-%m-%d").replace(tzinfo=UTC)
    end = datetime.strptime(args.end, "%Y-%m-%d").replace(tzinfo=UTC)
    signal_types = [args.type] if args.type else None

    settings = get_settings()
    pool = await asyncpg.create_pool(settings.database_url, min_size=1, max_size=3)

    # Build tables and evaluators
    sigma_table = await build_sigma_table(pool)
    climo_table = await build_climo_table(pool)

    registry = EvaluatorRegistry()
    registry.register("weather", WeatherSignalEvaluator(sigma_table=sigma_table, climo_table=climo_table))
    registry.register("crypto", CryptoSignalEvaluator(backtest_mode=True))

    backtester = Backtester(pool, registry)
    result = await backtester.run(start, end, signal_types)

    print(json.dumps(result.summary(), indent=2))

    await pool.close()


if __name__ == "__main__":
    asyncio.run(main())
