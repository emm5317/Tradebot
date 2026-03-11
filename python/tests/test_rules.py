"""Tests for the contract rules resolver, ticker parser, and timezone utilities."""

from datetime import UTC, date, datetime

from rules.resolver import ContractRules
from rules.ticker_parser import (
    CFB_RTI_EXCHANGES,
    STATION_MAP,
    _extract_city_abbrev,
    _extract_crypto_strike,
    _extract_weather_strike,
    parse_ticker,
)
from rules.timezone import STATION_TIMEZONES, compute_day_boundaries

# ─── Ticker Parser Tests ─────────────────────────────────────────────


class TestParseTickerCrypto:
    def test_crypto_ticker_basic(self):
        result = parse_ticker("KXBTCD-26MAR08-T98500", category="crypto")
        assert result is not None
        assert result.contract_type == "crypto_binary"
        assert result.settlement_source == "cfb_rti"
        assert result.strike == 98500.0
        assert result.underlying == "BTCUSD"
        assert result.constituent_exchanges == CFB_RTI_EXCHANGES
        assert result.series_ticker == "KXBTCD"

    def test_crypto_ticker_with_b_prefix(self):
        result = parse_ticker("KXBTC-26MAR08-B95000", category="crypto")
        assert result is not None
        assert result.contract_type == "crypto_binary"
        assert result.strike == 95000.0

    def test_crypto_inferred_from_title(self):
        result = parse_ticker(
            "SOMEPREFIX-26MAR08-T50000",
            title="Bitcoin above $50,000",
            category="crypto",
        )
        assert result is not None
        assert result.contract_type == "crypto_binary"


class TestParseTickerWeather:
    def test_weather_high_ticker(self):
        result = parse_ticker(
            "KXTEMP-26MAR08-CHI-T45",
            title="Chicago High Temperature Above 45°F",
            category="weather",
        )
        assert result is not None
        assert result.contract_type in ("weather_max", "weather_min")
        assert result.settlement_source == "nws_cli_dsm"
        assert result.settlement_station == "KORD"
        assert result.settlement_tz == "America/Chicago"
        assert result.strike == 45.0
        assert result.city_abbrev == "CHI"

    def test_weather_nyc(self):
        result = parse_ticker(
            "KXTEMP-26MAR08-NYC-T32",
            title="New York High Temperature Above 32°F",
            category="weather",
        )
        assert result is not None
        assert result.settlement_station == "KJFK"
        assert result.settlement_tz == "America/New_York"
        assert result.strike == 32.0

    def test_weather_denver(self):
        result = parse_ticker(
            "KXTEMP-26MAR08-DEN-T50",
            title="Denver temperature",
            category="weather",
        )
        assert result is not None
        assert result.settlement_station == "KDEN"
        assert result.settlement_tz == "America/Denver"

    def test_weather_high_from_title(self):
        result = parse_ticker(
            "HIGHTEMP-CHI-26MAR08-65",
            title="Chicago High Temperature Above 65°F",
            category="weather",
        )
        assert result is not None
        assert result.contract_type == "weather_max"

    def test_weather_low_from_title(self):
        result = parse_ticker(
            "LOWTEMP-NYC-26MAR08-28",
            title="New York Low Temperature Below 28°F",
            category="weather",
        )
        assert result is not None
        assert result.contract_type == "weather_min"


class TestParseTickerEdgeCases:
    def test_empty_ticker_returns_none(self):
        assert parse_ticker("") is None

    def test_unknown_format_with_category(self):
        result = parse_ticker(
            "UNKNOWN-TICKER-123",
            title="Bitcoin price above $100,000",
            category="crypto",
        )
        assert result is not None
        assert result.contract_type == "crypto_binary"

    def test_all_stations_mapped(self):
        for _abbrev, (station, tz) in STATION_MAP.items():
            assert station.startswith("K")
            assert "America/" in tz


# ─── Strike Extraction Tests ─────────────────────────────────────────


class TestCryptoStrike:
    def test_t_prefix(self):
        assert _extract_crypto_strike("KXBTCD-26MAR08-T98500") == 98500.0

    def test_b_prefix(self):
        assert _extract_crypto_strike("KXBTC-26MAR08-B95000") == 95000.0

    def test_no_match(self):
        assert _extract_crypto_strike("KXBTC-26MAR08") is None


class TestWeatherStrike:
    def test_ticker_embedded(self):
        assert _extract_weather_strike("KXTEMP-CHI-T45", "") == 45.0

    def test_title_fallback(self):
        assert _extract_weather_strike("KXTEMP-CHI", "above 72°F") == 72.0

    def test_title_below(self):
        assert _extract_weather_strike("KXTEMP-DEN", "below 30°F") == 30.0

    def test_no_match(self):
        assert _extract_weather_strike("KXTEMP-CHI", "some random title") is None


# ─── City Extraction Tests ───────────────────────────────────────────


