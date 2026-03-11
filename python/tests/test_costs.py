"""Tests for transaction cost modeling (Phase 8.2)."""

from __future__ import annotations

from backtester.costs import FeeModel


class TestFeeModelQuadratic:
    """Quadratic fee model: fee = multiplier × price × (1-price) × count × 100."""

    def test_fee_at_50_cents_is_maximal(self):
        """Quadratic fee is maximized at price=0.50."""
        fm = FeeModel()
        fee_50 = fm.compute_fee(0.50)
        fee_30 = fm.compute_fee(0.30)
        fee_70 = fm.compute_fee(0.70)
        assert fee_50 > fee_30
        assert fee_50 > fee_70

    def test_fee_at_extremes_near_zero(self):
        """Fee near price=0 or price=1 should be very small."""
        fm = FeeModel()
        assert fm.compute_fee(0.01) < 0.1  # < 0.1 cents
        assert fm.compute_fee(0.99) < 0.1

    def test_fee_symmetry(self):
        """fee(p) == fee(1-p) for quadratic model."""
        fm = FeeModel()
        assert abs(fm.compute_fee(0.3) - fm.compute_fee(0.7)) < 1e-10

    def test_default_taker_fee_at_50(self):
        """Default 7% taker at price=0.50: 0.07 * 0.5 * 0.5 * 100 = 1.75 cents."""
        fm = FeeModel()
        fee = fm.compute_fee(0.50)
        assert abs(fee - 1.75) < 1e-10

    def test_maker_fee_lower_than_taker(self):
        """Maker fee should be lower than taker fee at same price."""
        fm_taker = FeeModel(assume_taker=True)
        fm_maker = FeeModel(assume_taker=False)
        assert fm_maker.compute_fee(0.50) < fm_taker.compute_fee(0.50)

    def test_count_multiplier(self):
        """Fee scales linearly with count."""
        fm = FeeModel()
        assert abs(fm.compute_fee(0.50, count=3) - fm.compute_fee(0.50) * 3) < 1e-10

    def test_round_trip_equals_entry_only(self):
        """Round trip = entry fee only (settlement is free on Kalshi)."""
        fm = FeeModel()
        assert fm.round_trip_cost(0.50) == fm.compute_fee(0.50)


class TestFeeModelFlat:
    """Flat fee model."""

    def test_flat_fee(self):
        fm = FeeModel(fee_type="flat", flat_fee_cents=3)
        assert fm.compute_fee(0.50) == 3
        assert fm.compute_fee(0.10) == 3

    def test_flat_fee_count(self):
        fm = FeeModel(fee_type="flat", flat_fee_cents=2)
        assert fm.compute_fee(0.50, count=5) == 10


class TestFeeModelEdgeCases:
    """Edge cases for fee computation."""

    def test_zero_price(self):
        fm = FeeModel()
        assert fm.compute_fee(0.0) == 0.0

    def test_one_price(self):
        fm = FeeModel()
        assert fm.compute_fee(1.0) == 0.0

    def test_no_fees_config(self):
        """Zero-fee config for backward compatibility."""
        fm = FeeModel(fee_type="flat", flat_fee_cents=0)
        assert fm.compute_fee(0.50) == 0
        assert fm.round_trip_cost(0.50) == 0
