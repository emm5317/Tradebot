"""BE-3.1: ASOS Observation Fetcher — Iowa State Mesonet."""

from __future__ import annotations

import asyncio
from dataclasses import dataclass
from datetime import UTC, datetime

import httpx
import structlog

logger = structlog.get_logger()

_STALENESS_THRESHOLD_SECONDS = 300  # 5 minutes

# Iowa State Mesonet requires ICAO prefix stripped and state-specific network
_STATION_MAP: dict[str, tuple[str, str]] = {
    "KORD": ("ORD", "IL_ASOS"),
    "KJFK": ("JFK", "NY_ASOS"),
    "KDEN": ("DEN", "CO_ASOS"),
    "KLAX": ("LAX", "CA_ASOS"),
    "KIAH": ("IAH", "TX_ASOS"),
}


@dataclass(frozen=True, slots=True)
class ASOSObservation:
    station: str
    observed_at: datetime
    temperature_f: float | None
    wind_speed_kts: float | None
    wind_gust_kts: float | None
    precip_inch: float | None
    raw: dict
    staleness_seconds: float
    is_stale: bool  # True if > 300 seconds old


async def fetch_observation(
    client: httpx.AsyncClient,
    station: str,
    *,
    mesonet_base_url: str = "https://mesonet.agron.iastate.edu",
) -> ASOSObservation:
    """Fetch latest 1-minute ASOS observation from Iowa State Mesonet.

    Retries up to 3 times with 2s backoff on network errors.
    Missing data fields return None rather than raising.
    """
    url = f"{mesonet_base_url}/json/current.py"
    mesonet_id, network = _STATION_MAP.get(station, (station.lstrip("K"), "ASOS"))
    params = {"station": mesonet_id, "network": network}

    last_exc: Exception | None = None
    for attempt in range(3):
        try:
            resp = await client.get(url, params=params)
            resp.raise_for_status()
            break
        except (httpx.HTTPError, httpx.StreamError) as exc:
            last_exc = exc
            if attempt < 2:
                delay = 2.0 * (attempt + 1)
                logger.warning(
                    "mesonet_fetch_retry",
                    station=station,
                    attempt=attempt + 1,
                    delay=delay,
                    error=str(exc),
                )
                await asyncio.sleep(delay)
    else:
        raise ConnectionError(f"Failed to fetch observation for {station} after 3 attempts") from last_exc

    data = resp.json()

    # Mesonet returns {"last_ob": {...}} for the current observation
    ob = data.get("last_ob", {})
    if not ob:
        raise ValueError(f"No observation data returned for station {station}")

    # Parse observation timestamp
    utc_valid = ob.get("utc_valid")
    if utc_valid:
        observed_at = datetime.fromisoformat(utc_valid).replace(tzinfo=UTC)
    else:
        observed_at = datetime.now(UTC)

    now = datetime.now(UTC)
    staleness = (now - observed_at).total_seconds()

    return ASOSObservation(
        station=station,
        observed_at=observed_at,
        temperature_f=_safe_float(_first_key(ob, "tmpf", "airtemp[F]")),
        wind_speed_kts=_safe_float(_first_key(ob, "sknt", "windspeed[kt]")),
        wind_gust_kts=_safe_float(_first_key(ob, "gust", "windgust[kt]")),
        precip_inch=_safe_float(_first_key(ob, "p01i", "precip_today[in]")),
        raw=ob,
        staleness_seconds=staleness,
        is_stale=staleness > _STALENESS_THRESHOLD_SECONDS,
    )


async def fetch_all_stations(
    stations: list[str],
    *,
    mesonet_base_url: str = "https://mesonet.agron.iastate.edu",
) -> dict[str, ASOSObservation]:
    """Fetch observations for all stations concurrently.

    Returns a dict keyed by station code. Failed stations are logged
    and omitted from the result rather than failing the entire batch.
    """
    transport = httpx.AsyncHTTPTransport(retries=0)  # we handle retries ourselves
    async with httpx.AsyncClient(transport=transport, timeout=httpx.Timeout(15.0)) as client:
        tasks = {station: fetch_observation(client, station, mesonet_base_url=mesonet_base_url) for station in stations}
        gathered = await asyncio.gather(*tasks.values(), return_exceptions=True)
        results: dict[str, ASOSObservation] = {}
        for station, result in zip(tasks.keys(), gathered):
            if isinstance(result, Exception):
                logger.exception(
                    "mesonet_station_failed",
                    station=station,
                    error=str(result),
                )
            else:
                results[station] = result
        return results


def _first_key(d: dict, *keys: str) -> object:
    """Return the value for the first key present in the dict (even if falsy)."""
    for k in keys:
        if k in d:
            return d[k]
    return None


def _safe_float(value: object) -> float | None:
    """Convert a value to float, returning None for missing or invalid data."""
    if value is None:
        return None
    try:
        f = float(value)
        # Mesonet uses -99 / -9999 as sentinel for missing data
        if f <= -99:
            return None
        return f
    except (ValueError, TypeError):
        return None
