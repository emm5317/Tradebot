"""BE-3.4: Kalshi Historical Data Pull — settlement & price history ingestion."""

from __future__ import annotations

import asyncio
from datetime import UTC, datetime
from typing import Any

import asyncpg
import httpx
import structlog
from pydantic import BaseModel

from config import Settings, get_settings
from rules.ticker_parser import parse_ticker
from rules.timezone import compute_day_boundaries

logger = structlog.get_logger()

# Kalshi rate limit: 100 req/min → ~1.7 req/s. We pace at 1.5 req/s for margin.
_REQUEST_INTERVAL = 0.67  # seconds between requests
_PAGE_SIZE = 200  # Kalshi max per page


class MarketSnapshot(BaseModel):
    ticker: str
    title: str
    category: str
    city: str | None = None
    station: str | None = None
    threshold: float | None = None
    settlement_time: datetime
    status: str
    settled_yes: bool | None = None
    close_price: float | None = None


async def pull_settlement_history(
    settings: Settings | None = None,
    months: int = 12,
    categories: list[str] | None = None,
) -> int:
    """Pull all settled weather + crypto contracts from Kalshi.

    Upserts by ticker — safe to re-run.
    Returns the total number of contracts ingested.
    """
    if settings is None:
        settings = get_settings()
    if categories is None:
        categories = ["weather", "crypto"]

    pool = await asyncpg.create_pool(settings.database_url, min_size=1, max_size=3)
    total = 0

    try:
        transport = httpx.AsyncHTTPTransport(retries=2)
        async with httpx.AsyncClient(
            transport=transport,
            timeout=httpx.Timeout(30.0),
            base_url=settings.kalshi_base_url,
        ) as client:
            for category in categories:
                count = await _pull_category(client, pool, category, months)
                total += count
                logger.info(
                    "history_category_done",
                    category=category,
                    count=count,
                )
    finally:
        await pool.close()

    logger.info("history_pull_complete", total=total)
    return total


async def _pull_category(
    client: httpx.AsyncClient,
    pool: asyncpg.Pool,
    category: str,
    months: int,
) -> int:
    """Pull all settled markets for a category with pagination."""
    cursor: str | None = None
    count = 0

    while True:
        params: dict[str, Any] = {
            "status": "settled",
            "limit": _PAGE_SIZE,
        }
        if cursor:
            params["cursor"] = cursor

        # Pace requests to stay under rate limit
        await asyncio.sleep(_REQUEST_INTERVAL)

        try:
            resp = await client.get(
                "/trade-api/v2/markets",
                params=params,
            )
        except httpx.HTTPError as exc:
            logger.warning("history_fetch_error", category=category, error=str(exc))
            break

        if resp.status_code == 429:
            retry_after = float(resp.headers.get("Retry-After", "5"))
            logger.warning("history_rate_limited", retry_after=retry_after)
            await asyncio.sleep(retry_after)
            continue

        if resp.status_code != 200:
            logger.warning(
                "history_fetch_failed",
                category=category,
                status=resp.status_code,
            )
            break

        data = resp.json()
        markets = data.get("markets", [])

        if not markets:
            break

        # Filter to relevant category (Kalshi may not support server-side filtering)
        relevant = [m for m in markets if _matches_category(m, category)]

        if relevant:
            await _upsert_contracts(pool, relevant)
            count += len(relevant)

        cursor = data.get("cursor")
        if not cursor or len(markets) < _PAGE_SIZE:
            break

    return count


def _matches_category(market: dict, category: str) -> bool:
    """Check if a market belongs to the target category.

    Kalshi API often returns category=None, so we also match on
    ticker prefix and title keywords.
    """
    cat = (market.get("category") or "").lower()
    title = (market.get("title") or "").lower()
    ticker = (market.get("ticker") or "").upper()

    if category == "weather":
        return (
            cat == "weather"
            or any(kw in title for kw in ["temperature", "wind", "rain", "snow", "weather"])
            or ticker.startswith(("KXTEMP", "KXWIND", "KXRAIN", "KXSNOW"))
        )
    elif category == "crypto":
        return (
            cat == "crypto"
            or any(kw in title for kw in ["bitcoin", "btc", "crypto"])
            or ticker.startswith(("KXBTC", "KXETH", "KXCRYPTO"))
        )
    return cat == category


