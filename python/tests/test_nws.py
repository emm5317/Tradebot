"""Tests for NWS API observation fetcher."""

from data.nws import _c_to_f, _extract_value, _kmh_to_kts


def test_c_to_f():
    assert _c_to_f(0.0) == 32.0
    assert _c_to_f(100.0) == 212.0
    assert abs(_c_to_f(22.2) - 72.0) < 0.1
    assert abs(_c_to_f(-40.0) - (-40.0)) < 0.01  # -40 is the same in both


def test_c_to_f_none():
    assert _c_to_f(None) is None


def test_kmh_to_kts():
    assert abs(_kmh_to_kts(1.852) - 1.0) < 0.001
    assert abs(_kmh_to_kts(18.52) - 10.0) < 0.01
    assert abs(_kmh_to_kts(0.0) - 0.0) < 0.001


def test_kmh_to_kts_none():
    assert _kmh_to_kts(None) is None


def test_extract_value():
    assert _extract_value({"value": 22.3, "unitCode": "wmoUnit:degC"}) == 22.3
    assert _extract_value({"value": 0.0, "unitCode": "wmoUnit:degC"}) == 0.0


def test_extract_value_null():
    """NWS returns {"value": null} for missing data."""
    assert _extract_value({"value": None, "unitCode": "wmoUnit:degC"}) is None


def test_extract_value_none_obj():
    assert _extract_value(None) is None


def test_extract_value_empty_dict():
    assert _extract_value({}) is None


def test_unit_conversions_round_trip():
    """Verify common weather values convert correctly."""
    # 20°C = 68°F
    assert abs(_c_to_f(20.0) - 68.0) < 0.01

    # 10 km/h ≈ 5.4 kts
    assert abs(_kmh_to_kts(10.0) - 5.3996) < 0.01

    # 50 km/h ≈ 27 kts
    assert abs(_kmh_to_kts(50.0) - 27.0) < 0.1
