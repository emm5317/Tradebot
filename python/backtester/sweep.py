"""Parameter sweep framework for model optimization.

Runs the backtester across a grid of hyperparameters, stores each run
in backtest_runs, and ranks configurations by Brier score / P&L.

Usage:
    python -m backtester.sweep --start 2026-01-01 --end 2026-03-01 --type weather
    python -m backtester.sweep --start 2026-01-01 --end 2026-03-01 --type weather --walk-forward 14
"""

from __future__ import annotations

import argparse
import asyncio
import itertools
import json
import math
import os
import uuid
from concurrent.futures import ProcessPoolExecutor, as_completed
from dataclasses import asdict, dataclass, field
from datetime import date, datetime, timedelta, timezone
from typing import Any

import asyncpg
import structlog

from backtester.costs import FeeModel
from backtester.metrics import AdvancedMetrics, TradeRecord, compute_advanced_metrics
from config import get_settings
from data.mesonet import ASOSObservation
from models.physics import StationCalibration, build_climo_table, build_sigma_table
from models.weather_fv import WeatherFairValue, WeatherState, compute_weather_fair_value
from signals.types import Contract, OrderbookState

logger = structlog.get_logger()


# ── Default parameter grids ──────────────────────────────────────

WEATHER_PARAM_GRID: dict[str, list[Any]] = {
    "sigma_scale": [0.8, 0.9, 1.0, 1.1, 1.2],
    "weight_physics": [0.35, 0.40, 0.45, 0.50],
    "weight_hrrr": [0.20, 0.25, 0.30],
    "weight_trend": [0.10, 0.15, 0.20, 0.25],
    "min_edge": [0.03, 0.05, 0.07, 0.10],
}

CRYPTO_THRESHOLD_GRID: dict[str, list[Any]] = {
    "min_edge": [0.02, 0.03, 0.05, 0.07, 0.10],
    "min_confidence": [0.3, 0.4, 0.5, 0.6],
    "min_kelly": [0.01, 0.02, 0.03, 0.05],
    "kelly_multiplier": [0.25, 0.50, 0.75, 1.0],
}


@dataclass
class SweepResult:
    """Result from a single parameter configuration."""

    run_id: str
    params: dict[str, Any]
    total_contracts: int = 0
    total_signals: int = 0
    accuracy: float = 0.0
    brier_score: float = 0.0
    simulated_pnl_cents: int = 0
    win_count: int = 0
    loss_count: int = 0
    avg_edge: float = 0.0
    calibration: dict[str, dict] = field(default_factory=dict)
    signals_detail: list[dict] = field(default_factory=list)
    # Advanced metrics (Phase 8.2/8.3)
    log_loss: float = 0.0
    sharpe_ratio: float = 0.0
    sortino_ratio: float = 0.0
    max_drawdown_cents: float = 0.0
    max_drawdown_pct: float = 0.0
    profit_factor: float = 0.0
    ece: float = 0.0
    fee_total_cents: float = 0.0
    win_streak: int = 0
    loss_streak: int = 0


@dataclass
class WalkForwardSplit:
    """A single train/validation split for walk-forward analysis."""

    train_start: date
    train_end: date
    val_start: date
    val_end: date