async def _upsert_contracts(pool: asyncpg.Pool, markets: list[dict]) -> None:
    """Upsert market data into contracts table."""
    async with pool.acquire() as conn:
        for market in markets:
            ticker = market.get("ticker", "")
            if not ticker:
                continue

            settlement_time_str = market.get("close_time") or market.get("expected_expiration_time")
            if not settlement_time_str:
                continue

            try:
                settlement_time = datetime.fromisoformat(settlement_time_str.replace("Z", "+00:00"))
            except (ValueError, AttributeError):
                settlement_time = datetime.now(UTC)

            settled_yes: bool | None = None
            result = market.get("result")
            if result == "yes":
                settled_yes = True
            elif result == "no":
                settled_yes = False

            close_price = market.get("last_price")
            if close_price is not None:
                close_price = float(close_price) / 100.0

            # Extract weather-specific fields from subtitle/title
            city = _extract_city(market)
            station = _extract_station(market)
            threshold = _extract_threshold(market)

            await conn.execute(
                """
                INSERT INTO contracts (
                    ticker, title, category, city, station, threshold,
                    settlement_time, status, settled_yes, close_price
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                ON CONFLICT (ticker) DO UPDATE SET
                    status = EXCLUDED.status,
                    settled_yes = EXCLUDED.settled_yes,
                    close_price = EXCLUDED.close_price,
                    updated_at = now()
                """,
                ticker,
                market.get("title", ""),
                _categorize(market),
                city,
                station,
                threshold,
                settlement_time,
                market.get("status", "settled"),
                settled_yes,
                close_price,
            )

            # Also upsert into contract_rules via ticker parser
            await _upsert_rules(
                conn,
                ticker=ticker,
                title=market.get("title", ""),
                category=_categorize(market),
                settlement_time=settlement_time,
            )


async def pull_active_contracts(
    settings: Settings | None = None,
    categories: list[str] | None = None,
) -> int:
    """Pull all active (open/trading) weather + crypto contracts from Kalshi.

    Seeds the contracts table so the evaluator and contract discovery
    can find contracts to trade. Safe to re-run (upserts by ticker).
    """
    if settings is None:
        settings = get_settings()
    if categories is None:
        categories = ["weather", "crypto"]

    pool = await asyncpg.create_pool(settings.database_url, min_size=1, max_size=3)
    total = 0

    # Known series tickers for targeted pulls (Kalshi category field is often None)
    _CRYPTO_SERIES = ["KXBTC", "KXBTCD", "KXBTC15M", "KXETH"]
    _WEATHER_SERIES = ["KXTEMP", "KXTEMPHI", "KXTEMPLO"]

    try:
        transport = httpx.AsyncHTTPTransport(retries=2)
        async with httpx.AsyncClient(
            transport=transport,
            timeout=httpx.Timeout(30.0),
            base_url=settings.kalshi_base_url,
        ) as client:
            # Phase 1: Targeted series-based pulls (fast, reliable)
            series_to_pull = []
            if "crypto" in categories:
                series_to_pull.extend(_CRYPTO_SERIES)
            if "weather" in categories:
                series_to_pull.extend(_WEATHER_SERIES)

            for series in series_to_pull:
                count = await _pull_series(client, pool, series)
                total += count

            # Phase 2: Generic paginated pull (catches anything series pull missed)
            for status in ("open", "active"):
                cursor: str | None = None
                while True:
                    params: dict[str, Any] = {
                        "status": status,
                        "limit": _PAGE_SIZE,
                    }
                    if cursor:
                        params["cursor"] = cursor

                    await asyncio.sleep(_REQUEST_INTERVAL)

                    try:
                        resp = await client.get(
                            "/trade-api/v2/markets",
                            params=params,
                        )
                    except httpx.HTTPError as exc:
                        logger.warning("active_fetch_error", status=status, error=str(exc))
                        break

                    if resp.status_code == 429:
                        retry_after = float(resp.headers.get("Retry-After", "5"))
                        logger.warning("active_rate_limited", retry_after=retry_after)
                        await asyncio.sleep(retry_after)
                        continue

                    if resp.status_code != 200:
                        logger.warning(
                            "active_fetch_failed",
                            status_code=resp.status_code,
                            status_filter=status,
                        )
                        break

                    data = resp.json()
                    markets = data.get("markets", [])

                    if not markets:
                        break

                    relevant = [m for m in markets if any(_matches_category(m, cat) for cat in categories)]

                    if relevant:
                        await _upsert_contracts(pool, relevant)
                        total += len(relevant)
                        logger.info(
                            "active_contracts_ingested",
                            status=status,
                            batch=len(relevant),
                            total=total,
                        )

                    cursor = data.get("cursor")
                    if not cursor or len(markets) < _PAGE_SIZE:
                        break

    finally:
        await pool.close()

    logger.info("active_pull_complete", total=total)
    return total


