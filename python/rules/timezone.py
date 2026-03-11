"""Local-standard-time day boundary computation for weather contracts.

Kalshi weather contracts settle on the NWS Daily Climate Report, which uses
local standard time (LST) — not daylight saving time. During DST months the
reporting day effectively runs from 01:00 local clock time to 00:59 the next
day (since DST = LST + 1 hour).

This module computes the UTC-referenced day boundaries for any IANA timezone,
always using the zone's standard (non-DST) UTC offset.
"""

from __future__ import annotations

from datetime import UTC, date, datetime, time, timedelta
from zoneinfo import ZoneInfo


def compute_day_boundaries(
    station_tz: str,
    target_date: date,
) -> tuple[datetime, datetime]:
    """Return (day_start_utc, day_end_utc) in local STANDARD time.

    Uses the standard UTC offset for the timezone (ignoring DST), so
    the boundaries are consistent year-round relative to the station's
    winter clock.

    Args:
        station_tz: IANA timezone string, e.g. 'America/Chicago'.
        target_date: The calendar date of the reporting day.

    Returns:
        Tuple of (start, end) as timezone-aware UTC datetimes.
        Start is midnight LST of target_date.
        End is midnight LST of target_date + 1 day.
    """
    std_offset = _standard_utc_offset(station_tz)

    # Midnight LST on target_date, expressed as UTC
    day_start_utc = datetime.combine(target_date, time(0, 0), tzinfo=UTC) - std_offset

    day_end_utc = day_start_utc + timedelta(days=1)

    return day_start_utc, day_end_utc


def _standard_utc_offset(tz_name: str) -> timedelta:
    """Get the standard (non-DST) UTC offset for a timezone.

    Probes January 1 of a non-ambiguous year to find the winter offset,
    which is the standard offset for zones that observe DST.
    """
    tz = ZoneInfo(tz_name)
    # January 1 is always standard time in the US
    winter = datetime(2024, 1, 15, 12, 0, tzinfo=tz)
    return winter.utcoffset() or timedelta(0)


# Pre-computed standard offsets for known stations
STATION_TIMEZONES: dict[str, str] = {
    "KORD": "America/Chicago",
    "KJFK": "America/New_York",
    "KDEN": "America/Denver",
    "KLAX": "America/Los_Angeles",
    "KIAH": "America/Chicago",
}
