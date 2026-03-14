"""Tests for Black-Scholes binary option model."""

from models.binary_option import (
    PROB_CEILING,
    compress_tail_probability,
    compute_binary_probability,
    compute_binary_put_probability,
)


class TestBinaryProbability:
    def test_at_strike_near_50_percent(self):
        p = compute_binary_probability(spot=65000.0, strike=65000.0, minutes_remaining=10.0, sigma_annual=0.60)
        assert abs(p - 0.5) < 0.05

    def test_above_strike_high_prob(self):
        p = compute_binary_probability(spot=65100.0, strike=65000.0, minutes_remaining=10.0, sigma_annual=0.60)
        assert p > 0.50

    def test_well_below_strike_low_prob(self):
        p = compute_binary_probability(spot=64000.0, strike=65000.0, minutes_remaining=5.0, sigma_annual=0.40)
        assert p < 0.25  # compressed floor zone

    def test_zero_time_above(self):
        assert compute_binary_probability(66000.0, 65000.0, 0.0, 0.60) == 1.0

    def test_zero_time_below(self):
        assert compute_binary_probability(64000.0, 65000.0, 0.0, 0.60) == 0.0

    def test_negative_time(self):
        assert compute_binary_probability(66000.0, 65000.0, -1.0, 0.60) == 1.0

    def test_zero_vol_above(self):
        assert compute_binary_probability(66000.0, 65000.0, 10.0, 0.0) == 1.0

    def test_zero_vol_below(self):
        assert compute_binary_probability(64000.0, 65000.0, 10.0, 0.0) == 0.0

    def test_higher_vol_more_uncertainty(self):
        p_low = compute_binary_probability(64500.0, 65000.0, 10.0, 0.30)
        p_high = compute_binary_probability(64500.0, 65000.0, 10.0, 0.80)
        # Higher vol when below strike → more chance to reach it
        assert p_high > p_low

    def test_more_time_more_uncertainty(self):
        p_short = compute_binary_probability(64500.0, 65000.0, 2.0, 0.60)
        p_long = compute_binary_probability(64500.0, 65000.0, 15.0, 0.60)
        assert p_long > p_short

    def test_invalid_spot_zero(self):
        assert compute_binary_probability(0.0, 65000.0, 10.0, 0.60) == 0.0

    def test_probability_in_range(self):
        for spot in [60000, 65000, 70000]:
            for mins in [1, 5, 15]:
                p = compute_binary_probability(float(spot), 65000.0, float(mins), 0.60)
                assert 0.0 <= p <= 1.0


class TestCompressTailProbability:
    def test_identity_zone(self):
        """Values in [0.25, 0.75] should pass through unchanged."""
        for p in [0.25, 0.40, 0.50, 0.60, 0.75]:
            assert abs(compress_tail_probability(p) - p) < 1e-10

    def test_high_tail_compression(self):
        # 0.90 → 0.75 + (0.90 - 0.75) * 0.20 = 0.78
        assert abs(compress_tail_probability(0.90) - 0.78) < 1e-10
        # 0.95 → 0.75 + (0.95 - 0.75) * 0.20 = 0.79
        assert abs(compress_tail_probability(0.95) - 0.79) < 1e-10

    def test_low_tail_compression(self):
        # 0.10 → 0.25 - (0.25 - 0.10) * 0.20 = 0.22
        assert abs(compress_tail_probability(0.10) - 0.22) < 1e-10
        # 0.05 → 0.25 - (0.25 - 0.05) * 0.20 = 0.21
        assert abs(compress_tail_probability(0.05) - 0.21) < 1e-10

    def test_monotonicity(self):
        values = [0.05, 0.10, 0.20, 0.30, 0.50, 0.70, 0.80, 0.90, 0.95]
        compressed = [compress_tail_probability(p) for p in values]
        for i in range(1, len(compressed)):
            assert compressed[i] > compressed[i - 1]

    def test_boundary_continuity(self):
        at_ceil = compress_tail_probability(0.75)
        just_above = compress_tail_probability(0.75 + 1e-9)
        assert abs(at_ceil - just_above) < 1e-6

        at_floor = compress_tail_probability(0.25)
        just_below = compress_tail_probability(0.25 - 1e-9)
        assert abs(at_floor - just_below) < 1e-6


class TestProbCeiling:
    def test_ceiling_is_080(self):
        """Phase 14: PROB_CEILING should be 0.80."""
        assert PROB_CEILING == 0.80

    def test_deep_itm_capped(self):
        """Deep ITM should be capped at PROB_CEILING (0.80)."""
        p = compute_binary_probability(spot=70000.0, strike=60000.0, minutes_remaining=10.0, sigma_annual=0.60)
        assert p <= PROB_CEILING + 1e-10


class TestBinaryPutProbability:
    def test_put_complement(self):
        p_call = compute_binary_probability(65100.0, 65000.0, 10.0, 0.60)
        p_put = compute_binary_put_probability(65100.0, 65000.0, 10.0, 0.60)
        assert abs(p_call + p_put - 1.0) < 1e-10
