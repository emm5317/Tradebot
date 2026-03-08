"""Historical data import — bulk backfill ASOS observations + Kalshi settlements.

Pulls historical ASOS 1-minute observations from Iowa State Mesonet's
CSV download service. Pulls settled Kalshi contracts via the existing
history client. Populates the observations and contracts tables with
months of data needed for sigma/climo table building and backtesting.

Usage:
    python -m data.historical_import --months 6
    python -m data.historical_import --months 12 --stations KORD,KJFK
    python -m data.historical_import --kalshi-only --months 3
    python -m data.historical_import --asos-only --months 6
"""

from __future__ import annotations

import argparse
import asyncio
import csv
import io
from datetime import datetime, timedelta, timezone

import asyncpg
import httpx
import structlog

from config import Settings, get_settings
from data.kalshi_history import pull_settlement_history

logger = structlog.get_logger()

# Iowa State Mesonet CSV download endpoint
_MESONET_CSV_URL = "https://mesonet.agron.iastate.edu/cgi-bin/request/asos.py"

# Batch size for DB inserts
_INSERT_BATCH = 500


async def import_asos_history(
    pool: asyncpg.Pool,
    stations: list[str],
    months: int = 6,
    mesonet_base_url: str = "https://mesonet.agron.iastate.edu",
) -> int:
    """Download and import historical ASOS observations from Iowa Mesonet.

    Iowa Mesonet provides free CSV downloads of 1-minute ASOS data going
    back years. We download month-by-month per station, parse the CSV,
    and bulk-insert into the observations table.

    Returns total rows inserted.
    """
    total = 0
    end = datetime.now(timezone.utc)
    start = end - timedelta(days=months * 30)

    for station in stations:
        logger.info("asos_import_start", station=station, months=months)
        count = await _import_station(pool, station, start, end, mesonet_base_url)
        total += count
        logger.info("asos_import_done", station=station, rows=count)

    logger.info("asos_import_complete", total=total, stations=len(stations))
    return total


async def _import_station(
    pool: asyncpg.Pool,
    station: str,
    start: datetime,
    end: datetime,
    mesonet_base_url: str,
) -> int:
    """Download and import ASOS data for a single station."""
    # Build Mesonet CSV request parameters
    params = {
        "station": station,
        "data": "tmpf,sknt,gust,p01i",  # temp, wind, gust, precip
        "tz": "Etc/UTC",
        "format": "onlycomma",
        "latlon": "no",
        "elev": "no",
        "missing": "M",
        "trace": "T",
        "report_type": "1",  # 1-minute ASOS
        "year1": str(start.year),
        "month1": str(start.month),
        "day1": str(start.day),
        "year2": str(end.year),
        "month2": str(end.month),
        "day2": str(end.day),
    }

    url = f"{mesonet_base_url}/cgi-bin/request/asos.py"

    transport = httpx.AsyncHTTPTransport(retries=3)
    async with httpx.AsyncClient(
        transport=transport,
        timeout=httpx.Timeout(120.0),
    ) as client:
        try:
            resp = await client.get(url, params=params)
            resp.raise_for_status()
        except httpx.HTTPError as exc:
            logger.error("asos_download_failed", station=station, error=str(exc))
            return 0

    return await _parse_and_insert(pool, station, resp.text)


async def _parse_and_insert(pool: asyncpg.Pool, station: str, csv_text: str) -> int:
    """Parse Mesonet CSV and bulk-insert into observations table."""
    reader = csv.DictReader(io.StringIO(csv_text))
    batch: list[tuple] = []
    total = 0

    for row in reader:
        try:
            observed_at = datetime.strptime(
                row.get("valid", "").strip(), "%Y-%m-%d %H:%M"
            ).replace(tzinfo=timezone.utc)
        except (ValueError, KeyError):
            continue

        temp_f = _safe_float(row.get("tmpf"))
        wind_kts = _safe_float(row.get("sknt"))
        gust_kts = _safe_float(row.get("gust"))
        precip = _safe_float(row.get("p01i"))

        # Skip rows with no temperature (primary data point)
        if temp_f is None:
            continue

        batch.append((
            "asos",
            station,
            observed_at,
            temp_f,
            wind_kts,
            gust_kts,
            precip,
        ))

        if len(batch) >= _INSERT_BATCH:
            total += await _insert_batch(pool, batch)
            batch = []

    if batch:
        total += await _insert_batch(pool, batch)

    return total


async def _insert_batch(pool: asyncpg.Pool, rows: list[tuple]) -> int:
    """Insert a batch of observation rows, skipping conflicts."""
    async with pool.acquire() as conn:
        # Use ON CONFLICT DO NOTHING to handle re-runs safely
        await conn.executemany(
            """
            INSERT INTO observations (
                source, station, observed_at, temperature_f,
                wind_speed_kts, wind_gust_kts, precip_inch
            ) VALUES ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT DO NOTHING
            """,
            rows,
        )
    return len(rows)


def _safe_float(value: str | None) -> float | None:
    """Parse float from Mesonet CSV, handling 'M' (missing) and 'T' (trace)."""
    if value is None:
        return None
    value = value.strip()
    if value in ("M", "", "T"):
        return 0.0 if value == "T" else None
    try:
        v = float(value)
        # Mesonet uses -99 and -9999 as sentinel values
        if v <= -99:
            return None
        return v
    except ValueError:
        return None


async def import_kalshi_history(
    settings: Settings,
    months: int = 6,
) -> int:
    """Import settled Kalshi contracts using existing history client."""
    logger.info("kalshi_import_start", months=months)
    count = await pull_settlement_history(
        settings=settings,
        months=months,
        categories=["weather", "crypto"],
    )
    logger.info("kalshi_import_complete", contracts=count)
    return count


async def main() -> None:
    parser = argparse.ArgumentParser(description="Import historical data")
    parser.add_argument(
        "--months", type=int, default=6, help="Months of history to import (default: 6)"
    )
    parser.add_argument(
        "--stations",
        type=str,
        default=None,
        help="Comma-separated station codes (default: from config)",
    )
    parser.add_argument(
        "--asos-only", action="store_true", help="Only import ASOS observations"
    )
    parser.add_argument(
        "--kalshi-only", action="store_true", help="Only import Kalshi settlements"
    )
    args = parser.parse_args()

    settings = get_settings()
    stations = (
        args.stations.split(",") if args.stations else settings.asos_stations
    )

    pool = await asyncpg.create_pool(settings.database_url, min_size=2, max_size=5)

    try:
        if not args.kalshi_only:
            await import_asos_history(
                pool,
                stations=stations,
                months=args.months,
                mesonet_base_url=settings.mesonet_base_url,
            )

        if not args.asos_only:
            await import_kalshi_history(settings, months=args.months)

        # Report observation coverage
        async with pool.acquire() as conn:
            obs_count = await conn.fetchval(
                "SELECT COUNT(*) FROM observations WHERE source = 'asos'"
            )
            contract_count = await conn.fetchval(
                "SELECT COUNT(*) FROM contracts WHERE settled_yes IS NOT NULL"
            )
            station_count = await conn.fetchval(
                "SELECT COUNT(DISTINCT station) FROM observations WHERE source = 'asos'"
            )

        logger.info(
            "import_summary",
            total_observations=obs_count,
            total_settled_contracts=contract_count,
            stations_with_data=station_count,
        )
    finally:
        await pool.close()


if __name__ == "__main__":
    asyncio.run(main())