async def _pull_series(
    client: httpx.AsyncClient,
    pool: asyncpg.Pool,
    series_ticker: str,
) -> int:
    """Pull all non-settled markets for a specific series ticker."""
    cursor: str | None = None
    count = 0

    while True:
        params: dict[str, Any] = {
            "series_ticker": series_ticker,
            "limit": _PAGE_SIZE,
        }
        if cursor:
            params["cursor"] = cursor

        await asyncio.sleep(_REQUEST_INTERVAL)

        try:
            resp = await client.get("/trade-api/v2/markets", params=params)
        except httpx.HTTPError as exc:
            logger.warning("series_fetch_error", series=series_ticker, error=str(exc))
            break

        if resp.status_code == 429:
            retry_after = float(resp.headers.get("Retry-After", "5"))
            await asyncio.sleep(retry_after)
            continue

        if resp.status_code != 200:
            logger.warning("series_fetch_failed", series=series_ticker, status=resp.status_code)
            break

        data = resp.json()
        markets = data.get("markets", [])

        if not markets:
            break

        await _upsert_contracts(pool, markets)
        count += len(markets)

        cursor = data.get("cursor")
        if not cursor or len(markets) < _PAGE_SIZE:
            break

    if count > 0:
        logger.info("series_pull_done", series=series_ticker, count=count)
    return count


async def pull_historical_prices(
    ticker: str,
    settings: Settings | None = None,
) -> list[MarketSnapshot]:
    """Pull historical price snapshots for a specific contract.

    Returns a list of price snapshots ordered by time.
    """
    if settings is None:
        settings = get_settings()

    transport = httpx.AsyncHTTPTransport(retries=2)
    async with httpx.AsyncClient(
        transport=transport,
        timeout=httpx.Timeout(30.0),
        base_url=settings.kalshi_base_url,
    ) as client:
        snapshots: list[MarketSnapshot] = []
        cursor: str | None = None

        while True:
            await asyncio.sleep(_REQUEST_INTERVAL)

            params: dict[str, Any] = {
                "ticker": ticker,
                "limit": _PAGE_SIZE,
            }
            if cursor:
                params["cursor"] = cursor

            try:
                resp = await client.get(
                    "/trade-api/v2/markets/trades",
                    params=params,
                )
            except httpx.HTTPError as exc:
                logger.warning("price_history_error", ticker=ticker, error=str(exc))
                break

            if resp.status_code == 429:
                retry_after = float(resp.headers.get("Retry-After", "5"))
                await asyncio.sleep(retry_after)
                continue

            if resp.status_code != 200:
                break

            data = resp.json()
            trades = data.get("trades", [])

            if not trades:
                break

            for trade in trades:
                created = trade.get("created_time", "")
                try:
                    ts = datetime.fromisoformat(created.replace("Z", "+00:00"))
                except (ValueError, AttributeError):
                    ts = datetime.now(UTC)

                snapshots.append(
                    MarketSnapshot(
                        ticker=ticker,
                        title="",
                        category="",
                        settlement_time=ts,
                        status="trade",
                        close_price=float(trade.get("yes_price", 0)) / 100.0,
                    )
                )

            cursor = data.get("cursor")
            if not cursor or len(trades) < _PAGE_SIZE:
                break

        return snapshots