class ParameterSweep:
    """Grid search over backtester hyperparameters."""

    def __init__(
        self,
        pool: asyncpg.Pool,
        fee_model: FeeModel | None = None,
        time_decay_lambda: float = 0.0,
        multi_signal: bool = False,
    ) -> None:
        self.pool = pool
        self.fee_model = fee_model or FeeModel()
        self.time_decay_lambda = time_decay_lambda
        self.multi_signal = multi_signal

    async def sweep_weather(
        self,
        start: date,
        end: date,
        param_grid: dict[str, list[Any]] | None = None,
        max_combos: int = 200,
        description: str | None = None,
    ) -> list[SweepResult]:
        """Run weather model over all parameter combinations."""
        grid = param_grid or WEATHER_PARAM_GRID
        combos = _generate_combinations(grid, max_combos)

        logger.info(
            "sweep_start",
            signal_type="weather",
            combinations=len(combos),
            start=str(start),
            end=str(end),
        )

        # Pre-fetch all data once
        contracts = await self._fetch_settled_contracts(start, end, "weather")
        if not contracts:
            logger.warning("sweep_no_contracts")
            return []

        results = []
        for i, params in enumerate(combos):
            # Ensure weights sum to 1.0 (normalize the three we're sweeping)
            w_p = params.get("weight_physics", 0.45)
            w_h = params.get("weight_hrrr", 0.25)
            w_t = params.get("weight_trend", 0.20)
            w_c = max(0.0, 1.0 - w_p - w_h - w_t)
            if w_c < 0:
                continue  # invalid combination

            params["weight_climo"] = round(w_c, 2)

            result = await self._evaluate_weather_params(
                contracts, params, start, end
            )
            results.append(result)

            # Store to DB
            await self._store_run(
                result, "weather", start, end, description
            )

            if (i + 1) % 10 == 0:
                logger.info(
                    "sweep_progress",
                    completed=i + 1,
                    total=len(combos),
                    best_brier=min(r.brier_score for r in results if r.brier_score > 0) if any(r.brier_score > 0 for r in results) else None,
                )

        # Sort by Brier score (lower is better)
        results.sort(key=lambda r: r.brier_score if r.brier_score > 0 else float("inf"))

        logger.info(
            "sweep_complete",
            total_runs=len(results),
            best_brier=results[0].brier_score if results else None,
            best_params=results[0].params if results else None,
        )

        return results

    async def walk_forward(
        self,
        full_start: date,
        full_end: date,
        window_days: int = 14,
        signal_type: str = "weather",
        param_grid: dict[str, list[Any]] | None = None,
        max_combos: int = 100,
    ) -> list[dict]:
        """Walk-forward optimization: train on window, validate on next window.

        For each split:
        1. Grid search on training window → find best params
        2. Evaluate best params on validation window
        3. Record out-of-sample performance
        """
        splits = _generate_walk_forward_splits(full_start, full_end, window_days)

        logger.info(
            "walk_forward_start",
            splits=len(splits),
            window_days=window_days,
        )

        oos_results = []
        for split in splits:
            # Train: grid search
            if signal_type == "weather":
                train_results = await self.sweep_weather(
                    split.train_start,
                    split.train_end,
                    param_grid=param_grid,
                    max_combos=max_combos,
                    description=f"WF train {split.train_start}→{split.train_end}",
                )
            elif signal_type == "crypto":
                train_results = await self.sweep_crypto(
                    split.train_start,
                    split.train_end,
                    param_grid=param_grid,
                    max_combos=max_combos,
                    description=f"WF train {split.train_start}→{split.train_end}",
                )
            else:
                continue

            if not train_results:
                continue

            best_params = train_results[0].params

            # Validate: single run with best params
            if signal_type == "weather":
                val_contracts = await self._fetch_settled_contracts(
                    split.val_start, split.val_end, signal_type
                )
                if not val_contracts:
                    continue
                val_result = await self._evaluate_weather_params(
                    val_contracts, best_params, split.val_start, split.val_end
                )
            else:
                val_signals = await self._fetch_crypto_signals(
                    split.val_start, split.val_end
                )
                if not val_signals:
                    continue
                val_result = self._evaluate_crypto_thresholds(
                    val_signals, best_params
                )

            await self._store_run(
                val_result,
                signal_type,
                split.val_start,
                split.val_end,
                description=f"WF validate {split.val_start}→{split.val_end}",
                train_start=split.train_start,
                train_end=split.train_end,
                baseline_run_id=train_results[0].run_id,
            )

            oos_results.append({
                "split": f"{split.val_start}→{split.val_end}",
                "train_brier": train_results[0].brier_score,
                "val_brier": val_result.brier_score,
                "train_pnl": train_results[0].simulated_pnl_cents,
                "val_pnl": val_result.simulated_pnl_cents,
                "params": best_params,
                "overfit_ratio": (
                    val_result.brier_score / train_results[0].brier_score
                    if train_results[0].brier_score > 0
                    else None
                ),
            })

            logger.info(
                "walk_forward_split_done",
                split=f"{split.val_start}→{split.val_end}",
                train_brier=f"{train_results[0].brier_score:.4f}",
                val_brier=f"{val_result.brier_score:.4f}",
            )

        return oos_results

    # ── Crypto sweep ──────────────────────────────────────────────

    async def sweep_crypto(
        self,
        start: date,
        end: date,
        param_grid: dict[str, list[Any]] | None = None,
        max_combos: int = 200,
        description: str | None = None,
    ) -> list[SweepResult]:
        """Sweep crypto threshold/sizing params against stored signals.

        Instead of re-computing crypto FV (done in Rust), this filters
        historical signals by threshold parameters and simulates P&L.
        """
        grid = param_grid or CRYPTO_THRESHOLD_GRID
        combos = _generate_combinations(grid, max_combos)

        logger.info(
            "sweep_start",
            signal_type="crypto",
            combinations=len(combos),
            start=str(start),
            end=str(end),
        )

        # Pre-fetch all crypto signals with outcomes
        signals = await self._fetch_crypto_signals(start, end)
        if not signals:
            logger.warning("sweep_no_crypto_signals")
            return []

        results = []
        for i, params in enumerate(combos):
            result = self._evaluate_crypto_thresholds(signals, params)
            results.append(result)

            await self._store_run(result, "crypto", start, end, description)

            if (i + 1) % 10 == 0:
                logger.info(
                    "sweep_progress",
                    completed=i + 1,
                    total=len(combos),
                    best_brier=min(
                        r.brier_score for r in results if r.brier_score > 0
                    ) if any(r.brier_score > 0 for r in results) else None,
                )

        results.sort(key=lambda r: r.brier_score if r.brier_score > 0 else float("inf"))

        logger.info(
            "sweep_complete",
            total_runs=len(results),
            best_brier=results[0].brier_score if results else None,
            best_params=results[0].params if results else None,
        )

        return results

    def _evaluate_crypto_thresholds(
        self,
        signals: list[dict],
        params: dict[str, Any],
    ) -> SweepResult:
        """Filter stored crypto signals by thresholds and compute metrics."""
        run_id = str(uuid.uuid4())
        result = SweepResult(run_id=run_id, params=params)

        min_edge = params.get("min_edge", 0.05)
        min_confidence = params.get("min_confidence", 0.3)
        min_kelly = params.get("min_kelly", 0.01)
        kelly_mult = params.get("kelly_multiplier", 0.5)

        brier_sum = 0.0
        brier_count = 0
        pnl = 0.0
        wins = 0
        losses = 0
        edge_sum = 0.0
        trade_records: list[TradeRecord] = []
        tickers_seen: set[str] = set()

        for sig in signals:
            # Apply threshold filters
            edge = float(sig["edge"])
            kelly = float(sig["kelly_fraction"])
            confidence = float(sig.get("confidence") or 0.5)

            if abs(edge) < min_edge:
                continue
            if confidence < min_confidence:
                continue
            if kelly < min_kelly:
                continue

            # In single-signal mode, skip duplicate tickers
            ticker = sig["ticker"]
            if not self.multi_signal:
                if ticker in tickers_seen:
                    continue
                tickers_seen.add(ticker)

            settled_yes = sig["settled_yes"]
            if settled_yes is None:
                continue

            direction = sig["direction"]
            model_prob = float(sig["model_prob"])
            market_price = float(sig["market_price"])
            outcome = 1.0 if settled_yes else 0.0

            if direction == "yes":
                p = model_prob
                won = settled_yes
            else:
                p = 1.0 - model_prob
                won = not settled_yes

            brier_sum += (p - outcome) ** 2
            brier_count += 1

            # P&L with scaled Kelly and fees
            fee = self.fee_model.round_trip_cost(market_price)
            scaled_kelly = kelly * kelly_mult

            if direction == "yes":
                sig_pnl = (outcome - market_price) * 100 * scaled_kelly - fee
            else:
                sig_pnl = (market_price - outcome) * 100 * scaled_kelly - fee

            if won:
                wins += 1
            else:
                losses += 1

            pnl += sig_pnl
            edge_sum += abs(edge)
            result.total_signals += 1

            trade_records.append(TradeRecord(
                settlement_date=sig["settlement_time"].date() if sig.get("settlement_time") else sig["created_at"].date(),
                direction=direction,
                model_prob=model_prob,
                market_price=market_price,
                edge=abs(edge),
                settled_yes=settled_yes,
                pnl_cents=sig_pnl,
                fee_cents=fee,
            ))

        result.total_contracts = len(tickers_seen) if not self.multi_signal else brier_count

        if brier_count > 0:
            result.brier_score = brier_sum / brier_count
            result.accuracy = wins / brier_count
            result.avg_edge = edge_sum / brier_count

        result.simulated_pnl_cents = int(pnl)
        result.win_count = wins
        result.loss_count = losses

        if trade_records:
            adv = compute_advanced_metrics(trade_records, self.time_decay_lambda)
            result.log_loss = adv.log_loss
            result.sharpe_ratio = adv.sharpe_ratio
            result.sortino_ratio = adv.sortino_ratio
            result.max_drawdown_cents = adv.max_drawdown_cents
            result.max_drawdown_pct = adv.max_drawdown_pct
            result.profit_factor = adv.profit_factor
            result.ece = adv.expected_calibration_error
            result.fee_total_cents = adv.total_fees_cents
            result.win_streak = adv.win_streak
            result.loss_streak = adv.loss_streak

        return result

    async def _fetch_crypto_signals(self, start: date, end: date) -> list[dict]:
        """Fetch crypto signals with settled outcomes for threshold sweep."""
        async with self.pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT s.ticker, s.signal_type, s.direction, s.model_prob,
                       s.market_price, s.edge, s.kelly_fraction,
                       s.minutes_remaining, s.created_at,
                       c.settled_yes, c.settlement_time,
                       me.confidence
                FROM signals s
                JOIN contracts c ON s.ticker = c.ticker
                LEFT JOIN model_evaluations me
                    ON me.ticker = s.ticker
                    AND me.created_at BETWEEN s.created_at - interval '1 minute'
                                         AND s.created_at + interval '1 minute'
                WHERE s.signal_type = 'crypto'
                  AND c.settled_yes IS NOT NULL
                  AND s.created_at >= $1
                  AND s.created_at <= $2
                ORDER BY s.created_at
                """,
                datetime.combine(start, datetime.min.time(), tzinfo=timezone.utc),
                datetime.combine(end, datetime.max.time(), tzinfo=timezone.utc),
            )
        return [dict(r) for r in rows]

    # ── Parallel sweep ────────────────────────────────────────────

    async def sweep_weather_parallel(
        self,
        start: date,
        end: date,
        param_grid: dict[str, list[Any]] | None = None,
        max_combos: int = 200,
        max_workers: int | None = None,
        description: str | None = None,
    ) -> list[SweepResult]:
        """Run weather sweep in parallel using ProcessPoolExecutor.

        Pre-fetches all contracts once and distributes param combos
        across worker processes.
        """
        grid = param_grid or WEATHER_PARAM_GRID
        combos = _generate_combinations(grid, max_combos)
        workers = max_workers or min(4, os.cpu_count() or 1)

        logger.info(
            "sweep_parallel_start",
            signal_type="weather",
            combinations=len(combos),
            workers=workers,
        )

        contracts = await self._fetch_settled_contracts(start, end, "weather")
        if not contracts:
            logger.warning("sweep_no_contracts")
            return []

        # Normalize weights and filter invalid combos
        valid_combos = []
        for params in combos:
            w_p = params.get("weight_physics", 0.45)
            w_h = params.get("weight_hrrr", 0.25)
            w_t = params.get("weight_trend", 0.20)
            w_c = max(0.0, 1.0 - w_p - w_h - w_t)
            if w_c < 0:
                continue
            params["weight_climo"] = round(w_c, 2)
            valid_combos.append(params)

        # Run evaluations across workers — each combo gets its own
        # async evaluation via the event loop
        sem = asyncio.Semaphore(workers)

        async def eval_with_limit(params: dict) -> SweepResult:
            async with sem:
                return await self._evaluate_weather_params(
                    contracts, params, start, end
                )

        tasks = [eval_with_limit(p) for p in valid_combos]
        results = await asyncio.gather(*tasks)
        results = list(results)

        # Store all results
        for result in results:
            await self._store_run(result, "weather", start, end, description)

        results.sort(key=lambda r: r.brier_score if r.brier_score > 0 else float("inf"))

        logger.info(
            "sweep_parallel_complete",
            total_runs=len(results),
            best_brier=results[0].brier_score if results else None,
        )

        return results

    # ── Internal: weather evaluation ─────────────────────────────

    async def _evaluate_weather_params(
        self,
        contracts: list[dict],
        params: dict[str, Any],
        start: date,
        end: date,
    ) -> SweepResult:
        """Evaluate weather model with specific params across all contracts."""
        run_id = str(uuid.uuid4())
        result = SweepResult(run_id=run_id, params=params)

        sigma_scale = params.get("sigma_scale", 1.0)
        min_edge = params.get("min_edge", 0.05)
        w_p = params.get("weight_physics", 0.45)
        w_h = params.get("weight_hrrr", 0.25)
        w_t = params.get("weight_trend", 0.20)
        w_c = params.get("weight_climo", 0.10)

        cal = StationCalibration(
            sigma_10min=0.3 * sigma_scale,
            hrrr_bias_f=0.0,
            hrrr_skill=0.7,
            rounding_bias=0.0,
            weights=(w_p, w_h, w_t, w_c),
        )

        brier_sum = 0.0
        brier_count = 0
        pnl = 0
        wins = 0
        losses = 0
        edge_sum = 0.0
        trade_records: list[TradeRecord] = []
        buckets: dict[str, list[tuple[float, bool]]] = {
            f"{i*10}-{(i+1)*10}%": [] for i in range(10)
        }

        for contract in contracts:
            settled_yes = contract["settled_yes"]
            if settled_yes is None:
                continue

            result.total_contracts += 1

            snapshots = await self._fetch_snapshots(
                contract["ticker"], contract["settlement_time"]
            )
            if not snapshots:
                continue

            contract_type = (
                "weather_max" if "high" in (contract["title"] or "").lower()
                or "above" in (contract["title"] or "").lower()
                else "weather_min"
            )

            for snap in snapshots:
                obs = await self._fetch_nearest_observation(
                    contract["station"] or "KORD", snap["captured_at"]
                )
                if obs is None or obs.temperature_f is None:
                    continue

                metar = await self._fetch_nearest_metar(
                    contract["station"] or "KORD", snap["captured_at"]
                )
                hrrr = await self._fetch_hrrr_forecasts(
                    contract["station"] or "KORD", snap["captured_at"]
                )

                minutes_remaining = max(
                    0.0,
                    (contract["settlement_time"] - snap["captured_at"]).total_seconds() / 60.0,
                )

                fv = compute_weather_fair_value(
                    contract_type=contract_type,
                    strike_f=float(contract["threshold"]),
                    current_temp_f=obs.temperature_f,
                    minutes_remaining=minutes_remaining,
                    station_cal=cal,
                    metar_temp_c=metar["temp_c"] if metar else None,
                    hrrr_forecast_temps_f=hrrr,
                )

                market_price = float(snap["yes_price"] or 0.5)
                edge = fv.probability - market_price
                abs_edge = abs(edge)

                if abs_edge < min_edge:
                    continue

                # Record signal — use directional probability for Brier (8.0e)
                direction = "yes" if edge > 0 else "no"
                outcome = 1.0 if settled_yes else 0.0

                # Directional probability: P(our bet wins)
                if direction == "yes":
                    p = fv.probability
                    won = settled_yes
                else:
                    p = 1.0 - fv.probability
                    won = not settled_yes

                brier_sum += (p - outcome) ** 2
                brier_count += 1

                # Transaction cost
                fee = self.fee_model.round_trip_cost(market_price)

                if direction == "yes":
                    sig_pnl = (outcome - market_price) * 100 - fee
                else:
                    sig_pnl = (market_price - outcome) * 100 - fee

                if won:
                    wins += 1
                else:
                    losses += 1

                pnl += sig_pnl
                edge_sum += abs_edge
                result.total_signals += 1

                trade_records.append(TradeRecord(
                    settlement_date=contract["settlement_time"].date(),
                    direction=direction,
                    model_prob=fv.probability,
                    market_price=market_price,
                    edge=abs_edge,
                    settled_yes=settled_yes,
                    pnl_cents=sig_pnl,
                    fee_cents=fee,
                ))

                # Calibration buckets on directional probability
                bucket_idx = min(int(p * 10), 9)
                bucket_key = f"{bucket_idx*10}-{(bucket_idx+1)*10}%"
                buckets[bucket_key].append((p, bool(won)))

                # In default mode, take only first signal per contract
                if not self.multi_signal:
                    break

        if brier_count > 0:
            result.brier_score = brier_sum / brier_count
            result.accuracy = wins / brier_count
            result.avg_edge = edge_sum / brier_count

        result.simulated_pnl_cents = int(pnl)
        result.win_count = wins
        result.loss_count = losses

        for key, entries in buckets.items():
            if entries:
                avg_pred = sum(e[0] for e in entries) / len(entries)
                win_rate = sum(1 for e in entries if e[1]) / len(entries)
                result.calibration[key] = {
                    "count": len(entries),
                    "avg_predicted": round(avg_pred, 3),
                    "actual_win_rate": round(win_rate, 3),
                }

        # Compute advanced metrics from trade records
        if trade_records:
            adv = compute_advanced_metrics(trade_records, self.time_decay_lambda)
            result.log_loss = adv.log_loss
            result.sharpe_ratio = adv.sharpe_ratio
            result.sortino_ratio = adv.sortino_ratio
            result.max_drawdown_cents = adv.max_drawdown_cents
            result.max_drawdown_pct = adv.max_drawdown_pct
            result.profit_factor = adv.profit_factor
            result.ece = adv.expected_calibration_error
            result.fee_total_cents = adv.total_fees_cents
            result.win_streak = adv.win_streak
            result.loss_streak = adv.loss_streak

        return result

    # ── Data fetching ────────────────────────────────────────────

    async def _fetch_settled_contracts(
        self, start: date, end: date, signal_type: str
    ) -> list[dict]:
        async with self.pool.acquire() as conn:
            type_filter = (
                "('temperature', 'weather', 'wind', 'rain', 'snow')"
                if signal_type == "weather"
                else "('bitcoin', 'btc', 'crypto')"
            )
            rows = await conn.fetch(
                f"""
                SELECT ticker, title, category, city, station, threshold,
                       settlement_time, settled_yes, close_price
                FROM contracts
                WHERE settlement_time >= $1
                  AND settlement_time <= $2
                  AND settled_yes IS NOT NULL
                  AND LOWER(COALESCE(category, '')) SIMILAR TO '%({'|'.join(
                      ['temperature','weather','wind','rain','snow']
                      if signal_type == 'weather'
                      else ['bitcoin','btc','crypto']
                  )})%'
                ORDER BY settlement_time
                """,
                datetime.combine(start, datetime.min.time(), tzinfo=timezone.utc),
                datetime.combine(end, datetime.max.time(), tzinfo=timezone.utc),
            )
        return [dict(r) for r in rows]

    async def _fetch_snapshots(
        self, ticker: str, settlement_time: datetime
    ) -> list[dict]:
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

    async def _fetch_nearest_observation(
        self, station: str, at: datetime
    ) -> ASOSObservation | None:
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

    async def _fetch_nearest_metar(
        self, station: str, at: datetime
    ) -> dict | None:
        async with self.pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                SELECT temp_c, dewpoint_c, max_temp_6hr_c, min_temp_6hr_c
                FROM metar_observations
                WHERE station = $1
                  AND observed_at <= $2
                  AND observed_at > $2 - interval '30 minutes'
                ORDER BY observed_at DESC
                LIMIT 1
                """,
                station,
                at,
            )
        return dict(row) if row else None

    async def _fetch_hrrr_forecasts(
        self, station: str, at: datetime
    ) -> list[float] | None:
        async with self.pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT temp_2m_f
                FROM hrrr_forecasts
                WHERE station = $1
                  AND forecast_time > $2
                  AND forecast_time < $2 + interval '12 hours'
                  AND run_time <= $2
                  AND run_time > $2 - interval '6 hours'
                ORDER BY forecast_time
                """,
                station,
                at,
            )
        if not rows:
            return None
        return [float(r["temp_2m_f"]) for r in rows if r["temp_2m_f"] is not None]

    # ── DB persistence ───────────────────────────────────────────

    async def _store_run(
        self,
        result: SweepResult,
        signal_type: str,
        start: date,
        end: date,
        description: str | None = None,
        train_start: date | None = None,
        train_end: date | None = None,
        baseline_run_id: str | None = None,
        station: str | None = None,
    ) -> None:
        async with self.pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO backtest_runs (
                    run_id, signal_type, start_date, end_date,
                    params, description,
                    total_contracts, total_signals, accuracy,
                    brier_score, log_loss, simulated_pnl_cents,
                    win_count, loss_count, avg_edge,
                    calibration,
                    train_start, train_end,
                    validation_start, validation_end,
                    baseline_run_id, station,
                    sharpe_ratio, sortino_ratio,
                    max_drawdown_cents, profit_factor, ece,
                    fee_total_cents, win_streak, loss_streak,
                    time_decay_lambda
                ) VALUES (
                    $1, $2, $3, $4,
                    $5, $6,
                    $7, $8, $9,
                    $10, $11, $12,
                    $13, $14, $15,
                    $16,
                    $17, $18,
                    $19, $20,
                    $21, $22,
                    $23, $24,
                    $25, $26, $27,
                    $28, $29, $30,
                    $31
                )
                """,
                uuid.UUID(result.run_id),
                signal_type,
                start,
                end,
                json.dumps(result.params),
                description,
                result.total_contracts,
                result.total_signals,
                result.accuracy,
                result.brier_score,
                result.log_loss,
                result.simulated_pnl_cents,
                result.win_count,
                result.loss_count,
                result.avg_edge,
                json.dumps(result.calibration),
                train_start,
                train_end,
                start if train_start else None,
                end if train_start else None,
                uuid.UUID(baseline_run_id) if baseline_run_id else None,
                station,
                result.sharpe_ratio,
                result.sortino_ratio,
                int(result.max_drawdown_cents),
                result.profit_factor,
                result.ece,
                int(result.fee_total_cents),
                result.win_streak,
                result.loss_streak,
                self.time_decay_lambda,
            )


