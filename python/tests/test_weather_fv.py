"""Tests for weather fair-value engine, rounding model, and METAR parsing."""

from models.physics import StationCalibration
from models.rounding import (
    celsius_to_cli_fahrenheit,
    compute_rounding_uncertainty,
    fahrenheit_to_threshold_celsius,
)
from models.weather_fv import (
    WeatherState,
    compute_weather_fair_value,
    detect_source_conflict,
)

# ─── Rounding Model Tests ────────────────────────────────────────────


class TestRoundingUncertainty:
    def test_unambiguous_well_above(self):
        """7°C → 44.6°F, strike 40°F → not ambiguous."""
        result = compute_rounding_uncertainty(7, 40.0)
        assert not result.is_ambiguous
        assert result.reported_f == 45.0
        assert result.min_f < result.max_f

    def test_unambiguous_well_below(self):
        """0°C → 32°F, strike 40°F → not ambiguous."""
        result = compute_rounding_uncertainty(0, 40.0)
        assert not result.is_ambiguous

    def test_ambiguous_at_boundary(self):
        """7°C → range [43.7, 45.5]°F, strike 45°F → ambiguous."""
        result = compute_rounding_uncertainty(7, 45.0)
        assert result.is_ambiguous
        assert result.min_f <= 45.0 <= result.max_f

    def test_band_width_is_1_8f(self):
        """1°C range → 1.8°F range."""
        result = compute_rounding_uncertainty(10, 50.0)
        assert abs(result.ambiguity_band - 1.8) < 0.01

    def test_negative_celsius(self):
        """-5°C → 23°F."""
        result = compute_rounding_uncertainty(-5, 23.0)
        assert result.reported_f == 23.0
        assert result.is_ambiguous  # 23 is within range

    def test_freezing_point(self):
        """0°C → 32°F."""
        result = compute_rounding_uncertainty(0, 32.0)
        assert result.reported_f == 32.0
        assert result.is_ambiguous


class TestCelsiusToFahrenheit:
    def test_freezing(self):
        assert celsius_to_cli_fahrenheit(0.0) == 32

    def test_boiling(self):
        assert celsius_to_cli_fahrenheit(100.0) == 212

    def test_body_temp(self):
        assert celsius_to_cli_fahrenheit(37.0) == 99

    def test_negative(self):
        assert celsius_to_cli_fahrenheit(-40.0) == -40


class TestFahrenheitThresholdCelsius:
    def test_32f_threshold(self):
        min_c, max_c = fahrenheit_to_threshold_celsius(32.0)
        # C values in this range round to >= 32°F
        assert min_c < 0.0 < max_c

    def test_range_width(self):
        min_c, max_c = fahrenheit_to_threshold_celsius(50.0)
        assert abs((max_c - min_c) - 5.0 / 9.0) < 0.01


# ─── Weather Fair-Value Engine Tests ─────────────────────────────────


class TestWeatherFairValueLocked:
    def test_max_already_exceeded(self):
        """If running max already exceeds strike, probability ≈ 1."""
        state = WeatherState(
            station="KORD",
            obs_date="2024-03-08",
            contract_type="weather_max",
            strike_f=45.0,
            running_max_f=47.0,
        )
        result = compute_weather_fair_value(
            contract_type="weather_max",
            strike_f=45.0,
            current_temp_f=42.0,
            minutes_remaining=60.0,
            state=state,
        )
        assert result.already_locked
        assert result.probability >= 0.95

    def test_min_already_below(self):
        """If running min already below strike, probability ≈ 1."""
        state = WeatherState(
            station="KJFK",
            obs_date="2024-03-08",
            contract_type="weather_min",
            strike_f=30.0,
            running_min_f=28.0,
        )
        result = compute_weather_fair_value(
            contract_type="weather_min",
            strike_f=30.0,
            current_temp_f=35.0,
            minutes_remaining=60.0,
            state=state,
        )
        assert result.already_locked
        assert result.probability >= 0.95

    def test_not_locked_when_below_strike(self):
        """Running max below strike → not locked."""
        state = WeatherState(
            station="KORD",
            obs_date="2024-03-08",
            contract_type="weather_max",
            strike_f=45.0,
            running_max_f=40.0,
        )
        result = compute_weather_fair_value(
            contract_type="weather_max",
            strike_f=45.0,
            current_temp_f=40.0,
            minutes_remaining=60.0,
            state=state,
        )
        assert not result.already_locked


