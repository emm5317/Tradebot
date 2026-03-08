"""HRRR forecast fetcher via Open-Meteo API.

Provides high-resolution (15-min) weather forecasts for remaining-day
max/min estimation in settlement-aware weather fair-value computation.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any

import httpx
import structlog

logger = structlog.get_logger()

_BASE_URL = "https://api.open-meteo.com/v1/forecast"
_REQUEST_TIMEOUT = 15.0

# Station coordinates for HRRR grid point lookup
STATION_COORDS: dict[str, tuple[float, float]] = {
    "KORD": (41.9742, -87.9073),   # Chicago O'Hare
    "KJFK": (40.6413, -73.7781),   # New York JFK
    "KDEN": (39.8561, -104.6737),  # Denver
    "KLAX": (33.9425, -118.4081),  # Los Angeles
    "KIAH": (29.9902, -95.3368),   # Houston Intercontinental
}


@dataclass
class HRRRForecast:
    """Single HRRR forecast point."""

    station: str
    forecast_time: datetime
    run_time: datetime
    temp_2m_f: float | None = None
    temp_2m_c: float | None = None
    wind_10m_kts: float | None = None
    precip_mm: float | None = None


async def fetch_hrrr_forecasts(
    stations: list[str],
    forecast_hours: int = 24,
) -> list[HRRRForecast]:
    """Fetch HRRR forecasts from Open-Meteo for given stations.

    Args:
        stations: ICAO station codes (must be in STATION_COORDS)
        forecast_hours: Hours of forecast to request

    Returns:
        List of forecast points sorted by station and time.
    """
    results: list[HRRRForecast] = []

    transport = httpx.AsyncHTTPTransport(retries=2)
    async with httpx.AsyncClient(
        transport=transport,
        timeout=httpx.Timeout(_REQUEST_TIMEOUT),
    ) as client:
        for station in stations:
            coords = STATION_COORDS.get(station)
            if coords is None:
                logger.debug("hrrr_no_coords", station=station)
                continue

            lat, lon = coords
            forecasts = await _fetch_station(client, station, lat, lon, forecast_hours)
            results.extend(forecasts)

    return results


async def _fetch_station(
    client: httpx.AsyncClient,
    station: str,
    lat: float,
    lon: float,
    forecast_hours: int,
) -> list[HRRRForecast]:
    """Fetch HRRR forecast for a single station."""
    params = {
        "latitude": str(lat),
        "longitude": str(lon),
        "hourly": "temperature_2m,wind_speed_10m,precipitation",
        "temperature_unit": "celsius",
        "wind_speed_unit": "kn",
        "precipitation_unit": "mm",
        "forecast_hours": str(forecast_hours),
        "models": "gfs_hrrr",
        "timezone": "UTC",
    }

    try:
        resp = await client.get(_BASE_URL, params=params)
    except httpx.HTTPError as exc:
        logger.warning("hrrr_fetch_error", station=station, error=str(exc))
        return []

    if resp.status_code != 200:
        logger.warning(
            "hrrr_fetch_failed",
            station=station,
            status=resp.status_code,
        )
        return []

    try:
        data = resp.json()
    except Exception:
        logger.warning("hrrr_parse_error", station=station)
        return []

    return _parse_hourly(data, station)


def _parse_hourly(data: dict[str, Any], station: str) -> list[HRRRForecast]:
    """Parse Open-Meteo hourly response into HRRRForecast objects."""
    hourly = data.get("hourly")
    if not hourly:
        return []

    times = hourly.get("time", [])
    temps = hourly.get("temperature_2m", [])
    winds = hourly.get("wind_speed_10m", [])
    precips = hourly.get("precipitation", [])

    now = datetime.now(timezone.utc)

    results: list[HRRRForecast] = []
    for i, time_str in enumerate(times):
        try:
            ft = datetime.fromisoformat(time_str.replace("Z", "+00:00"))
            if ft.tzinfo is None:
                ft = ft.replace(tzinfo=timezone.utc)
        except (ValueError, AttributeError):
            continue

        temp_c = _safe_float(temps[i] if i < len(temps) else None)
        temp_f = (temp_c * 9.0 / 5.0 + 32.0) if temp_c is not None else None

        results.append(
            HRRRForecast(
                station=station,
                forecast_time=ft,
                run_time=now,
                temp_2m_c=temp_c,
                temp_2m_f=temp_f,
                wind_10m_kts=_safe_float(winds[i] if i < len(winds) else None),
                precip_mm=_safe_float(precips[i] if i < len(precips) else None),
            )
        )

    return results


def _safe_float(val: Any) -> float | None:
    if val is None:
        return None
    try:
        return float(val)
    except (ValueError, TypeError):
        return None
