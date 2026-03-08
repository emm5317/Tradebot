"""Ticker format discovery — catalogs all series ticker formats from Kalshi data.

Run this one-time script to discover the ticker encoding patterns used
by Kalshi for different market series. The output informs the SERIES_CONFIG
mapping in ticker_parser.py.

Usage:
    python -m rules.discover [--from-db] [--from-api]
"""

from __future__ import annotations

import asyncio
import json
import re
import sys
from collections import defaultdict
from datetime import datetime, timezone

import asyncpg
import structlog

from config import Settings, get_settings

logger = structlog.get_logger()


async def discover_from_db(settings: Settings) -> dict[str, list[dict]]:
    """Query all tickers from contracts table and group by series prefix."""
    pool = await asyncpg.create_pool(settings.database_url, min_size=1, max_size=3)

    try:
        async with pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT ticker, title, category, city, station, threshold,
                       settlement_time, status, settled_yes
                FROM contracts
                ORDER BY ticker
                """
            )
    finally:
        await pool.close()

    return _analyze_tickers(
        [
            {
                "ticker": r["ticker"],
                "title": r["title"],
                "category": r["category"],
                "city": r["city"],
                "station": r["station"],
                "threshold": r["threshold"],
                "settlement_time": r["settlement_time"].isoformat()
                if r["settlement_time"]
                else None,
                "status": r["status"],
                "settled_yes": r["settled_yes"],
            }
            for r in rows
        ]
    )


def _analyze_tickers(markets: list[dict]) -> dict[str, list[dict]]:
    """Group tickers by series prefix and analyze encoding patterns."""
    groups: dict[str, list[dict]] = defaultdict(list)

    for market in markets:
        ticker = market["ticker"]
        prefix = _extract_prefix(ticker)
        groups[prefix].append(market)

    return dict(groups)


def _extract_prefix(ticker: str) -> str:
    """Extract the series prefix from a ticker.

    Tries to find the non-date, non-strike portion of the ticker.
    Examples:
        KXBTCD-26MAR08-T98500 → KXBTCD
        KXTEMP-26MAR08-CHI-T45 → KXTEMP
        HIGHTEMP-CHI-26MAR08-45 → HIGHTEMP
    """
    parts = ticker.split("-")
    if not parts:
        return ticker

    # First part is usually the series prefix
    prefix = parts[0]

    # Strip trailing date-like segments from prefix
    prefix = re.sub(r"\d{2}[A-Z]{3}\d{2}$", "", prefix)

    return prefix or parts[0]


def print_discovery_report(groups: dict[str, list[dict]]) -> None:
    """Print a human-readable report of discovered ticker formats."""
    print(f"\n{'=' * 70}")
    print(f"TICKER FORMAT DISCOVERY REPORT")
    print(f"{'=' * 70}")
    print(f"Total tickers analyzed: {sum(len(v) for v in groups.values())}")
    print(f"Unique series prefixes: {len(groups)}")

    for prefix in sorted(groups.keys()):
        tickers = groups[prefix]
        print(f"\n{'─' * 70}")
        print(f"Series: {prefix}")
        print(f"  Count: {len(tickers)}")

        # Show sample tickers
        samples = tickers[:5]
        print(f"  Samples:")
        for s in samples:
            print(f"    {s['ticker']}")
            if s.get("title"):
                print(f"      Title: {s['title'][:80]}")
            if s.get("category"):
                print(f"      Category: {s['category']}")
            if s.get("threshold") is not None:
                print(f"      Threshold: {s['threshold']}")
            if s.get("city"):
                print(f"      City: {s['city']}")

        # Analyze encoding pattern
        _analyze_pattern(prefix, tickers)


def _analyze_pattern(prefix: str, tickers: list[dict]) -> None:
    """Analyze the encoding pattern for a series."""
    # Collect all segments beyond the prefix
    all_segments: list[list[str]] = []
    for t in tickers:
        parts = t["ticker"].split("-")
        if len(parts) > 1:
            all_segments.append(parts[1:])

    if not all_segments:
        return

    max_segments = max(len(s) for s in all_segments)

    print(f"  Segment analysis ({max_segments} segments after prefix):")
    for i in range(max_segments):
        seg_values = [s[i] for s in all_segments if len(s) > i]
        unique = set(seg_values)

        # Classify segment type
        if all(re.match(r"\d{2}[A-Z]{3}\d{2}", v) for v in unique):
            print(f"    [{i+1}] DATE: {list(unique)[:5]}")
        elif all(re.match(r"[TB]\d+", v) for v in unique):
            strikes = sorted(
                float(v.lstrip("TBtb")) for v in unique if v.lstrip("TBtb").isdigit()
            )
            print(f"    [{i+1}] STRIKE: range {strikes[0]}-{strikes[-1]} ({len(unique)} unique)")
        elif all(v in ("CHI", "NYC", "DEN", "LAX", "HOU", "ORD", "JFK", "IAH") for v in unique):
            print(f"    [{i+1}] CITY: {sorted(unique)}")
        elif all(v.isdigit() for v in unique):
            vals = sorted(float(v) for v in unique)
            print(f"    [{i+1}] NUMERIC: range {vals[0]}-{vals[-1]} ({len(unique)} unique)")
        else:
            print(f"    [{i+1}] OTHER: {list(unique)[:10]}")

    # Generate suggested SERIES_CONFIG entry
    categories = {t.get("category", "").lower() for t in tickers}
    print(f"  Categories: {categories}")
    print(f"  Suggested config:")

    if "crypto" in categories or any("btc" in t.get("title", "").lower() for t in tickers):
        print(f'    "{prefix}": {{"contract_type": "crypto_binary", "settlement_source": "cfb_rti", "underlying": "BTCUSD"}}')
    elif "weather" in categories:
        print(f'    "{prefix}": {{"settlement_source": "nws_cli_dsm"}}')
    else:
        print(f'    "{prefix}": {{}}  # needs manual classification')


async def main() -> None:
    settings = get_settings()
    use_db = "--from-db" in sys.argv or len(sys.argv) == 1

    if use_db:
        print("Discovering ticker formats from database...")
        groups = await discover_from_db(settings)
        print_discovery_report(groups)

        # Also output machine-readable JSON
        output = {
            prefix: {
                "count": len(tickers),
                "sample_tickers": [t["ticker"] for t in tickers[:10]],
                "categories": list({t.get("category", "") for t in tickers}),
                "sample_titles": [t.get("title", "") for t in tickers[:5]],
            }
            for prefix, tickers in groups.items()
        }
        with open("ticker_discovery.json", "w") as f:
            json.dump(output, f, indent=2)
        print(f"\nMachine-readable output saved to ticker_discovery.json")
    else:
        print("Usage: python -m rules.discover [--from-db]")


if __name__ == "__main__":
    asyncio.run(main())
