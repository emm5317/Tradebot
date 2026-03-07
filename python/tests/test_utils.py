"""Tests for shared trading utilities."""

from signals.types import OrderbookState
from signals.utils import (
    compute_effective_edge,
    compute_kelly,
    determine_direction,
    estimate_fill_price,
)


class TestEstimateFillPrice:
    def test_yes_uses_best_ask(self):
        ob = OrderbookState(mid_price=0.50, spread=0.04, best_ask=0.52)
        assert estimate_fill_price("yes", ob) == 0.52

    def test_no_uses_best_bid(self):
        ob = OrderbookState(mid_price=0.50, spread=0.04, best_bid=0.48)
        assert estimate_fill_price("no", ob) == 0.48

    def test_yes_fallback_to_mid_plus_half_spread(self):
        ob = OrderbookState(mid_price=0.50, spread=0.04)
        assert estimate_fill_price("yes", ob) == 0.52

    def test_no_fallback_to_mid_minus_half_spread(self):
        ob = OrderbookState(mid_price=0.50, spread=0.04)
        assert estimate_fill_price("no", ob) == 0.48


class TestComputeKelly:
    def test_positive_edge_yes(self):
        kelly = compute_kelly(model_prob=0.70, fill_price=0.55, direction="yes")
        assert kelly > 0

    def test_positive_edge_no(self):
        kelly = compute_kelly(model_prob=0.30, fill_price=0.55, direction="no")
        assert kelly > 0

    def test_no_edge_returns_zero(self):
        kelly = compute_kelly(model_prob=0.50, fill_price=0.50, direction="yes")
        assert kelly == 0.0

    def test_negative_edge_clamped_to_zero(self):
        kelly = compute_kelly(model_prob=0.30, fill_price=0.55, direction="yes")
        assert kelly == 0.0

    def test_zero_payout_returns_zero(self):
        kelly = compute_kelly(model_prob=0.70, fill_price=1.0, direction="yes")
        assert kelly == 0.0


class TestComputeEffectiveEdge:
    def test_subtracts_half_spread(self):
        edge = compute_effective_edge(raw_edge=0.10, spread=0.04)
        assert abs(edge - 0.08) < 1e-10

    def test_wide_spread_discount(self):
        edge = compute_effective_edge(raw_edge=0.10, spread=0.12)
        # (0.10 - 0.06) * 0.85 = 0.034
        assert abs(edge - 0.034) < 1e-10

    def test_narrow_spread_no_discount(self):
        edge = compute_effective_edge(raw_edge=0.10, spread=0.02)
        assert abs(edge - 0.09) < 1e-10


class TestDetermineDirection:
    def test_model_above_market_is_yes(self):
        direction, raw_edge = determine_direction(0.70, 0.50)
        assert direction == "yes"
        assert abs(raw_edge - 0.20) < 1e-10

    def test_model_below_market_is_no(self):
        direction, raw_edge = determine_direction(0.30, 0.50)
        assert direction == "no"
        assert abs(raw_edge - 0.20) < 1e-10

    def test_equal_is_no(self):
        direction, raw_edge = determine_direction(0.50, 0.50)
        assert direction == "no"
        assert raw_edge == 0.0
