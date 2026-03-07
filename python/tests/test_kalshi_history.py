"""Tests for Kalshi historical data pull utility functions."""

from data.kalshi_history import (
    _categorize,
    _extract_city,
    _extract_station,
    _extract_threshold,
    _matches_category,
)


def test_matches_category_weather():
    assert _matches_category({"category": "weather", "title": ""}, "weather")
    assert _matches_category({"category": "", "title": "Will temperature exceed 90"}, "weather")
    assert not _matches_category({"category": "crypto", "title": "Bitcoin above 50k"}, "weather")


def test_matches_category_crypto():
    assert _matches_category({"category": "crypto", "title": ""}, "crypto")
    assert _matches_category({"category": "", "title": "Bitcoin price above 60k"}, "crypto")
    assert not _matches_category({"category": "weather", "title": "Chicago temp"}, "crypto")


def test_categorize():
    assert _categorize({"category": "weather", "title": ""}) == "weather"
    assert _categorize({"category": "crypto", "title": ""}) == "crypto"
    assert _categorize({"category": "", "title": "BTC above 70k"}) == "crypto"
    assert _categorize({"category": "", "title": "Wind speed in Chicago"}) == "weather"


def test_extract_city():
    assert _extract_city({"title": "Chicago temperature", "subtitle": ""}) == "Chicago"
    assert _extract_city({"title": "", "subtitle": "New York weather"}) == "New York"
    assert _extract_city({"title": "Random market", "subtitle": ""}) is None


def test_extract_station():
    market = {"title": "Chicago temperature above 32", "subtitle": ""}
    assert _extract_station(market) == "KORD"

    market = {"title": "New York wind speed", "subtitle": ""}
    assert _extract_station(market) == "KJFK"


def test_extract_threshold():
    assert _extract_threshold({"title": "Temperature above 32°F"}) == 32.0
    assert _extract_threshold({"title": "Wind speed over 25 mph"}) == 25.0
    assert _extract_threshold({"title": "Will it rain?"}) is None
