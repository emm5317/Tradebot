"""METAR observation fetcher from Aviation Weather Center.

Fetches decoded METAR data including 6-hourly max/min temperature groups
(1xxxx/2xxxx remarks) that feed into NWS Daily Climate Reports used for
Kalshi weather contract settlement.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Any

import httpx
import structlog

logger = structlog.get_logger()

_BASE_URL = "https://aviationweather.gov/api/data/metar"
_REQUEST_TIMEOUT = 15.0


@dataclass
class METARObservation:
    """Parsed METAR observation with settlement-relevant fields."""

    station: str
    observed_at: datetime
    temp_c: float | None = None
    dewpoint_c: float | None = None
    wind_speed_kts: float | None = None
    wind_gust_kts: float | None = None
    altimeter_inhg: float | None = None
    visibility_mi: float | None = None
    wx_string: str | None = None
    max_temp_6hr_c: float | None = None
    min_temp_6hr_c: float | None = None
    max_temp_24hr_c: float | None = None
    min_temp_24hr_c: float | None = None
    raw_metar: str = ""

    @property
    def temp_f(self) -> float | None:
        """Temperature in Fahrenheit (for comparison with CLI thresholds)."""
        if self.temp_c is None:
            return None
        return self.temp_c * 9.0 / 5.0 + 32.0

    @property
    def max_temp_6hr_f(self) -> float | None:
        if self.max_temp_6hr_c is None:
            return None
        return self.max_temp_6hr_c * 9.0 / 5.0 + 32.0

    @property
    def min_temp_6hr_f(self) -> float | None:
        if self.min_temp_6hr_c is None:
            return None
        return self.min_temp_6hr_c * 9.0 / 5.0 + 32.0


async def fetch_metar(
    stations: list[str],
    hours: float = 2.0,
) -> list[METARObservation]:
    """Fetch recent METAR observations from Aviation Weather Center.

    Args:
        stations: List of ICAO station codes (e.g., ["KORD", "KJFK"])
        hours: Number of hours of data to request

    Returns:
        List of parsed METAR observations, newest first.
    """
    station_str = ",".join(stations)
    params = {
        "ids": station_str,
        "hours": str(hours),
        "format": "json",
    }

    transport = httpx.AsyncHTTPTransport(retries=2)
    async with httpx.AsyncClient(
        transport=transport,
        timeout=httpx.Timeout(_REQUEST_TIMEOUT),
    ) as client:
        try:
            resp = await client.get(_BASE_URL, params=params)
        except httpx.HTTPError as exc:
            logger.warning("metar_fetch_error", stations=station_str, error=str(exc))
            return []

        if resp.status_code != 200:
            logger.warning(
                "metar_fetch_failed",
                stations=station_str,
                status=resp.status_code,
            )
            return []

        try:
            data = resp.json()
        except Exception:
            logger.warning("metar_parse_error", stations=station_str)
            return []

    if not isinstance(data, list):
        return []

    observations: list[METARObservation] = []
    for entry in data:
        obs = _parse_metar_json(entry)
        if obs is not None:
            observations.append(obs)

    return observations


def _parse_metar_json(entry: dict[str, Any]) -> METARObservation | None:
    """Parse a single METAR JSON entry from Aviation Weather API."""
    station = entry.get("icaoId") or entry.get("stationId")
    if not station:
        return None

    obs_time_str = entry.get("obsTime") or entry.get("reportTime")
    if not obs_time_str:
        return None

    try:
        if isinstance(obs_time_str, (int, float)):
            observed_at = datetime.fromtimestamp(obs_time_str, tz=timezone.utc)
        else:
            observed_at = datetime.fromisoformat(
                str(obs_time_str).replace("Z", "+00:00")
            )
    except (ValueError, OSError):
        return None

    raw = entry.get("rawOb", "")

    # Parse 6-hourly and 24-hourly temperature groups from remarks
    max_6hr, min_6hr = _parse_6hr_temps(raw)
    max_24hr, min_24hr = _parse_24hr_temps(raw)

    return METARObservation(
        station=str(station).upper(),
        observed_at=observed_at,
        temp_c=_safe_float(entry.get("temp")),
        dewpoint_c=_safe_float(entry.get("dewp")),
        wind_speed_kts=_safe_float(entry.get("wspd")),
        wind_gust_kts=_safe_float(entry.get("wgst")),
        altimeter_inhg=_safe_float(entry.get("altim")),
        visibility_mi=_safe_float(entry.get("visib")),
        wx_string=entry.get("wxString"),
        max_temp_6hr_c=max_6hr,
        min_temp_6hr_c=min_6hr,
        max_temp_24hr_c=max_24hr,
        min_temp_24hr_c=min_24hr,
        raw_metar=raw,
    )


def _parse_6hr_temps(raw_metar: str) -> tuple[float | None, float | None]:
    """Extract 6-hourly max/min temperature groups from METAR remarks.

    Format: 1snTTT (max) / 2snTTT (min)
    - s = 0 for positive, 1 for negative
    - TTT = temperature in tenths of Celsius
    """
    max_temp = None
    min_temp = None

    # 1xxxx = 6-hour maximum temperature
    max_match = re.search(r"\b1([01])(\d{3})\b", raw_metar)
    if max_match:
        sign = -1 if max_match.group(1) == "1" else 1
        max_temp = sign * int(max_match.group(2)) / 10.0

    # 2xxxx = 6-hour minimum temperature
    min_match = re.search(r"\b2([01])(\d{3})\b", raw_metar)
    if min_match:
        sign = -1 if min_match.group(1) == "1" else 1
        min_temp = sign * int(min_match.group(2)) / 10.0

    return max_temp, min_temp


def _parse_24hr_temps(raw_metar: str) -> tuple[float | None, float | None]:
    """Extract 24-hour max/min temperature group from METAR remarks.

    Format: 4snTTTsnTTT
    - First snTTT = 24-hour max
    - Second snTTT = 24-hour min
    """
    match = re.search(r"\b4([01])(\d{3})([01])(\d{3})\b", raw_metar)
    if not match:
        return None, None

    max_sign = -1 if match.group(1) == "1" else 1
    max_temp = max_sign * int(match.group(2)) / 10.0

    min_sign = -1 if match.group(3) == "1" else 1
    min_temp = min_sign * int(match.group(4)) / 10.0

    return max_temp, min_temp


def _safe_float(val: Any) -> float | None:
    if val is None:
        return None
    try:
        return float(val)
    except (ValueError, TypeError):
        return None
