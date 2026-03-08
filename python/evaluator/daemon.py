"""Signal Evaluation Daemon — orchestrates evaluators on a 10s cycle.

Connects the collector's data to signal evaluators and publishes results
via NATS, DB, and Redis. This is the core trading loop.
"""

from __future__ import annotations

import asyncio
import json
import signal
from datetime import datetime, timezone

import asyncpg
import nats
import redis.asyncio as aioredis
import structlog

from config import Settings, get_settings
from data.binance_ws import BinanceFeed
from data.mesonet import fetch_all_stations
from models.physics import build_climo_table, build_sigma_table
from signals.crypto import CryptoSignalEvaluator, load_blackout_windows
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
        self.btc_feed = BinanceFeed(ws_url=self.settings.binance_ws_url)
        self.registry = EvaluatorRegistry()
        self.notifier = DiscordNotifier(self.settings.discord_webhook_url or None)
        self._shutdown = asyncio.Event()
        self._last_blackout_refresh: datetime | None = None
        self._signal_outcomes: list[bool] = []  # Rolling window of recent outcomes

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

        # Register evaluators
        weather_eval = WeatherSignalEvaluator(
            sigma_table=sigma_table,
            climo_table=climo_table,
        )
        crypto_eval = CryptoSignalEvaluator()
        blackout_windows = await load_blackout_windows(self.pool)
        crypto_eval.set_blackout_windows(blackout_windows)

        self.registry.register("weather", weather_eval)
        self.registry.register("crypto", crypto_eval)

        await self.notifier.start()

        logger.info(
            "evaluator_started",
            evaluators=self.registry.types(),
            interval=self.settings.evaluation_interval_seconds,
        )

        try:
            await asyncio.gather(
                self._evaluation_loop(),
                self._fast_evaluation_loop(),
                self.btc_feed.connect(),
                return_exceptions=True,
            )
        finally:
            await self._cleanup()

    async def _evaluation_loop(self) -> None:
        """Main loop: every N seconds, evaluate all near-settlement contracts."""
        interval = self.settings.evaluation_interval_seconds

        # Wait for BTC feed to establish
        await self._sleep_or_shutdown(5)

        while not self._shutdown.is_set():
            try:
                await self._evaluate_all()
            except Exception:
                logger.exception("evaluation_cycle_error")

            await self._sleep_or_shutdown(interval)

    def _get_recent_accuracy(self) -> tuple[float | None, int]:
        """Get accuracy from recent signal outcomes."""
        if not self._signal_outcomes:
            return None, 0
        recent = self._signal_outcomes[-50:]
        return sum(recent) / len(recent), len(recent)

    async def _fast_evaluation_loop(self) -> None:
        """Fast-path loop: every 2s, evaluate contracts settling within 5 minutes."""
        # Wait for BTC feed + initial data
        await self._sleep_or_shutdown(10)

        while not self._shutdown.is_set():
            try:
                await self._evaluate_near_expiry()
            except Exception:
                logger.exception("fast_evaluation_cycle_error")
            await self._sleep_or_shutdown(2)

    async def _evaluate_near_expiry(self) -> None:
        """Evaluate only contracts within 5 minutes of settlement (fast path)."""
        assert self.pool is not None
        assert self.publisher is not None

        async with self.pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT ticker, category, city, station, threshold,
                       settlement_time, status
                FROM contracts
                WHERE status = 'active'
                  AND settlement_time > now()
                  AND settlement_time < now() + interval '5 minutes'
                """
            )

        if not rows:
            return

        btc_state = self.btc_feed.get_state()

        # Use Redis orderbook data (fresher than DB)
        orderbook_overrides = await self._fetch_redis_orderbooks(
            [r["ticker"] for r in rows]
        )

        # Get latest snapshots for fallback
        async with self.pool.acquire() as conn:
            snapshot_rows = await conn.fetch(
                """
                SELECT DISTINCT ON (ticker)
                    ticker, yes_price, no_price, spread,
                    best_bid, best_ask, bid_depth, ask_depth
                FROM market_snapshots
                WHERE captured_at > now() - interval '2 minutes'
                ORDER BY ticker, captured_at DESC
                """
            )
        snapshots = {r["ticker"]: r for r in snapshot_rows}

        for row in rows:
            ticker = row["ticker"]
            contract = Contract(
                ticker=ticker,
                category=row["category"] or "",
                city=row["city"],
                station=row["station"],
                threshold=row["threshold"],
                settlement_time=row["settlement_time"],
                status=row["status"],
            )

            orderbook = self._build_orderbook(ticker, snapshots, orderbook_overrides)
            if orderbook is None:
                continue

            signal_type = self._infer_signal_type(contract)
            evaluator = self.registry.get(signal_type)
            if evaluator is None:
                continue

            try:
                if signal_type == "weather":
                    asos_obs = await fetch_all_stations(
                        self.settings.asos_stations,
                        mesonet_base_url=self.settings.mesonet_base_url,
                    )
                    station = contract.station or "KORD"
                    obs = asos_obs.get(station) if asos_obs else None
                    if obs is None:
                        continue
                    sig, rej, state = evaluator.evaluate(
                        contract=contract,
                        observation=obs,
                        orderbook=orderbook,
                    )
                elif signal_type == "crypto":
                    vol = btc_state.ewma_vol_30m or btc_state.realized_vol_30m
                    sig, rej, state = evaluator.evaluate(
                        contract=contract,
                        spot_price=btc_state.spot_price,
                        realized_vol=vol,
                        btc_last_updated=btc_state.last_updated,
                        orderbook=orderbook,
                    )
                else:
                    continue

                await self.publisher.publish_model_state(state)
                if sig is not None:
                    await self.publisher.publish(sig)
                    await self.notifier.notify_signal(sig)
                elif rej is not None:
                    await self.publisher.publish_rejection(rej)
            except Exception:
                logger.exception("fast_evaluate_error", ticker=ticker)

    async def _evaluate_all(self) -> None:
        """Evaluate all active contracts nearing settlement."""
        assert self.pool is not None
        assert self.publisher is not None

        # Refresh blackout windows every 5 minutes
        await self._maybe_refresh_blackouts()

        # Fetch contracts settling within 30 minutes
        async with self.pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT ticker, category, city, station, threshold,
                       settlement_time, status
                FROM contracts
                WHERE status = 'active'
                  AND settlement_time > now()
                  AND settlement_time < now() + interval '30 minutes'
                """
            )

        if not rows:
            return

        # Fetch latest data sources
        asos_obs = await fetch_all_stations(
            self.settings.asos_stations,
            mesonet_base_url=self.settings.mesonet_base_url,
        )
        btc_state = self.btc_feed.get_state()

        # Fetch latest market snapshots for orderbook state
        async with self.pool.acquire() as conn:
            snapshot_rows = await conn.fetch(
                """
                SELECT DISTINCT ON (ticker)
                    ticker, yes_price, no_price, spread,
                    best_bid, best_ask, bid_depth, ask_depth
                FROM market_snapshots
                WHERE captured_at > now() - interval '5 minutes'
                ORDER BY ticker, captured_at DESC
                """
            )

        snapshots = {r["ticker"]: r for r in snapshot_rows}

        # Try to get orderbook data from Redis (written by Rust)
        orderbook_overrides = await self._fetch_redis_orderbooks(
            [r["ticker"] for r in rows]
        )

        for row in rows:
            ticker = row["ticker"]
            contract = Contract(
                ticker=ticker,
                category=row["category"] or "",
                city=row["city"],
                station=row["station"],
                threshold=row["threshold"],
                settlement_time=row["settlement_time"],
                status=row["status"],
            )

            # Build orderbook state — prefer Redis (real-time), fall back to snapshot
            orderbook = self._build_orderbook(ticker, snapshots, orderbook_overrides)
            if orderbook is None:
                continue

            # Determine signal type from category
            signal_type = self._infer_signal_type(contract)
            evaluator = self.registry.get(signal_type)
            if evaluator is None:
                continue

            try:
                if signal_type == "weather":
                    station = contract.station or "KORD"
                    obs = asos_obs.get(station) if asos_obs else None
                    if obs is None:
                        continue
                    sig, rej, state = evaluator.evaluate(
                        contract=contract,
                        observation=obs,
                        orderbook=orderbook,
                    )
                elif signal_type == "crypto":
                    vol = btc_state.ewma_vol_30m or btc_state.realized_vol_30m
                    sig, rej, state = evaluator.evaluate(
                        contract=contract,
                        spot_price=btc_state.spot_price,
                        realized_vol=vol,
                        btc_last_updated=btc_state.last_updated,
                        orderbook=orderbook,
                    )
                else:
                    continue

                # Publish results
                await self.publisher.publish_model_state(state)
                if sig is not None:
                    await self.publisher.publish(sig)
                    await self.notifier.notify_signal(sig)
                elif rej is not None:
                    await self.publisher.publish_rejection(rej)

            except Exception:
                logger.exception("evaluate_contract_error", ticker=ticker)
                await self.notifier.notify_error(
                    "evaluate_contract_error", {"ticker": ticker}
                )

    async def _maybe_refresh_blackouts(self) -> None:
        """Refresh blackout windows from DB every 5 minutes."""
        now = datetime.now(timezone.utc)
        if self._last_blackout_refresh and (now - self._last_blackout_refresh).total_seconds() < 300:
            return
        crypto_eval = self.registry.get("crypto")
        if crypto_eval is not None and self.pool is not None:
            from signals.crypto import load_blackout_windows
            windows = await load_blackout_windows(self.pool)
            crypto_eval.set_blackout_windows(windows)
            self._last_blackout_refresh = now
            logger.debug("blackout_windows_refreshed", count=len(windows))

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

        keys = [f"orderbook:{t}" for t in tickers]
        try:
            values = await self.redis.mget(keys)
        except Exception:
            logger.warning("redis_orderbook_mget_failed", exc_info=True)
            return {}

        result = {}
        for ticker, raw in zip(tickers, values):
            if raw:
                try:
                    result[ticker] = json.loads(raw)
                except (json.JSONDecodeError, TypeError):
                    logger.warning("redis_orderbook_parse_failed", ticker=ticker)
        return result

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
        self.btc_feed.stop()


async def main() -> None:
    settings = get_settings()
    daemon = EvaluationDaemon(settings)

    loop = asyncio.get_running_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        loop.add_signal_handler(sig, daemon.shutdown)

    await daemon.run()


if __name__ == "__main__":
    import uvloop
    uvloop.install()
    asyncio.run(main())
