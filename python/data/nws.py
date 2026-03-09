"""NWS API (api.weather.gov) observation fetcher.

Parallel weather data source alongside Iowa State Mesonet.
Accepts ICAO station IDs directly (KORD, KJFK, etc.).
No API key required.
"""

from __future__ import annotations

import asyncio
from datetime import datetime, timezone

import httpx
import structlog

from data.mesonet import ASOSObservation, _safe_float

logger = structlog.get_logger()

_NWS_BASE = "https://api.weather.gov"
_HEADERS = {
    "User-Agent": "(tradebot, dev@localhost)",
    "Accept": "application/geo+json",
}


def _c_to_f(celsius: float | None) -> float | None:
    """Convert Celsius to Fahrenheit."""
    if celsius is None:
        return None
    return celsius * 9.0 / 5.0 + 32.0


def _kmh_to_kts(kmh: float | None) -> float | None:
    """Convert km/h to knots."""
    if kmh is None:
        return None
    return kmh / 1.852


def _extract_value(obj: dict | None) -> float | None:
    """Extract 'value' from NWS quantity object like {"value": 22.3, "unitCode": "..."}."""
    if obj is None:
        return None
    val = obj.get("value")
    if val is None:
        return None
    return float(val)


async def fetch_nws_observation(
    client: httpx.AsyncClient,
    station: str,
) -> ASOSObservation:
    """Fetch latest observation from NWS API.

    Returns the same ASOSObservation dataclass for compatibility.
    Retries up to 3 times with 2s backoff on network errors.
    """
    url = f"{_NWS_BASE}/stations/{station}/observations/latest"

    last_exc: Exception | None = None
    for attempt in range(3):
        try:
            resp = await client.get(url, headers=_HEADERS)
            resp.raise_for_status()
            break
        except (httpx.HTTPError, httpx.StreamError) as exc:
            last_exc = exc
            if attempt < 2:
                delay = 2.0 * (attempt + 1)
                logger.warning(
                    "nws_fetch_retry",
                    station=station,
                    attempt=attempt + 1,
                    delay=delay,
                    error=str(exc),
                )
                await asyncio.sleep(delay)
    else:
        raise ConnectionError(
            f"Failed to fetch NWS observation for {station} after 3 attempts"
        ) from last_exc

    data = resp.json()
    props = data.get("properties", {})

    if not props:
        raise ValueError(f"No NWS observation data for station {station}")

    # Parse timestamp
    timestamp_str = props.get("timestamp")
    if timestamp_str:
        observed_at = datetime.fromisoformat(timestamp_str).replace(tzinfo=timezone.utc)
    else:
        observed_at = datetime.now(timezone.utc)

    now = datetime.now(timezone.utc)
    staleness = (now - observed_at).total_seconds()

    # Extract and convert units
    temp_c = _extract_value(props.get("temperature"))
    wind_kmh = _extract_value(props.get("windSpeed"))
    gust_kmh = _extract_value(props.get("windGust"))

    return ASOSObservation(
        station=station,
        observed_at=observed_at,
        temperature_f=_c_to_f(temp_c),
        wind_speed_kts=_kmh_to_kts(wind_kmh),
        wind_gust_kts=_kmh_to_kts(gust_kmh),
        precip_inch=None,  # NWS latest obs doesn't provide precip_today reliably
        raw=props,
        staleness_seconds=staleness,
        is_stale=staleness > 300,
    )


async def fetch_all_nws_stations(
    stations: list[str],
) -> dict[str, ASOSObservation]:
    """Fetch NWS observations for all stations concurrently.

    Returns a dict keyed by station code. Failed stations are logged
    and omitted from the result rather than failing the entire batch.
    """
    transport = httpx.AsyncHTTPTransport(retries=0)
    async with httpx.AsyncClient(
        transport=transport, timeout=httpx.Timeout(15.0)
    ) as client:
        tasks = {
            station: fetch_nws_observation(client, station)
            for station in stations
        }
        results: dict[str, ASOSObservation] = {}
        for station, coro in tasks.items():
            try:
                results[station] = await coro
            except Exception:
                logger.exception("nws_station_failed", station=station)
        return results
