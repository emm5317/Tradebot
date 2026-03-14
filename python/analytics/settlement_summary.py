"""Daily settlement summary aggregation.

Computes daily_settlement_summary from observations, METAR data, and
settled contracts. Designed to run nightly or on-demand for backfill.

Usage:
    python -m analytics.settlement_summary                    # yesterday
    python -m analytics.settlement_summary --date 2026-03-01  # specific date
    python -m analytics.settlement_summary --backfill 30      # last 30 days
"""

from __future__ import annotations

import argparse
import asyncio
from datetime import UTC, date, datetime, timedelta
from typing import TYPE_CHECKING

import structlog

if TYPE_CHECKING:
    import asyncpg

from config import get_db_ssl_mode, get_settings

logger = structlog.get_logger()


async def aggregate_settlement_summary(
    pool: asyncpg.Pool,
    target_date: date,
) -> dict[str, dict]:
    """Build daily settlement summary for all stations on a given date.

    Combines:
    - ASOS observations: running max/min from temperature_f
    - METAR 6hr groups: authoritative max/min
    - Contracts: count of settled contracts

    Returns dict of station -> summary metrics.
    """
    results = {}

    async with pool.acquire() as conn:
        # 1. ASOS: daily max/min per station from observations
        asos_rows = await conn.fetch(
            """
            SELECT
                station,
                MAX(temperature_f) AS max_f,
                MIN(temperature_f) AS min_f,
                COUNT(*) AS obs_count,
                MIN(observed_at) AS first_obs,
                MAX(observed_at) AS last_obs
            FROM observations
            WHERE source = 'asos'
              AND observed_at::date = $1
              AND temperature_f IS NOT NULL
            GROUP BY station
            """,
            target_date,
        )

        for row in asos_rows:
            station = row["station"]
            results[station] = {
                "asos_max_f": float(row["max_f"]) if row["max_f"] else None,
                "asos_min_f": float(row["min_f"]) if row["min_f"] else None,
                "obs_count": row["obs_count"],
                "first_obs_at": row["first_obs"],
                "last_obs_at": row["last_obs"],
            }

        # 2. METAR: 6hr max/min groups (authoritative)
        metar_rows = await conn.fetch(
            """
            SELECT
                station,
                MAX(max_temp_6hr_c) AS metar_max_c,
                MIN(min_temp_6hr_c) AS metar_min_c
            FROM metar_observations
            WHERE observed_at::date = $1
              AND (max_temp_6hr_c IS NOT NULL OR min_temp_6hr_c IS NOT NULL)
            GROUP BY station
            """,
            target_date,
        )

        for row in metar_rows:
            station = row["station"]
            if station not in results:
                results[station] = {
                    "asos_max_f": None,
                    "asos_min_f": None,
                    "obs_count": 0,
                    "first_obs_at": None,
                    "last_obs_at": None,
                }
            # Convert C to F
            if row["metar_max_c"] is not None:
                results[station]["metar_max_f"] = float(row["metar_max_c"]) * 9 / 5 + 32
            else:
                results[station]["metar_max_f"] = None

            if row["metar_min_c"] is not None:
                results[station]["metar_min_f"] = float(row["metar_min_c"]) * 9 / 5 + 32
            else:
                results[station]["metar_min_f"] = None

        # 3. Contracts settled this day per station
        contract_rows = await conn.fetch(
            """
            SELECT station, COUNT(*) AS settled
            FROM contracts
            WHERE settlement_time::date = $1
              AND settled_yes IS NOT NULL
              AND station IS NOT NULL
            GROUP BY station
            """,
            target_date,
        )

        contract_counts = {r["station"]: r["settled"] for r in contract_rows}

        # 4. Upsert into daily_settlement_summary
        for station, data in results.items():
            # Final max/min: prefer METAR authoritative, fall back to ASOS
            final_max_f = data.get("metar_max_f") or data.get("asos_max_f")
            final_min_f = data.get("metar_min_f") or data.get("asos_min_f")

            # If both METAR and ASOS exist, take the more extreme value
            asos_max = data.get("asos_max_f")
            metar_max = data.get("metar_max_f")
            if asos_max is not None and metar_max is not None:
                final_max_f = max(asos_max, metar_max)

            asos_min = data.get("asos_min_f")
            metar_min = data.get("metar_min_f")
            if asos_min is not None and metar_min is not None:
                final_min_f = min(asos_min, metar_min)

            await conn.execute(
                """
                INSERT INTO daily_settlement_summary (
                    station, obs_date, final_max_f, final_min_f,
                    metar_max_f, metar_min_f,
                    obs_count, first_obs_at, last_obs_at,
                    contracts_settled
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                ON CONFLICT (station, obs_date) DO UPDATE SET
                    final_max_f = EXCLUDED.final_max_f,
                    final_min_f = EXCLUDED.final_min_f,
                    metar_max_f = EXCLUDED.metar_max_f,
                    metar_min_f = EXCLUDED.metar_min_f,
                    obs_count = EXCLUDED.obs_count,
                    first_obs_at = EXCLUDED.first_obs_at,
                    last_obs_at = EXCLUDED.last_obs_at,
                    contracts_settled = EXCLUDED.contracts_settled
                """,
                station,
                target_date,
                final_max_f,
                final_min_f,
                data.get("metar_max_f"),
                data.get("metar_min_f"),
                data.get("obs_count", 0),
                data.get("first_obs_at"),
                data.get("last_obs_at"),
                contract_counts.get(station, 0),
            )

            logger.info(
                "settlement_summary_upserted",
                station=station,
                date=str(target_date),
                final_max_f=final_max_f,
                final_min_f=final_min_f,
                obs_count=data.get("obs_count", 0),
                contracts=contract_counts.get(station, 0),
            )

    return results


async def backfill(pool: asyncpg.Pool, days: int) -> None:
    """Backfill settlement summaries for the last N days."""
    today = datetime.now(UTC).date()

    for i in range(days, 0, -1):
        target = today - timedelta(days=i)
        try:
            await aggregate_settlement_summary(pool, target)
        except Exception:
            logger.exception("backfill_error", date=str(target))


async def main() -> None:
    import asyncpg as _asyncpg

    parser = argparse.ArgumentParser(description="Aggregate daily settlement summary")
    parser.add_argument("--date", help="Target date (YYYY-MM-DD), default=yesterday")
    parser.add_argument("--backfill", type=int, default=0, help="Backfill last N days")
    args = parser.parse_args()

    settings = get_settings()
    pool = await _asyncpg.create_pool(settings.database_url, min_size=1, max_size=3, ssl=get_db_ssl_mode(settings.database_url))

    if args.backfill > 0:
        await backfill(pool, args.backfill)
    else:
        if args.date:
            target = date.fromisoformat(args.date)
        else:
            target = datetime.now(UTC).date() - timedelta(days=1)

        results = await aggregate_settlement_summary(pool, target)
        print(f"Aggregated {len(results)} stations for {target}")

    await pool.close()


if __name__ == "__main__":
    asyncio.run(main())