class TestCityAbbrev:
    def test_ticker_segment(self):
        assert _extract_city_abbrev("KXTEMP-26MAR08-CHI-T45", "") == "CHI"

    def test_title_fallback(self):
        assert _extract_city_abbrev("KXTEMP-26MAR08-T45", "Chicago High Temperature") == "CHI"
        assert _extract_city_abbrev("KXTEMP-26MAR08-T45", "New York Temperature") == "NYC"
        assert _extract_city_abbrev("KXTEMP-26MAR08-T45", "Houston High") == "HOU"

    def test_no_city(self):
        assert _extract_city_abbrev("KXTEMP-26MAR08-T45", "Random Title") is None


# ─── Timezone / Day Boundary Tests ───────────────────────────────────


class TestDayBoundaries:
    def test_chicago_winter(self):
        """Chicago is CST = UTC-6 in winter."""
        start, end = compute_day_boundaries("America/Chicago", date(2024, 1, 15))
        # Midnight CST = 06:00 UTC
        assert start.hour == 6
        assert start.minute == 0
        assert (end - start).total_seconds() == 86400  # exactly 24 hours

    def test_chicago_summer_uses_standard_time(self):
        """During DST, boundaries should still use standard time (CST, not CDT)."""
        start, end = compute_day_boundaries("America/Chicago", date(2024, 7, 15))
        # Should still be UTC-6 (CST), not UTC-5 (CDT)
        assert start.hour == 6
        assert (end - start).total_seconds() == 86400

    def test_new_york_winter(self):
        """New York is EST = UTC-5."""
        start, end = compute_day_boundaries("America/New_York", date(2024, 1, 15))
        assert start.hour == 5
        assert start.minute == 0

    def test_new_york_summer_uses_standard_time(self):
        """During DST, should still use EST (UTC-5), not EDT (UTC-4)."""
        start, end = compute_day_boundaries("America/New_York", date(2024, 7, 15))
        assert start.hour == 5

    def test_denver_winter(self):
        """Denver is MST = UTC-7."""
        start, end = compute_day_boundaries("America/Denver", date(2024, 1, 15))
        assert start.hour == 7

    def test_denver_summer_uses_standard_time(self):
        start, end = compute_day_boundaries("America/Denver", date(2024, 7, 15))
        assert start.hour == 7

    def test_los_angeles_winter(self):
        """LA is PST = UTC-8."""
        start, end = compute_day_boundaries("America/Los_Angeles", date(2024, 1, 15))
        assert start.hour == 8

    def test_los_angeles_summer_uses_standard_time(self):
        start, end = compute_day_boundaries("America/Los_Angeles", date(2024, 7, 15))
        assert start.hour == 8

    def test_dst_transition_day_march(self):
        """On DST spring-forward day (Mar 10, 2024), boundaries use standard time."""
        start, end = compute_day_boundaries("America/Chicago", date(2024, 3, 10))
        assert start.hour == 6  # CST = UTC-6, not CDT
        assert (end - start).total_seconds() == 86400

    def test_dst_transition_day_november(self):
        """On DST fall-back day (Nov 3, 2024), boundaries use standard time."""
        start, end = compute_day_boundaries("America/Chicago", date(2024, 11, 3))
        assert start.hour == 6
        assert (end - start).total_seconds() == 86400

    def test_all_station_timezones_valid(self):
        """All stations in STATION_TIMEZONES produce valid boundaries."""
        for _station, tz in STATION_TIMEZONES.items():
            start, end = compute_day_boundaries(tz, date(2024, 6, 15))
            assert end > start
            assert (end - start).total_seconds() == 86400

    def test_boundaries_are_utc(self):
        start, end = compute_day_boundaries("America/Chicago", date(2024, 1, 15))
        assert start.tzinfo == UTC
        assert end.tzinfo == UTC


# ─── ContractRules Dataclass Tests ───────────────────────────────────


class TestContractRules:
    def test_crypto_properties(self):
        rules = ContractRules(
            market_ticker="KXBTCD-26MAR08-T98500",
            series_ticker="KXBTCD",
            contract_type="crypto_binary",
            settlement_source="cfb_rti",
            settlement_station=None,
            settlement_tz=None,
            strike=98500.0,
            expiry_time=datetime(2026, 3, 8, 22, 0, tzinfo=UTC),
            underlying="BTCUSD",
            constituent_exchanges=CFB_RTI_EXCHANGES,
        )
        assert rules.is_crypto
        assert not rules.is_weather
        assert rules.signal_type == "crypto"

    def test_weather_properties(self):
        rules = ContractRules(
            market_ticker="KXTEMP-26MAR08-CHI-T45",
            series_ticker="KXTEMP",
            contract_type="weather_max",
            settlement_source="nws_cli_dsm",
            settlement_station="KORD",
            settlement_tz="America/Chicago",
            strike=45.0,
            expiry_time=datetime(2026, 3, 8, 22, 0, tzinfo=UTC),
        )
        assert rules.is_weather
        assert not rules.is_crypto
        assert rules.signal_type == "weather"
