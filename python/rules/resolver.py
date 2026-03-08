"""Contract rules resolver — loads and caches settlement rules per ticker."""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime
from typing import Literal

import asyncpg
import structlog

logger = structlog.get_logger()


@dataclass(frozen=True)
class ContractRules:
    """Full settlement specification for a single contract."""

    market_ticker: str
    series_ticker: str
    contract_type: Literal["crypto_binary", "weather_max", "weather_min"]
    settlement_source: str  # 'cfb_rti' or 'nws_cli_dsm'
    settlement_station: str | None  # ASOS code for weather
    settlement_tz: str | None  # IANA timezone
    strike: float
    expiry_time: datetime
    settlement_window_start: datetime | None = None  # CFB RTI 60s window
    settlement_window_end: datetime | None = None
    day_boundary_start: datetime | None = None  # weather LST day start
    day_boundary_end: datetime | None = None
    underlying: str | None = None  # 'BTCUSD' or station code
    constituent_exchanges: list[str] = field(default_factory=list)

    @property
    def is_crypto(self) -> bool:
        return self.contract_type == "crypto_binary"

    @property
    def is_weather(self) -> bool:
        return self.contract_type in ("weather_max", "weather_min")

    @property
    def signal_type(self) -> str:
        return "crypto" if self.is_crypto else "weather"


class ContractRulesResolver:
    """Loads contract rules from DB and caches them in memory.

    Thread-safe for concurrent reads. Call refresh() periodically
    to pick up new contracts.
    """

    def __init__(self) -> None:
        self._cache: dict[str, ContractRules] = {}

    async def load(self, pool: asyncpg.Pool) -> int:
        """Load all active contract rules from DB. Returns count loaded."""
        async with pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT market_ticker, series_ticker, contract_type,
                       settlement_source, settlement_station, settlement_tz,
                       strike, expiry_time,
                       settlement_window_start, settlement_window_end,
                       day_boundary_start, day_boundary_end,
                       underlying, constituent_exchanges
                FROM contract_rules
                WHERE expiry_time > now() - interval '1 day'
                """
            )

        new_cache: dict[str, ContractRules] = {}
        for row in rows:
            exchanges = row["constituent_exchanges"] or []
            rules = ContractRules(
                market_ticker=row["market_ticker"],
                series_ticker=row["series_ticker"],
                contract_type=row["contract_type"],
                settlement_source=row["settlement_source"],
                settlement_station=row["settlement_station"],
                settlement_tz=row["settlement_tz"],
                strike=row["strike"],
                expiry_time=row["expiry_time"],
                settlement_window_start=row["settlement_window_start"],
                settlement_window_end=row["settlement_window_end"],
                day_boundary_start=row["day_boundary_start"],
                day_boundary_end=row["day_boundary_end"],
                underlying=row["underlying"],
                constituent_exchanges=list(exchanges),
            )
            new_cache[row["market_ticker"]] = rules

        self._cache = new_cache
        logger.info("rules_loaded", count=len(new_cache))
        return len(new_cache)

    def get(self, market_ticker: str) -> ContractRules | None:
        """Look up rules for a specific contract ticker."""
        return self._cache.get(market_ticker)

    def all_active(self) -> list[ContractRules]:
        """Return all cached rules."""
        return list(self._cache.values())

    @property
    def count(self) -> int:
        return len(self._cache)