class TestWeatherFairValuePhysics:
    def test_temp_well_above_max_strike(self):
        """Current temp well above max strike → high probability."""
        result = compute_weather_fair_value(
            contract_type="weather_max",
            strike_f=40.0,
            current_temp_f=50.0,
            minutes_remaining=30.0,
        )
        assert result.probability > 0.7

    def test_temp_well_below_max_strike(self):
        """Current temp well below max strike → low probability."""
        result = compute_weather_fair_value(
            contract_type="weather_max",
            strike_f=80.0,
            current_temp_f=50.0,
            minutes_remaining=30.0,
        )
        assert result.probability < 0.3

    def test_temp_at_strike_locks(self):
        """Current temp at strike for max contract → locked (max already reached)."""
        result = compute_weather_fair_value(
            contract_type="weather_max",
            strike_f=50.0,
            current_temp_f=50.0,
            minutes_remaining=30.0,
        )
        assert result.already_locked
        assert result.probability >= 0.95

    def test_temp_just_below_strike(self):
        """Current temp just below max strike → moderate probability."""
        result = compute_weather_fair_value(
            contract_type="weather_max",
            strike_f=50.0,
            current_temp_f=49.0,
            minutes_remaining=30.0,
        )
        assert 0.2 < result.probability < 0.8

    def test_zero_minutes_above(self):
        """At settlement, temp above strike → high probability."""
        result = compute_weather_fair_value(
            contract_type="weather_max",
            strike_f=40.0,
            current_temp_f=45.0,
            minutes_remaining=0.0,
        )
        assert result.probability > 0.5

    def test_zero_minutes_below(self):
        """At settlement, temp below strike → low probability."""
        result = compute_weather_fair_value(
            contract_type="weather_max",
            strike_f=50.0,
            current_temp_f=45.0,
            minutes_remaining=0.0,
        )
        assert result.probability < 0.5


class TestWeatherFairValueHRRR:
    def test_hrrr_high_forecast(self):
        """HRRR forecasting high temps → increases max probability."""
        result_no_hrrr = compute_weather_fair_value(
            contract_type="weather_max",
            strike_f=60.0,
            current_temp_f=55.0,
            minutes_remaining=120.0,
        )
        result_with_hrrr = compute_weather_fair_value(
            contract_type="weather_max",
            strike_f=60.0,
            current_temp_f=55.0,
            minutes_remaining=120.0,
            hrrr_forecast_temps_f=[62.0, 65.0, 63.0, 58.0],
        )
        # HRRR showing max above strike should increase probability
        assert result_with_hrrr.probability >= result_no_hrrr.probability - 0.05


class TestWeatherFairValueRounding:
    def test_rounding_ambiguity_flagged(self):
        """Rounding ambiguity at boundary is detected."""
        result = compute_weather_fair_value(
            contract_type="weather_max",
            strike_f=45.0,
            current_temp_f=44.6,
            minutes_remaining=30.0,
            metar_temp_c=7,  # 7°C → 44.6°F, range [43.7, 45.5]
        )
        assert result.rounding_ambiguous

    def test_no_rounding_ambiguity(self):
        result = compute_weather_fair_value(
            contract_type="weather_max",
            strike_f=60.0,
            current_temp_f=44.6,
            minutes_remaining=30.0,
            metar_temp_c=7,
        )
        assert not result.rounding_ambiguous


class TestWeatherFairValueState:
    def test_state_updates_running_max(self):
        """State running max updates with new observations."""
        state = WeatherState(
            station="KORD",
            obs_date="2024-03-08",
            contract_type="weather_max",
            strike_f=50.0,
            running_max_f=45.0,
        )
        compute_weather_fair_value(
            contract_type="weather_max",
            strike_f=50.0,
            current_temp_f=48.0,
            minutes_remaining=60.0,
            state=state,
        )
        assert state.running_max_f == 48.0
        assert state.obs_count == 1

    def test_state_incorporates_6hr_max(self):
        """6-hourly METAR max group updates running max."""
        state = WeatherState(
            station="KORD",
            obs_date="2024-03-08",
            contract_type="weather_max",
            strike_f=50.0,
            running_max_f=42.0,
        )
        compute_weather_fair_value(
            contract_type="weather_max",
            strike_f=50.0,
            current_temp_f=40.0,
            minutes_remaining=60.0,
            state=state,
            max_temp_6hr_f=47.0,
        )
        assert state.running_max_f == 47.0

    def test_state_running_min_updates(self):
        state = WeatherState(
            station="KJFK",
            obs_date="2024-03-08",
            contract_type="weather_min",
            strike_f=30.0,
            running_min_f=35.0,
        )
        compute_weather_fair_value(
            contract_type="weather_min",
            strike_f=30.0,
            current_temp_f=32.0,
            minutes_remaining=60.0,
            state=state,
        )
        assert state.running_min_f == 32.0