# ── Utilities ────────────────────────────────────────────────────

def _generate_combinations(
    grid: dict[str, list[Any]], max_combos: int
) -> list[dict[str, Any]]:
    """Generate all parameter combinations, capped at max_combos."""
    keys = list(grid.keys())
    values = list(grid.values())
    combos = []

    for vals in itertools.product(*values):
        combo = dict(zip(keys, vals))
        combos.append(combo)
        if len(combos) >= max_combos:
            break

    return combos


def _generate_walk_forward_splits(
    start: date, end: date, window_days: int
) -> list[WalkForwardSplit]:
    """Generate non-overlapping train/validation splits."""
    splits = []
    current = start

    while current + timedelta(days=window_days * 2) <= end:
        train_start = current
        train_end = current + timedelta(days=window_days - 1)
        val_start = current + timedelta(days=window_days)
        val_end = current + timedelta(days=window_days * 2 - 1)

        splits.append(WalkForwardSplit(
            train_start=train_start,
            train_end=train_end,
            val_start=val_start,
            val_end=val_end,
        ))

        current += timedelta(days=window_days)

    return splits


# ── Reporting ────────────────────────────────────────────────────

async def print_leaderboard(pool: asyncpg.Pool, signal_type: str, top_n: int = 20) -> None:
    """Print top N runs ranked by Brier score."""
    async with pool.acquire() as conn:
        rows = await conn.fetch(
            """
            SELECT run_id, params, brier_score, accuracy, simulated_pnl_cents,
                   total_signals, win_count, loss_count, description,
                   train_start, validation_start,
                   sharpe_ratio, max_drawdown_cents, profit_factor,
                   fee_total_cents, ece
            FROM backtest_runs
            WHERE signal_type = $1
              AND brier_score IS NOT NULL
              AND brier_score > 0
            ORDER BY brier_score ASC
            LIMIT $2
            """,
            signal_type,
            top_n,
        )

    print(f"\n{'='*100}")
    print(f"  Top {top_n} {signal_type} backtest runs (by Brier score)")
    print(f"{'='*100}")
    print(
        f"  {'Rank':>4}  {'Brier':>7}  {'Acc':>6}  {'PnL':>8}  {'Fees':>6}  "
        f"{'Sharpe':>7}  {'MaxDD':>7}  {'PF':>5}  {'Signals':>7}  Params"
    )
    print(
        f"  {'-'*4}  {'-'*7}  {'-'*6}  {'-'*8}  {'-'*6}  "
        f"{'-'*7}  {'-'*7}  {'-'*5}  {'-'*7}  {'-'*30}"
    )

    for i, row in enumerate(rows, 1):
        params = json.loads(row["params"]) if isinstance(row["params"], str) else row["params"]
        param_str = ", ".join(f"{k}={v}" for k, v in params.items())
        desc = f" [{row['description']}]" if row["description"] else ""
        is_wf = " (WF)" if row["train_start"] else ""
        sharpe = row["sharpe_ratio"] or 0.0
        max_dd = (row["max_drawdown_cents"] or 0) / 100
        pf = row["profit_factor"] or 0.0
        fees = (row["fee_total_cents"] or 0) / 100

        print(
            f"  {i:>4}  {row['brier_score']:>7.4f}  {row['accuracy']:>5.1%}  "
            f"${row['simulated_pnl_cents']/100:>7.2f}  ${fees:>5.2f}  "
            f"{sharpe:>7.2f}  ${max_dd:>6.2f}  {pf:>5.2f}  "
            f"{row['total_signals']:>7}  {param_str}{is_wf}{desc}"
        )

    print()


