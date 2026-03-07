"""BE-3.3: Continuous Data Collector Daemon.

Always-on process that passively builds historical dataset by collecting
ASOS observations, Kalshi market snapshots, and BTC price/vol data.
"""

from __future__ import annotations

import asyncio
import signal
from datetime import datetime, timezone

import asyncpg
import httpx
import structlog

from config import Settings, get_settings
from data.binance_ws import BinanceFeed
from data.mesonet import ASOSObservation, fetch_all_stations

logger = structlog.get_logger()


class CollectorDaemon:
    """Always-on process that builds historical dataset passively.

    Three independent collection loops run concurrently:
    - ASOS weather observations (every 60s)
    - Kalshi market snapshots for near-settlement contracts (every 60s)
    - BTC spot + volatility (every 60s)

    Each loop is fault-isolated — one failing does not stop the others.
    """

    def __init__(self, settings: Settings | None = None) -> None:
        self.settings = settings or get_settings()
        self.pool: asyncpg.Pool | None = None
        self.btc_feed = BinanceFeed(ws_url=self.settings.binance_ws_url)
        self._shutdown = asyncio.Event()

    async def run(self) -> None:
        """Start all collection loops. Runs until shutdown signal."""
        self.pool = await asyncpg.create_pool(
            self.settings.database_url,
            min_size=2,
            max_size=5,
        )
        logger.info("collector_started", stations=self.settings.asos_stations)

        try:
            await asyncio.gather(
                self.collect_asos_loop(),
                self.collect_market_snapshots_loop(),
                self.collect_btc_loop(),
                self.btc_feed.connect(),
                return_exceptions=True,
            )
        finally:
            if self.pool:
                await self.pool.close()
            logger.info("collector_stopped")

    async def collect_asos_loop(self) -> None:
        """Every 60s: fetch all station observations -> insert into observations table."""
        interval = self.settings.collection_interval_seconds

        while not self._shutdown.is_set():
            try:
                observations = await fetch_all_stations(
                    self.settings.asos_stations,
                    mesonet_base_url=self.settings.mesonet_base_url,
                )

                if observations:
                    await self._insert_asos_observations(observations)
                    logger.info(
                        "asos_collected",
                        count=len(observations),
                        stale=[s for s, o in observations.items() if o.is_stale],
                    )

            except Exception:
                logger.exception("asos_collection_error")

            await self._sleep_or_shutdown(interval)

    async def collect_market_snapshots_loop(self) -> None:
        """Every 60s: for contracts within 30 min of settlement, snapshot prices."""
        interval = self.settings.collection_interval_seconds

        transport = httpx.AsyncHTTPTransport(retries=2)
        async with httpx.AsyncClient(
            transport=transport, timeout=httpx.Timeout(15.0)
        ) as client:
            while not self._shutdown.is_set():
                try:
                    await self._collect_market_snapshots(client)
                except Exception:
                    logger.exception("market_snapshot_error")

                await self._sleep_or_shutdown(interval)

    async def collect_btc_loop(self) -> None:
        """Every 60s: write BTC spot + vol to observations table."""
        interval = self.settings.collection_interval_seconds

        # Wait a bit for the WS feed to establish
        await self._sleep_or_shutdown(5)

        while not self._shutdown.is_set():
            try:
                state = self.btc_feed.get_state()
                if state.spot_price > 0:
                    await self._insert_btc_observation(state)
                    logger.info(
                        "btc_collected",
                        spot=state.spot_price,
                        vol_30m=state.realized_vol_30m,
                        bars=state.bars_count,
                    )
            except Exception:
                logger.exception("btc_collection_error")

            await self._sleep_or_shutdown(interval)

    # --- Database writes ---

    async def _insert_asos_observations(
        self, observations: dict[str, ASOSObservation]
    ) -> None:
        """Batch insert ASOS observations using COPY for efficiency."""
        assert self.pool is not None

        records = [
            (
                "asos",
                ob.station,
                ob.observed_at,
                ob.temperature_f,
                ob.wind_speed_kts,
                ob.wind_gust_kts,
                ob.precip_inch,
                None,  # btc_spot
                None,  # btc_vol_30m
            )
            for ob in observations.values()
        ]

        async with self.pool.acquire() as conn:
            await conn.copy_records_to_table(
                "observations",
                records=records,
                columns=[
                    "source",
                    "station",
                    "observed_at",
                    "temperature_f",
                    "wind_speed_kts",
                    "wind_gust_kts",
                    "precip_inch",
                    "btc_spot",
                    "btc_vol_30m",
                ],
            )

    async def _insert_btc_observation(self, state) -> None:
        assert self.pool is not None

        now = datetime.now(timezone.utc)
        async with self.pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO observations (source, observed_at, btc_spot, btc_vol_30m)
                VALUES ($1, $2, $3, $4)
                """,
                "binance",
                now,
                state.spot_price,
                state.realized_vol_30m,
            )

    async def _collect_market_snapshots(self, client: httpx.AsyncClient) -> None:
        """Snapshot prices for contracts settling within 30 minutes."""
        assert self.pool is not None

        # Query contracts settling within 30 minutes
        async with self.pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT ticker, settlement_time
                FROM contracts
                WHERE status = 'active'
                  AND settlement_time > now()
                  AND settlement_time < now() + interval '30 minutes'
                """
            )

        if not rows:
            return

        tickers = [r["ticker"] for r in rows]
        settlement_times = {r["ticker"]: r["settlement_time"] for r in rows}

        # Fetch current prices from Kalshi REST API
        snapshots = []
        for ticker in tickers:
            try:
                resp = await client.get(
                    f"{self.settings.kalshi_base_url}/trade-api/v2/markets/{ticker}",
                )
                if resp.status_code != 200:
                    logger.warning(
                        "market_snapshot_fetch_failed",
                        ticker=ticker,
                        status=resp.status_code,
                    )
                    continue

                market = resp.json().get("market", {})
                now = datetime.now(timezone.utc)
                settlement = settlement_times[ticker]
                minutes_to_settlement = (
                    settlement - now
                ).total_seconds() / 60.0

                yes_price = float(market.get("yes_price", 0)) / 100.0
                no_price = float(market.get("no_price", 0)) / 100.0

                snapshots.append((
                    ticker,
                    yes_price,
                    no_price,
                    abs(yes_price - no_price),
                    None,  # best_bid (from orderbook, not REST)
                    None,  # best_ask
                    None,  # bid_depth
                    None,  # ask_depth
                    minutes_to_settlement,
                ))
            except Exception:
                logger.exception("market_snapshot_single_error", ticker=ticker)

        if snapshots:
            async with self.pool.acquire() as conn:
                await conn.copy_records_to_table(
                    "market_snapshots",
                    records=snapshots,
                    columns=[
                        "ticker",
                        "yes_price",
                        "no_price",
                        "spread",
                        "best_bid",
                        "best_ask",
                        "bid_depth",
                        "ask_depth",
                        "minutes_to_settlement",
                    ],
                )
            logger.info("market_snapshots_collected", count=len(snapshots))

    async def _sleep_or_shutdown(self, seconds: float) -> None:
        """Sleep for the given duration, returning early if shutdown is signalled."""
        try:
            await asyncio.wait_for(self._shutdown.wait(), timeout=seconds)
        except asyncio.TimeoutError:
            pass

    def shutdown(self) -> None:
        """Signal the daemon to stop."""
        self._shutdown.set()
        self.btc_feed.stop()


async def main() -> None:
    settings = get_settings()
    daemon = CollectorDaemon(settings)

    loop = asyncio.get_running_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        loop.add_signal_handler(sig, daemon.shutdown)

    await daemon.run()


if __name__ == "__main__":
    asyncio.run(main())