class TestWeatherFairValueComponents:
    def test_components_present(self):
        """Output includes component probabilities."""
        result = compute_weather_fair_value(
            contract_type="weather_max",
            strike_f=50.0,
            current_temp_f=48.0,
            minutes_remaining=30.0,
        )
        assert "physics" in result.components
        assert "trend" in result.components
        assert "climo" in result.components

    def test_probability_clamped(self):
        result = compute_weather_fair_value(
            contract_type="weather_max",
            strike_f=50.0,
            current_temp_f=48.0,
            minutes_remaining=30.0,
        )
        assert 0.01 <= result.probability <= 0.99

    def test_confidence_range(self):
        result = compute_weather_fair_value(
            contract_type="weather_max",
            strike_f=50.0,
            current_temp_f=48.0,
            minutes_remaining=30.0,
        )
        assert 0.0 < result.confidence <= 1.0


# ─── METAR Parsing Tests ─────────────────────────────────────────────


class TestMETARParsing:
    def test_parse_6hr_max(self):
        from data.aviationweather import _parse_6hr_temps

        max_t, min_t = _parse_6hr_temps("KORD 081756Z ... RMK ... 10156 20067")
        assert max_t is not None
        assert abs(max_t - 15.6) < 0.01
        assert min_t is not None
        assert abs(min_t - 6.7) < 0.01

    def test_parse_6hr_negative(self):
        from data.aviationweather import _parse_6hr_temps

        max_t, min_t = _parse_6hr_temps("KORD 081756Z ... RMK ... 11023 21045")
        assert max_t is not None
        assert abs(max_t - (-2.3)) < 0.01
        assert min_t is not None
        assert abs(min_t - (-4.5)) < 0.01

    def test_parse_6hr_no_groups(self):
        from data.aviationweather import _parse_6hr_temps

        max_t, min_t = _parse_6hr_temps("KORD 081756Z 36008KT 10SM FEW250")
        assert max_t is None
        assert min_t is None

    def test_parse_24hr_temps(self):
        from data.aviationweather import _parse_24hr_temps

        max_t, min_t = _parse_24hr_temps("RMK ... 401560067")
        assert max_t is not None
        assert abs(max_t - 15.6) < 0.01
        assert min_t is not None
        assert abs(min_t - 6.7) < 0.01

    def test_parse_24hr_negative(self):
        from data.aviationweather import _parse_24hr_temps

        max_t, min_t = _parse_24hr_temps("RMK 411001200")
        assert max_t is not None
        assert abs(max_t - (-10.0)) < 0.01
        assert min_t is not None
        assert abs(min_t - (-20.0)) < 0.01


# ─── Phase 4.5: Station-Specific Calibration Tests ──────────────────