# ── CLI entry point ──────────────────────────────────────────────

async def main() -> None:
    parser = argparse.ArgumentParser(description="Parameter sweep for model optimization")
    parser.add_argument("--start", required=True, help="Start date (YYYY-MM-DD)")
    parser.add_argument("--end", required=True, help="End date (YYYY-MM-DD)")
    parser.add_argument("--type", default="weather", help="Signal type (weather, crypto)")
    parser.add_argument("--walk-forward", type=int, default=0, help="Walk-forward window in days (0=disabled)")
    parser.add_argument("--max-combos", type=int, default=200, help="Max parameter combinations")
    parser.add_argument("--leaderboard", action="store_true", help="Just print leaderboard")
    parser.add_argument("--time-decay", type=float, default=0.0, help="Exponential time-decay lambda (0=off)")
    parser.add_argument("--no-fees", action="store_true", help="Disable transaction cost modeling")
    parser.add_argument("--multi-signal", action="store_true", help="Evaluate all snapshots per contract")
    parser.add_argument("--workers", type=int, default=0, help="Parallel workers (0=sequential)")
    args = parser.parse_args()

    settings = get_settings()
    pool = await asyncpg.create_pool(settings.database_url, min_size=2, max_size=5)

    start = date.fromisoformat(args.start)
    end = date.fromisoformat(args.end)
    fee_model = FeeModel() if not args.no_fees else FeeModel(fee_type="flat", flat_fee_cents=0)
    sweep = ParameterSweep(
        pool,
        fee_model=fee_model,
        time_decay_lambda=args.time_decay,
        multi_signal=args.multi_signal,
    )

    if args.leaderboard:
        await print_leaderboard(pool, args.type)
    elif args.walk_forward > 0:
        results = await sweep.walk_forward(
            start, end,
            window_days=args.walk_forward,
            signal_type=args.type,
            max_combos=args.max_combos,
        )
        print("\nWalk-Forward Results:")
        print(json.dumps(results, indent=2, default=str))
    else:
        if args.type == "weather":
            if args.workers > 0:
                results = await sweep.sweep_weather_parallel(
                    start, end,
                    max_combos=args.max_combos,
                    max_workers=args.workers,
                )
            else:
                results = await sweep.sweep_weather(
                    start, end, max_combos=args.max_combos
                )
        elif args.type == "crypto":
            results = await sweep.sweep_crypto(
                start, end, max_combos=args.max_combos
            )
        else:
            print(f"Unknown signal type: {args.type}")
            await pool.close()
            return

        await print_leaderboard(pool, args.type)

    await pool.close()


if __name__ == "__main__":
    asyncio.run(main())
