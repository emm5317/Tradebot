"""Tests for Black-Scholes binary option model."""

from models.binary_option import compute_binary_probability, compute_binary_put_probability


class TestBinaryProbability:
    def test_at_strike_near_50_percent(self):
        p = compute_binary_probability(
            spot=65000.0, strike=65000.0, minutes_remaining=10.0, sigma_annual=0.60
        )
        assert abs(p - 0.5) < 0.05

    def test_above_strike_high_prob(self):
        p = compute_binary_probability(
            spot=65100.0, strike=65000.0, minutes_remaining=10.0, sigma_annual=0.60
        )
        assert p > 0.50

    def test_well_below_strike_low_prob(self):
        p = compute_binary_probability(
            spot=64000.0, strike=65000.0, minutes_remaining=5.0, sigma_annual=0.40
        )
        assert p < 0.15

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


class TestBinaryPutProbability:
    def test_put_complement(self):
        p_call = compute_binary_probability(65100.0, 65000.0, 10.0, 0.60)
        p_put = compute_binary_put_probability(65100.0, 65000.0, 10.0, 0.60)
        assert abs(p_call + p_put - 1.0) < 1e-10
