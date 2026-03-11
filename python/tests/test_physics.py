"""Tests for weather physics and ensemble model."""

from models.physics import (
    climatological_probability,
    compute_ensemble_probability,
    compute_weather_probability,
    fast_norm_cdf,
    trend_extrapolation_probability,
)


class TestFastNormCdf:
    def test_at_zero(self):
        assert abs(fast_norm_cdf(0.0) - 0.5) < 1e-10

    def test_large_positive(self):
        assert fast_norm_cdf(5.0) > 0.999

    def test_large_negative(self):
        assert fast_norm_cdf(-5.0) < 0.001

    def test_symmetry(self):
        assert abs(fast_norm_cdf(1.0) + fast_norm_cdf(-1.0) - 1.0) < 1e-10

    def test_known_values(self):
        # N(1.0) ≈ 0.8413
        assert abs(fast_norm_cdf(1.0) - 0.8413) < 0.001
        # N(-1.96) ≈ 0.025
        assert abs(fast_norm_cdf(-1.96) - 0.025) < 0.001


class TestWeatherProbability:
    def test_at_threshold_gives_50_percent(self):
        # At threshold with any time remaining → P ≈ 0.50
        p = compute_weather_probability(70.0, 70.0, 12.0)
        assert abs(p - 0.5) < 0.01

    def test_well_below_threshold_low_prob(self):
        # 6°F below, 12 min → very low probability
        p = compute_weather_probability(64.0, 70.0, 12.0)
        assert p < 0.05

    def test_slightly_below_threshold(self):
        # 1°F below, 12 min, default σ=0.3/√10min → σ_total ≈ 0.33°F
        # z = 1/0.33 ≈ 3.0 → very low P with tight default sigma
        # Use σ=1.0 for a more intuitive "moderate" range
        p = compute_weather_probability(69.0, 70.0, 12.0, sigma_per_10min=1.0)
        assert 0.15 < p < 0.45

    def test_slightly_above_threshold(self):
        # 1°F above, 12 min, σ=1.0 → moderate-high probability
        p = compute_weather_probability(71.0, 70.0, 12.0, sigma_per_10min=1.0)
        assert 0.55 < p < 0.85

    def test_well_above_threshold_high_prob(self):
        # 6°F above, 12 min → very high probability
        p = compute_weather_probability(76.0, 70.0, 12.0)
        assert p > 0.95

    def test_zero_time_above(self):
        assert compute_weather_probability(71.0, 70.0, 0.0) == 1.0

    def test_zero_time_below(self):
        assert compute_weather_probability(69.0, 70.0, 0.0) == 0.0

    def test_negative_time(self):
        assert compute_weather_probability(71.0, 70.0, -5.0) == 1.0

    def test_higher_sigma_wider_distribution(self):
        p_low = compute_weather_probability(68.0, 70.0, 12.0, sigma_per_10min=0.2)
        p_high = compute_weather_probability(68.0, 70.0, 12.0, sigma_per_10min=0.5)
        # Higher sigma → more uncertainty → probability closer to 0.5
        assert p_high > p_low

    def test_more_time_more_uncertainty(self):
        p_short = compute_weather_probability(68.0, 70.0, 5.0)
        p_long = compute_weather_probability(68.0, 70.0, 30.0)
        # Below threshold: more time → more chance to reach it
        assert p_long > p_short


class TestClimatologicalProbability:
    def test_no_table_returns_0_5(self):
        p = climatological_probability("KORD", 14, 7, 90.0, 85.0, None)
        assert p == 0.5

    def test_missing_key_returns_0_5(self):
        table = {("KJFK", 12, 6): (75.0, 5.0)}
        p = climatological_probability("KORD", 14, 7, 90.0, 85.0, table)
        assert p == 0.5

    def test_with_table_data(self):
        table = {("KORD", 14, 7): (88.0, 4.0)}
        p = climatological_probability("KORD", 14, 7, 90.0, 85.0, table)
        # Blended temp = 0.7*85 + 0.3*88 = 86, threshold=90, sigma=4
        # z = (90-86)/4 = 1.0, P ≈ 0.16
        assert 0.1 < p < 0.25


class TestTrendExtrapolation:
    def test_insufficient_data(self):
        p = trend_extrapolation_probability([70.0, 71.0], 72.0, 10.0)
        assert p == 0.5

    def test_rising_trend_below_threshold(self):
        # Steadily rising temps: extrapolation pushes toward threshold
        temps = [60.0 + i * 0.5 for i in range(30)]  # 60 → 74.5
        p_trend = trend_extrapolation_probability(temps, 80.0, 10.0)
        p_flat = trend_extrapolation_probability([74.5] * 30, 80.0, 10.0)
        # Rising trend should give higher probability than flat
        assert p_trend > p_flat

    def test_flat_trend_well_below(self):
        temps = [65.0] * 30
        p = trend_extrapolation_probability(temps, 80.0, 10.0)
        assert p < 0.05  # well below, flat trend

    def test_flat_trend_at_threshold(self):
        temps = [70.0] * 30
        p = trend_extrapolation_probability(temps, 70.0, 10.0)
        assert 0.4 < p < 0.6  # near 0.5


class TestEnsembleProbability:
    def test_returns_four_values(self):
        result = compute_ensemble_probability(
            current_temp_f=70.0,
            threshold_f=70.0,
            minutes_remaining=12.0,
            station="KORD",
            hour=14,
            month=7,
        )
        assert len(result) == 4
        p_ensemble, p_physics, p_climo, p_trend = result

    def test_at_threshold_near_0_5(self):
        p_ensemble, _, _, _ = compute_ensemble_probability(
            current_temp_f=70.0,
            threshold_f=70.0,
            minutes_remaining=12.0,
            station="KORD",
            hour=14,
            month=7,
        )
        assert abs(p_ensemble - 0.5) < 0.05

    def test_ensemble_between_components(self):
        p_ensemble, p_physics, p_climo, p_trend = compute_ensemble_probability(
            current_temp_f=72.0,
            threshold_f=70.0,
            minutes_remaining=12.0,
            station="KORD",
            hour=14,
            month=7,
        )
        components = [p_physics, p_climo, p_trend]
        assert min(components) <= p_ensemble <= max(components) + 0.01

    def test_custom_weights(self):
        # All weight on physics
        p_phys_only, p_physics, _, _ = compute_ensemble_probability(
            current_temp_f=72.0,
            threshold_f=70.0,
            minutes_remaining=12.0,
            station="KORD",
            hour=14,
            month=7,
            weights=(1.0, 0.0, 0.0),
        )
        assert abs(p_phys_only - p_physics) < 1e-10

    def test_with_sigma_table(self):
        sigma_table = {("KORD", 14, 7): 0.5}
        p_high_sigma, _, _, _ = compute_ensemble_probability(
            current_temp_f=68.0,
            threshold_f=70.0,
            minutes_remaining=12.0,
            station="KORD",
            hour=14,
            month=7,
            sigma_table=sigma_table,
        )
        p_default, _, _, _ = compute_ensemble_probability(
            current_temp_f=68.0,
            threshold_f=70.0,
            minutes_remaining=12.0,
            station="KORD",
            hour=14,
            month=7,
        )
        # Higher sigma should give probability closer to 0.5
        assert abs(p_high_sigma - 0.5) < abs(p_default - 0.5)

    def test_clamped_to_valid_range(self):
        p, _, _, _ = compute_ensemble_probability(
            current_temp_f=100.0,
            threshold_f=50.0,
            minutes_remaining=1.0,
            station="KORD",
            hour=14,
            month=7,
        )
        assert 0.0 <= p <= 1.0