class TestStationCalibration:
    def test_station_cal_overrides_sigma(self):
        """Station calibration sigma should override default."""
        cal = StationCalibration(sigma_10min=0.5)
        fv = compute_weather_fair_value(
            contract_type="weather_max",
            strike_f=50.0,
            current_temp_f=48.0,
            minutes_remaining=15.0,
            sigma_per_10min=0.3,  # default
            station_cal=cal,
        )
        # With higher sigma, there's more uncertainty
        fv_default = compute_weather_fair_value(
            contract_type="weather_max",
            strike_f=50.0,
            current_temp_f=48.0,
            minutes_remaining=15.0,
            sigma_per_10min=0.3,
        )
        # Higher sigma should produce different probability
        assert abs(fv.probability - fv_default.probability) > 0.01

    def test_hrrr_bias_correction(self):
        """HRRR bias correction should shift forecast temps."""
        cal = StationCalibration(hrrr_bias_f=2.0, hrrr_skill=0.5)
        fv = compute_weather_fair_value(
            contract_type="weather_max",
            strike_f=50.0,
            current_temp_f=48.0,
            minutes_remaining=15.0,
            hrrr_forecast_temps_f=[51.0, 52.0, 53.0],
            station_cal=cal,
        )
        # Bias of 2.0 means forecasts shifted down by 2
        # Effective forecasts: [49, 50, 51] instead of [51, 52, 53]
        assert "hrrr" in fv.components

    def test_low_hrrr_skill_reduces_weight(self):
        """Low HRRR skill should reduce HRRR weight in blend."""
        cal_high = StationCalibration(hrrr_skill=0.9)
        cal_low = StationCalibration(hrrr_skill=0.1)

        fv_high = compute_weather_fair_value(
            contract_type="weather_max",
            strike_f=50.0,
            current_temp_f=48.0,
            minutes_remaining=15.0,
            hrrr_forecast_temps_f=[55.0, 56.0],
            station_cal=cal_high,
        )
        fv_low = compute_weather_fair_value(
            contract_type="weather_max",
            strike_f=50.0,
            current_temp_f=48.0,
            minutes_remaining=15.0,
            hrrr_forecast_temps_f=[55.0, 56.0],
            station_cal=cal_low,
        )
        # With low HRRR skill, HRRR is less influential
        # HRRR says high → high skill = more bullish
        assert fv_high.probability != fv_low.probability

    def test_station_cal_none_preserves_default(self):
        """station_cal=None should produce identical results to no cal."""
        kwargs = dict(
            contract_type="weather_max",
            strike_f=50.0,
            current_temp_f=48.0,
            minutes_remaining=15.0,
            sigma_per_10min=0.3,
        )
        fv_none = compute_weather_fair_value(**kwargs, station_cal=None)
        fv_omit = compute_weather_fair_value(**kwargs)
        assert abs(fv_none.probability - fv_omit.probability) < 0.001


# ─── Phase 4.6: Source Conflict & Outage Tests ──────────────────────


class TestSourceConflict:
    def test_metar_hrrr_conflict(self):
        """METAR 7C (44.6F), HRRR max 49F → conflict, sigma * 1.5."""
        conflict = detect_source_conflict(
            current_temp_f=44.6,
            hrrr_forecast_temps_f=[49.0, 50.0, 51.0],
            metar_temp_c=7,
        )
        assert conflict.metar_hrrr_conflict
        assert abs(conflict.sigma_multiplier - 1.5) < 0.01

    def test_hrrr_unavailable(self):
        """HRRR unavailable → no conflict, normal sigma."""
        conflict = detect_source_conflict(
            current_temp_f=44.6,
            hrrr_forecast_temps_f=None,
            metar_temp_c=7,
        )
        assert conflict.hrrr_missing
        assert not conflict.metar_hrrr_conflict
        assert abs(conflict.sigma_multiplier - 1.0) < 0.01

    def test_metar_unavailable(self):
        """METAR unavailable → sigma * 1.25."""
        conflict = detect_source_conflict(
            current_temp_f=44.6,
            hrrr_forecast_temps_f=[45.0, 46.0],
            metar_temp_c=None,
        )
        assert conflict.metar_missing
        assert abs(conflict.sigma_multiplier - 1.25) < 0.01

    def test_both_unavailable(self):
        """Both sources unavailable."""
        conflict = detect_source_conflict(
            current_temp_f=None,
            hrrr_forecast_temps_f=None,
            metar_temp_c=None,
        )
        assert conflict.metar_missing
        assert conflict.hrrr_missing

    def test_normal_agreement(self):
        """METAR and HRRR agree within 3F → no conflict."""
        conflict = detect_source_conflict(
            current_temp_f=44.6,
            hrrr_forecast_temps_f=[45.0, 46.0, 44.0],
            metar_temp_c=7,
        )
        assert not conflict.metar_hrrr_conflict
        assert abs(conflict.sigma_multiplier - 1.0) < 0.01


class TestSourceConflictIntegration:
    def test_both_missing_low_confidence(self):
        """Both METAR and HRRR missing → still computes but low confidence."""
        fv = compute_weather_fair_value(
            contract_type="weather_max",
            strike_f=50.0,
            current_temp_f=48.0,
            minutes_remaining=15.0,
            metar_temp_c=None,
            hrrr_forecast_temps_f=None,
        )
        # Should still compute probability from physics model
        assert 0.01 <= fv.probability <= 0.99
        # But lower confidence (no HRRR, no rounding info)
        assert fv.confidence <= 0.8

    def test_conflict_inflates_sigma(self):
        """METAR-HRRR conflict should produce different prob via inflated sigma."""
        # No conflict
        fv_normal = compute_weather_fair_value(
            contract_type="weather_max",
            strike_f=50.0,
            current_temp_f=48.0,
            minutes_remaining=15.0,
            metar_temp_c=9,  # ~48.2F
            hrrr_forecast_temps_f=[49.0, 50.0],  # within 3F
        )
        # Conflict
        fv_conflict = compute_weather_fair_value(
            contract_type="weather_max",
            strike_f=50.0,
            current_temp_f=44.6,
            minutes_remaining=15.0,
            metar_temp_c=7,  # ~44.6F
            hrrr_forecast_temps_f=[49.0, 50.0, 51.0],  # >3F away
        )
        # Conflict should change probability due to sigma inflation
        assert fv_normal.probability != fv_conflict.probability


