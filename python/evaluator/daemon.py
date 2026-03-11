"""Signal Evaluation Daemon — orchestrates evaluators on a 10s cycle.

Connects the collector's data to signal evaluators and publishes results
via NATS, DB, and Redis. This is the core trading loop.
"""

from __future__ import annotations

import asyncio
import signal
import time as time_mod
from datetime import datetime, time, timezone

import asyncpg
import nats
import redis.asyncio as aioredis
import structlog

from analytics.aggregator import aggregate_daily_performance
from config import Settings, get_settings
from data.mesonet import fetch_all_stations
from models.physics import build_climo_table, build_sigma_table, build_station_calibration
from rules.resolver import ContractRulesResolver
from signals.notifier import DiscordNotifier
from signals.publisher import SignalPublisher
from signals.registry import EvaluatorRegistry
from signals.types import Contract, OrderbookState
from signals.weather import WeatherSignalEvaluator

logger = structlog.get_logger()


class EvaluationDaemon:
    """Runs evaluators on a cycle, publishing signals via NATS."""

    def __init__(self, settings: Settings | None = None) -> None:
        self.settings = settings or get_settings()
        self.pool: asyncpg.Pool | None = None
        self.nc: nats.NATS | None = None
        self.redis: aioredis.Redis | None = None
        self.publisher: SignalPublisher | None = None
        self.registry = EvaluatorRegistry()
        self.rules_resolver = ContractRulesResolver()
        self.notifier = DiscordNotifier(self.settings.discord_webhook_url or None)
        self._shutdown = asyncio.Event()

    async def run(self) -> None:
        """Initialize connections, register evaluators, run evaluation loop."""
        self.pool = await asyncpg.create_pool(
            self.settings.database_url, min_size=2, max_size=5
        )
        self.nc = await nats.connect(self.settings.nats_url)
        self.redis = aioredis.from_url(self.settings.redis_url)

        self.publisher = SignalPublisher(
            nats_client=self.nc,
            db_pool=self.pool,
            redis_client=self.redis,
        )

        # Build lookup tables from historical data
        sigma_table = await build_sigma_table(self.pool)
        climo_table = await build_climo_table(self.pool)
        station_calibration = await build_station_calibration(self.pool)

        # Register evaluators (crypto moved to Rust event-driven evaluator in Phase 3)
        weather_eval = WeatherSignalEvaluator(
            sigma_table=sigma_table,
            climo_table=climo_table,
            station_calibration=station_calibration,
        )

        self._sigma_table = sigma_table
        self._climo_table = climo_table
        self._station_calibration = station_calibration
        self._weather_eval = weather_eval
        self._last_cal_refresh = time_mod.monotonic()

        self.registry.register("weather", weather_eval)

        # Load contract rules
        try:
            await self.rules_resolver.load(self.pool)
        except Exception:
            logger.warning("rules_load_failed", exc_info=True)

        await self.notifier.start()

        logger.info(
            "evaluator_started",
            evaluators=self.registry.types(),
            interval=self.settings.evaluation_interval_seconds,
        )

        try:
            await self._evaluation_loop()
        finally:
            await self._cleanup()

    async def _maybe_refresh_calibration(self) -> None:
        """Reload calibration tables if stale (every 15 minutes)."""
        if (time_mod.monotonic() - self._last_cal_refresh) > 900:
            self._sigma_table, self._climo_table, self._station_calibration = (
                await asyncio.gather(
                    build_sigma_table(self.pool),
                    build_climo_table(self.pool),
                    build_station_calibration(self.pool),
                )
            )
            # Update the weather evaluator with fresh tables
            self._weather_eval.sigma_table = self._sigma_table
            self._weather_eval.climo_table = self._climo_table
            self._weather_eval.station_calibration = self._station_calibration
            self._last_cal_refresh = time_mod.monotonic()
            logger.info("calibration_tables_refreshed")

    async def _evaluation_loop(self) -> None:
        """Main loop: every N seconds, evaluate all near-settlement contracts."""
        interval = self.settings.evaluation_interval_seconds
        self._last_aggregation_date = None

        # Wait for Rust feeds to establish and write to Redis
        await self._sleep_or_shutdown(5)

        while not self._shutdown.is_set():
            try:
                await self._maybe_refresh_calibration()
                await self._evaluate_all()
            except Exception:
                logger.exception("evaluation_cycle_error")

            # Daily aggregation at midnight UTC
            await self._maybe_run_daily_aggregation()

            await self._sleep_or_shutdown(interval)

    async def _fetch_active_contracts(self):
        """Fetch contracts settling within 30 minutes."""
        async with self.pool.acquire() as conn:
            return await conn.fetch(
                """
                SELECT ticker, category, city, station, threshold,
                       settlement_time, status
                FROM contracts
                WHERE status = 'active'
                  AND settlement_time > now()
                  AND settlement_time < now() + interval '30 minutes'
                """
            )

    async def _fetch_market_snapshots(self):
        """Fetch latest market snapshots for orderbook state."""
        async with self.pool.acquire() as conn:
            return await conn.fetch(
                """
                SELECT DISTINCT ON (ticker)
                    ticker, yes_price, no_price, spread,
                    best_bid, best_ask, bid_depth, ask_depth
                FROM market_snapshots
                WHERE captured_at > now() - interval '5 minutes'
                ORDER BY ticker, captured_at DESC
                """
            )

    async def _evaluate_all(self) -> None:
        """Evaluate all active contracts nearing settlement."""
        assert self.pool is not None
        assert self.publisher is not None

        t0 = time_mod.monotonic()

        # Steps 1, 2, 3 are independent — run in parallel
        rows, asos_obs, snapshot_rows = await asyncio.gather(
            self._fetch_active_contracts(),
            fetch_all_stations(
                self.settings.asos_stations,
                mesonet_base_url=self.settings.mesonet_base_url,
            ),
            self._fetch_market_snapshots(),
        )

        if not rows:
            return

        snapshots = {r["ticker"]: r for r in snapshot_rows}

        # Step 4 depends on rows (needs ticker list)
        orderbook_overrides = await self._fetch_redis_orderbooks(
            [r["ticker"] for r in rows]
        )

        t_io = time_mod.monotonic()

        for row in rows:
            ticker = row["ticker"]

            # Attach resolved rules if available
            rules = self.rules_resolver.get(ticker)

            contract = Contract(
                ticker=ticker,
                category=row["category"] or "",
                city=row["city"],
                station=row["station"],
                threshold=row["threshold"],
                settlement_time=row["settlement_time"],
                status=row["status"],
                rules=rules,
            )

            # Use rules-based station/threshold if contract fields are missing
            if rules:
                if contract.station is None and rules.settlement_station:
                    contract.station = rules.settlement_station
                if contract.threshold is None and rules.strike:
                    contract.threshold = rules.strike

            # Build orderbook state — prefer Redis (real-time), fall back to snapshot
            orderbook = self._build_orderbook(ticker, snapshots, orderbook_overrides)
            if orderbook is None:
                continue

            # Determine signal type: prefer rules, fall back to category inference
            signal_type = rules.signal_type if rules else self._infer_signal_type(contract)
            evaluator = self.registry.get(signal_type)
            if evaluator is None:
                continue

            try:
                if signal_type == "weather":
                    station = contract.station or "KORD"
                    obs = asos_obs.get(station) if asos_obs else None
                    if obs is None:
                        asyncio.create_task(self._write_decision_log(
                            ticker=ticker, outcome="skipped",
                            rejection_reason="no_observation_data",
                        ))
                        continue
                    sig, rej, state = evaluator.evaluate(
                        contract=contract,
                        observation=obs,
                        orderbook=orderbook,
                    )
                else:
                    # Crypto evaluation moved to Rust (Phase 3)
                    continue

                # Publish results
                await self.publisher.publish_model_state(state)

                # Persist full evaluation for replay engine (8.0d)
                await self.publisher.publish_model_evaluation(
                    ticker=contract.ticker,
                    signal_type=signal_type,
                    model_prob=state.model_prob if state else None,
                    market_price=orderbook.mid_price,
                    edge=sig.edge if sig else (rej.edge if rej else None),
                    direction=sig.direction if sig else None,
                    inputs={"station": contract.station, "threshold": contract.threshold},
                    components=sig.model_components if (sig and hasattr(sig, "model_components")) else None,
                    confidence=state.confidence if state else None,
                    acted_on=sig is not None,
                )

                if sig is not None:
                    await self.publisher.publish(sig)
                    await self.notifier.notify_signal(sig)
                elif rej is not None:
                    await self.publisher.publish_rejection(rej)

                # Decision audit log
                mins_left = (contract.settlement_time - datetime.now(timezone.utc)).total_seconds() / 60.0
                if sig is not None:
                    asyncio.create_task(self._write_decision_log(
                        ticker=ticker, outcome="signal",
                        model_prob=state.model_prob if state else None,
                        market_price=orderbook.mid_price,
                        edge=sig.edge,
                        direction=sig.direction,
                        minutes_remaining=mins_left,
                        confidence=state.confidence if state else None,
                    ))
                elif rej is not None:
                    asyncio.create_task(self._write_decision_log(
                        ticker=ticker, outcome="rejected",
                        rejection_reason=rej.rejection_reason if hasattr(rej, "rejection_reason") else None,
                        model_prob=state.model_prob if state else None,
                        market_price=orderbook.mid_price,
                        edge=rej.edge if hasattr(rej, "edge") else None,
                        minutes_remaining=mins_left,
                        confidence=state.confidence if state else None,
                    ))
                else:
                    asyncio.create_task(self._write_decision_log(
                        ticker=ticker, outcome="skipped",
                        minutes_remaining=mins_left,
                    ))

            except Exception:
                logger.exception("evaluate_contract_error", ticker=ticker)
                await self.notifier.notify_error(
                    "evaluate_contract_error", {"ticker": ticker}
                )

        t_eval = time_mod.monotonic()
        logger.info(
            "eval_cycle_complete",
            contracts=len(rows),
            io_ms=round((t_io - t0) * 1000, 1),
            eval_ms=round((t_eval - t_io) * 1000, 1),
            total_ms=round((t_eval - t0) * 1000, 1),
        )

    async def _write_decision_log(
        self,
        ticker: str,
        outcome: str,
        rejection_reason: str | None = None,
        model_prob: float | None = None,
        market_price: float | None = None,
        edge: float | None = None,
        direction: str | None = None,
        minutes_remaining: float | None = None,
        confidence: float | None = None,
        signal_id: int | None = None,
    ) -> None:
        """Fire-and-forget write to decision_log for Grafana observability."""
        if self.pool is None:
            return
        try:
            async with self.pool.acquire() as conn:
                await conn.execute(
                    """
                    INSERT INTO decision_log (
                        ticker, signal_type, source, outcome, rejection_reason,
                        model_prob, market_price, edge, direction,
                        minutes_remaining, confidence, signal_id
                    ) VALUES ($1, 'weather', 'python', $2, $3,
                              $4, $5, $6, $7, $8, $9, $10)
                    """,
                    ticker, outcome, rejection_reason,
                    model_prob, market_price, edge, direction,
                    minutes_remaining, confidence, signal_id,
                )
        except Exception:
            logger.debug("decision_log_write_failed", ticker=ticker)

    def _infer_signal_type(self, contract: Contract) -> str:
        """Infer signal type from contract category."""
        cat = (contract.category or "").lower()
        if any(kw in cat for kw in ("temperature", "weather", "wind", "rain", "snow")):
            return "weather"
        if any(kw in cat for kw in ("bitcoin", "btc", "crypto")):
            return "crypto"
        return contract.category or "unknown"

    def _build_orderbook(
        self,
        ticker: str,
        snapshots: dict,
        redis_overrides: dict,
    ) -> OrderbookState | None:
        """Build OrderbookState preferring Redis real-time data over DB snapshots."""
        redis_data = redis_overrides.get(ticker)
        if redis_data:
            return OrderbookState(
                mid_price=redis_data.get("mid_price", 0.5),
                spread=redis_data.get("spread", 0.0),
                best_bid=redis_data.get("best_bid"),
                best_ask=redis_data.get("best_ask"),
                bid_depth=redis_data.get("bid_depth", 0),
                ask_depth=redis_data.get("ask_depth", 0),
            )

        snap = snapshots.get(ticker)
        if snap:
            yes_price = float(snap["yes_price"] or 0)
            no_price = float(snap["no_price"] or 0)
            mid = (yes_price + (1.0 - no_price)) / 2.0 if yes_price > 0 else 0.5
            return OrderbookState(
                mid_price=mid,
                spread=float(snap["spread"] or 0),
                best_bid=float(snap["best_bid"]) if snap["best_bid"] else None,
                best_ask=float(snap["best_ask"]) if snap["best_ask"] else None,
                bid_depth=int(snap["bid_depth"] or 0),
                ask_depth=int(snap["ask_depth"] or 0),
            )

        return None

    async def _fetch_redis_orderbooks(self, tickers: list[str]) -> dict:
        """Fetch orderbook summaries from Redis (written by Rust WS feed)."""
        if self.redis is None or not tickers:
            return {}

        import json

        keys = [f"orderbook:{t}" for t in tickers]
        try:
            values = await self.redis.mget(keys)
        except Exception:
            return {}

        result = {}
        for ticker, raw in zip(tickers, values):
            if raw:
                try:
                    result[ticker] = json.loads(raw)
                except Exception:
                    pass
        return result

    async def _maybe_run_daily_aggregation(self) -> None:
        """Run daily strategy aggregation once per day after midnight UTC."""
        now = datetime.now(timezone.utc)
        today = now.date()

        # Only run once per day, after midnight UTC
        if self._last_aggregation_date == today:
            return
        if now.time() < time(0, 1):
            # Wait until at least 00:01 to let final signals settle
            return

        if self.pool is not None:
            try:
                from datetime import timedelta

                yesterday = today - timedelta(days=1)
                await aggregate_daily_performance(self.pool, yesterday)
                self._last_aggregation_date = today
            except Exception:
                logger.exception("daily_aggregation_error")

    async def _sleep_or_shutdown(self, seconds: float) -> None:
        try:
            await asyncio.wait_for(self._shutdown.wait(), timeout=seconds)
        except asyncio.TimeoutError:
            pass

    async def _cleanup(self) -> None:
        await self.notifier.close()
        if self.nc:
            await self.nc.close()
        if self.redis:
            await self.redis.close()
        if self.pool:
            await self.pool.close()
        logger.info("evaluator_stopped")

    def shutdown(self) -> None:
        self._shutdown.set()


async def main() -> None:
    settings = get_settings()
    daemon = EvaluationDaemon(settings)

    for sig in (signal.SIGINT, signal.SIGTERM):
        signal.signal(sig, lambda s, f: daemon.shutdown())

    await daemon.run()


if __name__ == "__main__":
    asyncio.run(main())
