"""Tests for ASOS observation fetcher."""

from datetime import UTC, datetime

import pytest

from data.mesonet import _STATION_MAP, ASOSObservation, _first_key, _safe_float


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
        observed_at=datetime.now(UTC),
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
        observed_at=datetime.now(UTC),
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
        observed_at=datetime.now(UTC),
        temperature_f=72.0,
        wind_speed_kts=None,
        wind_gust_kts=None,
        precip_inch=None,
        raw={},
        staleness_seconds=600.0,
        is_stale=True,
    )
    assert ob_stale.is_stale is True


def test_station_map_known_stations():
    """Known ICAO stations should map to stripped ID + state network."""
    assert _STATION_MAP["KORD"] == ("ORD", "IL_ASOS")
    assert _STATION_MAP["KJFK"] == ("JFK", "NY_ASOS")
    assert _STATION_MAP["KDEN"] == ("DEN", "CO_ASOS")
    assert _STATION_MAP["KLAX"] == ("LAX", "CA_ASOS")
    assert _STATION_MAP["KIAH"] == ("IAH", "TX_ASOS")


def test_station_map_fallback():
    """Unknown stations should strip K prefix and use generic ASOS network."""
    mesonet_id, network = _STATION_MAP.get("KATL", ("KATL".lstrip("K"), "ASOS"))
    assert mesonet_id == "ATL"
    assert network == "ASOS"


def test_new_field_names_fallback():
    """The _first_key fallback should pick up new Mesonet field names."""
    ob = {"airtemp[F]": 72.5, "windspeed[kt]": 12.0, "windgust[kt]": 18.0, "precip_today[in]": 0.05}
    assert _safe_float(_first_key(ob, "tmpf", "airtemp[F]")) == 72.5
    assert _safe_float(_first_key(ob, "sknt", "windspeed[kt]")) == 12.0
    assert _safe_float(_first_key(ob, "gust", "windgust[kt]")) == 18.0
    assert _safe_float(_first_key(ob, "p01i", "precip_today[in]")) == 0.05


def test_old_field_names_still_work():
    """Old-style field names should still be parsed correctly."""
    ob = {"tmpf": 65.0, "sknt": 8.0, "gust": None, "p01i": 0.0}
    assert _safe_float(_first_key(ob, "tmpf", "airtemp[F]")) == 65.0
    assert _safe_float(_first_key(ob, "sknt", "windspeed[kt]")) == 8.0
    assert _safe_float(_first_key(ob, "gust", "windgust[kt]")) is None
    assert _safe_float(_first_key(ob, "p01i", "precip_today[in]")) == 0.0