def _categorize(market: dict) -> str:
    cat = market.get("category", "").lower()
    if cat in ("weather", "crypto"):
        return cat
    title = market.get("title", "").lower()
    if any(kw in title for kw in ["bitcoin", "btc", "crypto"]):
        return "crypto"
    return "weather"


def _extract_city(market: dict) -> str | None:
    """Try to extract city from market title/subtitle."""
    subtitle = market.get("subtitle", "") or ""
    city_map = {
        "Chicago": "Chicago",
        "New York": "New York",
        "Denver": "Denver",
        "Los Angeles": "Los Angeles",
        "Houston": "Houston",
    }
    for city in city_map:
        if city.lower() in subtitle.lower() or city.lower() in market.get("title", "").lower():
            return city_map[city]
    return None


def _extract_station(market: dict) -> str | None:
    """Try to extract ASOS station code from market metadata."""
    # Kalshi doesn't directly expose station codes, but we can infer from city
    city = _extract_city(market)
    city_to_station = {
        "Chicago": "KORD",
        "New York": "KJFK",
        "Denver": "KDEN",
        "Los Angeles": "KLAX",
        "Houston": "KIAH",
    }
    return city_to_station.get(city) if city else None


def _extract_threshold(market: dict) -> float | None:
    """Try to extract threshold (e.g., temperature) from title."""
    import re

    title = market.get("title", "")
    # Look for patterns like "above 32°F", "below 90°F", "over 10 mph"
    match = re.search(r"(?:above|below|over|under|at least|at most)\s+([\d.]+)", title)
    if match:
        try:
            return float(match.group(1))
        except ValueError:
            pass
    return None


async def _upsert_rules(
    conn: asyncpg.Connection,
    ticker: str,
    title: str,
    category: str,
    settlement_time: datetime,
) -> None:
    """Parse ticker and upsert into contract_rules table."""
    parsed = parse_ticker(ticker, title=title, category=category)
    if parsed is None or parsed.contract_type is None:
        return

    # Compute day boundaries for weather contracts
    day_start = None
    day_end = None
    if parsed.settlement_tz and parsed.contract_type in ("weather_max", "weather_min"):
        try:
            day_start, day_end = compute_day_boundaries(
                parsed.settlement_tz,
                settlement_time.date(),
            )
        except Exception:
            pass

    try:
        await conn.execute(
            """
            INSERT INTO contract_rules (
                series_ticker, market_ticker, contract_type,
                settlement_source, settlement_station, settlement_tz,
                strike, expiry_time,
                settlement_window_start, settlement_window_end,
                day_boundary_start, day_boundary_end,
                underlying, constituent_exchanges
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
            ON CONFLICT (market_ticker) DO UPDATE SET
                contract_type = EXCLUDED.contract_type,
                settlement_source = EXCLUDED.settlement_source,
                settlement_station = EXCLUDED.settlement_station,
                settlement_tz = EXCLUDED.settlement_tz,
                strike = EXCLUDED.strike,
                day_boundary_start = EXCLUDED.day_boundary_start,
                day_boundary_end = EXCLUDED.day_boundary_end,
                updated_at = now()
            """,
            parsed.series_ticker,
            parsed.market_ticker,
            parsed.contract_type,
            parsed.settlement_source or "unknown",
            parsed.settlement_station,
            parsed.settlement_tz,
            parsed.strike or 0.0,
            settlement_time,
            None,  # settlement_window_start (populated for crypto at runtime)
            None,  # settlement_window_end
            day_start,
            day_end,
            parsed.underlying,
            parsed.constituent_exchanges or None,
        )
    except Exception:
        logger.debug("rules_upsert_skipped", ticker=ticker)
