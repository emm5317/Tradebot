"""BE-3.3: Continuous Data Collector Daemon.

Always-on process that passively builds historical dataset by collecting
ASOS observations, Kalshi market snapshots, and BTC price/vol data.
"""

from __future__ import annotations

import asyncio
import signal
from datetime import UTC, datetime, timedelta

import asyncpg
import httpx
import structlog

from analytics.settlement_summary import aggregate_settlement_summary
from config import Settings, get_settings
from data.aviationweather import METARObservation, fetch_metar
from data.binance_ws import BinanceFeed
from data.mesonet import ASOSObservation, fetch_all_stations
from data.nws import fetch_all_nws_stations
from data.open_meteo import HRRRForecast, fetch_hrrr_forecasts

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
                self.collect_nws_loop(),
                self.collect_market_snapshots_loop(),
                self.collect_btc_loop(),
                self.collect_crypto_ticks_loop(),
                self.collect_metar_loop(),
                self.collect_hrrr_loop(),
                self.settlement_summary_loop(),
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

    async def collect_nws_loop(self) -> None:
        """Every 60s: fetch all station observations from NWS API -> insert into observations table."""
        interval = self.settings.collection_interval_seconds

        while not self._shutdown.is_set():
            try:
                observations = await fetch_all_nws_stations(
                    self.settings.asos_stations,
                )

                if observations:
                    await self._insert_nws_observations(observations)
                    logger.info(
                        "nws_collected",
                        count=len(observations),
                        stale=[s for s, o in observations.items() if o.is_stale],
                    )

            except Exception:
                logger.exception("nws_collection_error")

            await self._sleep_or_shutdown(interval)

    async def collect_market_snapshots_loop(self) -> None:
        """Every 60s: for contracts within 30 min of settlement, snapshot prices."""
        interval = self.settings.collection_interval_seconds

        transport = httpx.AsyncHTTPTransport(retries=2)
        async with httpx.AsyncClient(transport=transport, timeout=httpx.Timeout(15.0)) as client:
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

    async def collect_metar_loop(self) -> None:
        """Every 60s: fetch METAR observations for settlement-relevant fields."""
        interval = self.settings.collection_interval_seconds

        while not self._shutdown.is_set():
            try:
                observations = await fetch_metar(self.settings.asos_stations, hours=1.0)
                if observations:
                    await self._insert_metar_observations(observations)
                    logger.info("metar_collected", count=len(observations))
            except Exception:
                logger.exception("metar_collection_error")

            await self._sleep_or_shutdown(interval)

    async def collect_hrrr_loop(self) -> None:
        """Every 300s: fetch HRRR forecasts for remaining-day estimation."""
        interval = 300  # 5 minutes

        while not self._shutdown.is_set():
            try:
                forecasts = await fetch_hrrr_forecasts(self.settings.asos_stations, forecast_hours=24)
                if forecasts:
                    await self._insert_hrrr_forecasts(forecasts)
                    logger.info("hrrr_collected", count=len(forecasts))
            except Exception:
                logger.exception("hrrr_collection_error")

            await self._sleep_or_shutdown(interval)

    async def collect_crypto_ticks_loop(self) -> None:
        """Every 60s: write per-venue BTC tick to crypto_ticks for backtesting."""
        interval = self.settings.collection_interval_seconds

        # Wait for WS feed to establish
        await self._sleep_or_shutdown(10)

        while not self._shutdown.is_set():
            try:
                state = self.btc_feed.get_state()
                if state.spot_price > 0:
                    await self._insert_crypto_tick(state)
            except Exception:
                logger.exception("crypto_tick_error")

            await self._sleep_or_shutdown(interval)

    async def _insert_crypto_tick(self, state) -> None:
        """Insert Binance spot tick into crypto_ticks table."""
        assert self.pool is not None

        now = datetime.now(UTC)
        async with self.pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO crypto_ticks (source, symbol, price, observed_at)
                VALUES ($1, $2, $3, $4)
                ON CONFLICT (source, observed_at) DO NOTHING
                """,
                "binance_spot",
                "BTCUSDT",
                state.spot_price,
                now,
            )

    async def settlement_summary_loop(self) -> None:
        """Every hour: aggregate daily settlement summary for yesterday and today."""
        interval = 3600  # 1 hour

        # Initial delay to let other loops establish data
        await self._sleep_or_shutdown(30)

        while not self._shutdown.is_set():
            try:
                assert self.pool is not None
                today = datetime.now(UTC).date()
                yesterday = today - timedelta(days=1)
                # Aggregate both yesterday (catch stragglers) and today (running)
                await aggregate_settlement_summary(self.pool, yesterday)
                await aggregate_settlement_summary(self.pool, today)
                logger.info("settlement_summary_aggregated", yesterday=str(yesterday), today=str(today))
            except Exception:
                logger.exception("settlement_summary_error")

            await self._sleep_or_shutdown(interval)

    # --- Database writes ---

    async def _insert_asos_observations(self, observations: dict[str, ASOSObservation]) -> None:
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

    async def _insert_nws_observations(self, observations: dict[str, ASOSObservation]) -> None:
        """Batch insert NWS observations using COPY for efficiency."""
        assert self.pool is not None

        records = [
            (
                "nws",
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

        now = datetime.now(UTC)
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

    async def _insert_metar_observations(self, observations: list[METARObservation]) -> None:
        """Insert METAR observations into metar_observations table."""
        assert self.pool is not None

        async with self.pool.acquire() as conn:
            for obs in observations:
                try:
                    await conn.execute(
                        """
                        INSERT INTO metar_observations (
                            station, observed_at, temp_c, dewpoint_c,
                            wind_speed_kts, wind_gust_kts, altimeter_inhg,
                            visibility_mi, wx_string,
                            max_temp_6hr_c, min_temp_6hr_c,
                            max_temp_24hr_c, min_temp_24hr_c, raw_metar
                        ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)
                        ON CONFLICT (station, observed_at) DO NOTHING
                        """,
                        obs.station,
                        obs.observed_at,
                        obs.temp_c,
                        obs.dewpoint_c,
                        obs.wind_speed_kts,
                        obs.wind_gust_kts,
                        obs.altimeter_inhg,
                        obs.visibility_mi,
                        obs.wx_string,
                        obs.max_temp_6hr_c,
                        obs.min_temp_6hr_c,
                        obs.max_temp_24hr_c,
                        obs.min_temp_24hr_c,
                        obs.raw_metar,
                    )
                except Exception:
                    logger.debug("metar_insert_skipped", station=obs.station)

    async def _insert_hrrr_forecasts(self, forecasts: list[HRRRForecast]) -> None:
        """Insert HRRR forecasts into hrrr_forecasts table."""
        assert self.pool is not None

        async with self.pool.acquire() as conn:
            for fc in forecasts:
                try:
                    await conn.execute(
                        """
                        INSERT INTO hrrr_forecasts (
                            station, forecast_time, run_time,
                            temp_2m_f, temp_2m_c, wind_10m_kts, precip_mm
                        ) VALUES ($1,$2,$3,$4,$5,$6,$7)
                        ON CONFLICT (station, forecast_time, run_time) DO NOTHING
                        """,
                        fc.station,
                        fc.forecast_time,
                        fc.run_time,
                        fc.temp_2m_f,
                        fc.temp_2m_c,
                        fc.wind_10m_kts,
                        fc.precip_mm,
                    )
                except Exception:
                    logger.debug("hrrr_insert_skipped", station=fc.station)

    async def _collect_market_snapshots(self, client: httpx.AsyncClient) -> None:
        """Snapshot prices for contracts settling within 30 minutes.

        Fetches concurrently with a semaphore to respect rate limits.
        """
        assert self.pool is not None

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

        settlement_times = {r["ticker"]: r["settlement_time"] for r in rows}
        tickers = list(settlement_times.keys())
        sem = asyncio.Semaphore(self.settings.max_concurrent_snapshots)

        async def _fetch_one(ticker: str) -> tuple | None:
            async with sem:
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
                        return None

                    market = resp.json().get("market", {})
                    now = datetime.now(UTC)
                    settlement = settlement_times[ticker]
                    minutes_to_settlement = (settlement - now).total_seconds() / 60.0

                    # New API: dollar-based fields (strings)
                    yes_bid = float(market.get("yes_bid_dollars", 0))
                    yes_ask = float(market.get("yes_ask_dollars", 0))
                    no_bid = float(market.get("no_bid_dollars", 0))
                    no_ask = float(market.get("no_ask_dollars", 0))
                    # Fallback to legacy integer fields (cents)
                    if yes_bid == 0 and yes_ask == 0:
                        yes_bid = float(market.get("yes_price", 0)) / 100.0
                        no_bid = float(market.get("no_price", 0)) / 100.0
                        yes_ask = yes_bid
                        no_ask = no_bid

                    yes_price = (yes_bid + yes_ask) / 2.0 if (yes_bid + yes_ask) > 0 else 0.0
                    no_price = (no_bid + no_ask) / 2.0 if (no_bid + no_ask) > 0 else 0.0
                    spread = yes_ask - yes_bid if yes_ask >= yes_bid else 0.0

                    return (
                        ticker,
                        yes_price,
                        no_price,
                        spread,
                        yes_bid,   # best_bid
                        yes_ask,   # best_ask
                        None,  # bid_depth
                        None,  # ask_depth
                        minutes_to_settlement,
                    )
                except Exception:
                    logger.exception("market_snapshot_single_error", ticker=ticker)
                    return None

        results = await asyncio.gather(*[_fetch_one(t) for t in tickers], return_exceptions=True)
        snapshots = [r for r in results if isinstance(r, tuple)]

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
        except TimeoutError:
            pass

    def shutdown(self) -> None:
        """Signal the daemon to stop."""
        self._shutdown.set()
        self.btc_feed.stop()


async def main() -> None:
    settings = get_settings()
    daemon = CollectorDaemon(settings)

    for sig in (signal.SIGINT, signal.SIGTERM):
        signal.signal(sig, lambda s, f: daemon.shutdown())

    await daemon.run()


if __name__ == "__main__":
    asyncio.run(main())