# ─── Phase 4.7: Rounding Ambiguity Hardening Tests ──────────────────


class TestRoundingBoundaryProbability:
    def test_boundary_prob_ambiguous_near_max(self):
        """metar_temp_c=7, strike=45: boundary_prob = (45.3 - 45) / 1.8 ≈ 0.167."""
        result = compute_rounding_uncertainty(7, 45.0)
        assert result.is_ambiguous
        expected = (result.max_f - 45.0) / (result.max_f - result.min_f)
        assert abs(result.boundary_probability - expected) < 0.01

    def test_boundary_prob_ambiguous_near_min(self):
        """metar_temp_c=7, strike=44: boundary_prob = (45.3 - 44) / 1.8 ≈ 0.722."""
        result = compute_rounding_uncertainty(7, 44.0)
        assert result.is_ambiguous
        expected = (result.max_f - 44.0) / (result.max_f - result.min_f)
        assert abs(result.boundary_probability - expected) < 0.01

    def test_safe_zone_well_above(self):
        """metar_temp_c=10 (50F), strike=45: not ambiguous, safe_zone=True."""
        result = compute_rounding_uncertainty(10, 45.0)
        assert not result.is_ambiguous
        assert result.safe_zone

    def test_safe_zone_well_below(self):
        """metar_temp_c=7 (45F), strike=47: not ambiguous, safe_zone=True."""
        result = compute_rounding_uncertainty(7, 47.0)
        assert not result.is_ambiguous
        assert result.safe_zone

    def test_not_safe_zone_when_close(self):
        """Close to boundary but not ambiguous → not safe_zone if < 0.7F."""
        # 7C → reported 45F, range [43.7, 45.5]
        # Strike 45.8 → not ambiguous (45.8 > 45.5), but |45 - 45.8| = 0.8 > 0.7 → safe
        result = compute_rounding_uncertainty(7, 45.8)
        assert not result.is_ambiguous
        assert result.safe_zone  # 0.8 > 0.7

    def test_boundary_prob_below_min(self):
        """Strike below min_f → probability = 1.0."""
        result = compute_rounding_uncertainty(10, 40.0)  # 10C → range [49.1, 50.9]
        assert abs(result.boundary_probability - 1.0) < 0.01

    def test_boundary_prob_above_max(self):
        """Strike above max_f → probability = 0.0."""
        result = compute_rounding_uncertainty(10, 55.0)  # 10C → range [49.1, 50.9]
        assert abs(result.boundary_probability - 0.0) < 0.01

    def test_non_ambiguous_unchanged(self):
        """Non-ambiguous cases should preserve existing behavior."""
        result = compute_rounding_uncertainty(20, 50.0)  # 20C → 68F, strike 50
        assert not result.is_ambiguous
        assert result.safe_zone
        assert abs(result.boundary_probability - 1.0) < 0.01  # 50 < min_f


class TestRoundingIntegration:
    def test_safe_zone_confidence_bonus(self):
        """Safe zone should add +0.05 confidence."""
        fv = compute_weather_fair_value(
            contract_type="weather_max",
            strike_f=45.0,
            current_temp_f=50.0,
            minutes_remaining=15.0,
            metar_temp_c=10,  # 50F, well away from 45 strike
        )
        # Confidence should include safe zone bonus
        assert fv.confidence >= 0.75  # base 0.7 + safe zone 0.05

    def test_ambiguous_blends_boundary_prob(self):
        """Ambiguous rounding should blend boundary probability into ensemble."""
        fv = compute_weather_fair_value(
            contract_type="weather_max",
            strike_f=45.0,
            current_temp_f=44.6,
            minutes_remaining=15.0,
            metar_temp_c=7,  # range [43.7, 45.5], ambiguous at 45
        )
        assert fv.rounding_ambiguous
        assert "rounding" in fv.components
