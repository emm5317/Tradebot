"""Ticker format parser — extracts contract metadata from structured tickers.

Kalshi market tickers encode series, date, and strike information
in a structured format. This module parses those tickers deterministically
instead of relying on English title regex.

The SERIES_CONFIG mapping is populated by the discovery script
(rules/discover.py) and should be updated when Kalshi adds new series.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any

import structlog

from rules.timezone import STATION_TIMEZONES, compute_day_boundaries

logger = structlog.get_logger()


# City abbreviation → (ASOS station, IANA timezone)
STATION_MAP: dict[str, tuple[str, str]] = {
    "CHI": ("KORD", "America/Chicago"),
    "NYC": ("KJFK", "America/New_York"),
    "DEN": ("KDEN", "America/Denver"),
    "LAX": ("KLAX", "America/Los_Angeles"),
    "HOU": ("KIAH", "America/Chicago"),
    # Alternative abbreviations
    "ORD": ("KORD", "America/Chicago"),
    "JFK": ("KJFK", "America/New_York"),
    "IAH": ("KIAH", "America/Chicago"),
}

# CFB RTI constituent exchanges for BTC settlement
CFB_RTI_EXCHANGES = [
    "bitstamp",
    "coinbase",
    "gemini",
    "itbit",
    "kraken",
    "lmax",
    "bullish",
    "cryptocom",
]


# Series configuration mapping.
# Keys are series prefix patterns (regex), values are base config.
# This is the output of the discovery script.
SERIES_CONFIG: dict[str, dict[str, Any]] = {
    # Crypto daily BTC binary
    r"^KXBTC": {
        "contract_type": "crypto_binary",
        "settlement_source": "cfb_rti",
        "underlying": "BTCUSD",
        "constituent_exchanges": CFB_RTI_EXCHANGES,
    },
    # Weather temperature high
    r"^KXTEMP": {
        "settlement_source": "nws_cli_dsm",
        # contract_type determined by sub-parsing (HIGH vs LOW)
    },
    r"^KXTEMPHI": {
        "contract_type": "weather_max",
        "settlement_source": "nws_cli_dsm",
    },
    r"^KXTEMPLO": {
        "contract_type": "weather_min",
        "settlement_source": "nws_cli_dsm",
    },
    # Weather high temp (alternative pattern)
    r"^HIGHTEMP": {
        "contract_type": "weather_max",
        "settlement_source": "nws_cli_dsm",
    },
    r"^LOWTEMP": {
        "contract_type": "weather_min",
        "settlement_source": "nws_cli_dsm",
    },
    # INX prefix for temperature
    r"^INX": {
        "settlement_source": "nws_cli_dsm",
    },
}


@dataclass(frozen=True)
class ParsedTicker:
    """Result of parsing a Kalshi market ticker."""

    market_ticker: str
    series_ticker: str
    contract_type: str | None  # 'crypto_binary', 'weather_max', 'weather_min'
    settlement_source: str | None
    settlement_station: str | None
    settlement_tz: str | None
    strike: float | None
    underlying: str | None
    constituent_exchanges: list[str]
    city_abbrev: str | None = None


def parse_ticker(
    ticker: str,
    title: str = "",
    category: str = "",
) -> ParsedTicker | None:
    """Parse a Kalshi market ticker into structured metadata.

    Tries structured ticker parsing first, falls back to title-based
    extraction for unrecognized formats.

    Args:
        ticker: The market ticker string, e.g. 'KXBTCD-26MAR08-T98500'
        title: The market title (used as fallback for contract_type detection)
        category: The market category from Kalshi API

    Returns:
        ParsedTicker if parseable, None if completely unrecognized.
    """
    if not ticker:
        return None

    # Find matching series config
    series_config = _match_series(ticker)
    series_ticker = _extract_series_ticker(ticker)

    # Determine contract type
    contract_type = _determine_contract_type(
        ticker, title, category, series_config
    )
    if contract_type is None:
        return None

    # Extract settlement details based on type
    settlement_source = (series_config or {}).get("settlement_source")
    underlying = (series_config or {}).get("underlying")
    constituent_exchanges = (series_config or {}).get("constituent_exchanges", [])

    station = None
    station_tz = None
    city_abbrev = None
    strike = None

    if contract_type == "crypto_binary":
        settlement_source = settlement_source or "cfb_rti"
        underlying = underlying or "BTCUSD"
        constituent_exchanges = constituent_exchanges or CFB_RTI_EXCHANGES
        strike = _extract_crypto_strike(ticker)
    else:
        settlement_source = settlement_source or "nws_cli_dsm"
        city_abbrev = _extract_city_abbrev(ticker, title)
        if city_abbrev and city_abbrev in STATION_MAP:
            station, station_tz = STATION_MAP[city_abbrev]
        underlying = station
        strike = _extract_weather_strike(ticker, title)

    return ParsedTicker(
        market_ticker=ticker,
        series_ticker=series_ticker,
        contract_type=contract_type,
        settlement_source=settlement_source,
        settlement_station=station,
        settlement_tz=station_tz,
        strike=strike,
        underlying=underlying,
        constituent_exchanges=constituent_exchanges,
        city_abbrev=city_abbrev,
    )


def _match_series(ticker: str) -> dict[str, Any] | None:
    """Find the matching series config for a ticker."""
    for pattern, config in SERIES_CONFIG.items():
        if re.match(pattern, ticker, re.IGNORECASE):
            return config
    return None


def _extract_series_ticker(ticker: str) -> str:
    """Extract the series portion of a ticker (everything before the strike)."""
    # Try splitting on common separators
    parts = ticker.split("-")
    if len(parts) >= 2:
        return parts[0]
    return ticker


def _determine_contract_type(
    ticker: str,
    title: str,
    category: str,
    series_config: dict[str, Any] | None,
) -> str | None:
    """Determine the contract type from ticker, title, and config."""
    # Check series config first
    if series_config and "contract_type" in series_config:
        return series_config["contract_type"]

    # Infer from ticker patterns
    ticker_upper = ticker.upper()
    if any(kw in ticker_upper for kw in ("BTC", "CRYPTO", "BITCOIN")):
        return "crypto_binary"

    # Infer from category
    cat_lower = (category or "").lower()
    if cat_lower in ("crypto", "bitcoin"):
        return "crypto_binary"

    # Infer max vs min from ticker or title
    title_lower = title.lower()
    if any(kw in ticker_upper for kw in ("HIGH", "TEMPHI", "MAX")):
        return "weather_max"
    if any(kw in ticker_upper for kw in ("LOW", "TEMPLO", "MIN")):
        return "weather_min"
    if "high" in title_lower or "above" in title_lower or "warmer" in title_lower:
        return "weather_max"
    if "low" in title_lower or "below" in title_lower or "colder" in title_lower:
        return "weather_min"

    # Weather category but can't determine max/min
    if cat_lower == "weather" or any(
        kw in title_lower for kw in ("temperature", "temp", "weather")
    ):
        return "weather_max"  # default to max if ambiguous

    return None


def _extract_crypto_strike(ticker: str) -> float | None:
    """Extract BTC strike price from ticker.

    Examples: KXBTCD-26MAR08-T98500 → 98500.0
              KXBTC-26MAR08-B95000 → 95000.0
    """
    # Look for T/B prefix followed by digits
    match = re.search(r"[TB](\d+(?:\.\d+)?)\s*$", ticker)
    if match:
        return float(match.group(1))

    # Try last segment after dash
    parts = ticker.split("-")
    if parts:
        last = parts[-1].lstrip("TBtb")
        try:
            return float(last)
        except ValueError:
            pass
    return None


def _extract_weather_strike(ticker: str, title: str) -> float | None:
    """Extract temperature threshold from ticker or title.

    Ticker examples: KXTEMP-26MAR08-CHI-T45 → 45.0
    Title fallback: "above 32°F" → 32.0
    """
    # Try ticker-embedded threshold (T/B prefix followed by digits)
    match = re.search(r"[TB](\d+(?:\.\d+)?)\s*$", ticker)
    if match:
        return float(match.group(1))

    # Try last numeric segment in ticker
    parts = ticker.split("-")
    for part in reversed(parts):
        cleaned = part.lstrip("TBtb")
        try:
            val = float(cleaned)
            if 0 <= val <= 150:  # reasonable temperature range in F
                return val
        except ValueError:
            continue

    # Fall back to title regex
    title_match = re.search(
        r"(?:above|below|over|under|at least|at most)\s+([\d.]+)",
        title,
    )
    if title_match:
        try:
            return float(title_match.group(1))
        except ValueError:
            pass

    return None


def _extract_city_abbrev(ticker: str, title: str) -> str | None:
    """Extract city abbreviation from ticker or title."""
    ticker_upper = ticker.upper()

    # Check for known abbreviations in ticker segments
    parts = ticker_upper.split("-")
    for part in parts:
        if part in STATION_MAP:
            return part

    # Check for city names in title
    city_name_to_abbrev = {
        "chicago": "CHI",
        "new york": "NYC",
        "denver": "DEN",
        "los angeles": "LAX",
        "houston": "HOU",
    }
    title_lower = title.lower()
    for city_name, abbrev in city_name_to_abbrev.items():
        if city_name in title_lower:
            return abbrev

    return None
