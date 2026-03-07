"""Tests for ASOS observation fetcher."""

from datetime import datetime, timezone

import httpx
import pytest

from data.mesonet import ASOSObservation, _safe_float, fetch_observation


def test_safe_float_valid():
    assert _safe_float(72.5) == 72.5
    assert _safe_float("32.0") == 32.0
    assert _safe_float(0) == 0.0


def test_safe_float_missing():
    assert _safe_float(None) is None
    assert _safe_float("M") is None
    assert _safe_float(-99) is None
    assert _safe_float(-9999) is None


def test_observation_is_frozen():
    ob = ASOSObservation(
        station="KORD",
        observed_at=datetime.now(timezone.utc),
        temperature_f=72.0,
        wind_speed_kts=10.0,
        wind_gust_kts=None,
        precip_inch=0.0,
        raw={},
        staleness_seconds=30.0,
        is_stale=False,
    )
    assert ob.station == "KORD"
    assert ob.is_stale is False

    with pytest.raises(AttributeError):
        ob.station = "KJFK"  # type: ignore[misc]


def test_staleness_flag():
    ob_fresh = ASOSObservation(
        station="KORD",
        observed_at=datetime.now(timezone.utc),
        temperature_f=72.0,
        wind_speed_kts=None,
        wind_gust_kts=None,
        precip_inch=None,
        raw={},
        staleness_seconds=60.0,
        is_stale=False,
    )
    assert ob_fresh.is_stale is False

    ob_stale = ASOSObservation(
        station="KORD",
        observed_at=datetime.now(timezone.utc),
        temperature_f=72.0,
        wind_speed_kts=None,
        wind_gust_kts=None,
        precip_inch=None,
        raw={},
        staleness_seconds=600.0,
        is_stale=True,
    )
    assert ob_stale.is_stale is True
