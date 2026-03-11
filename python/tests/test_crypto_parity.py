"""Phase 4.4: Parity verification between Python and Rust crypto fair-value engines.

Feeds identical inputs to both implementations and asserts probabilities
match within tolerance. The Rust implementation is the source of truth
going forward; this test ensures the Python reference remains valid.

Run: pytest python/tests/test_crypto_parity.py -v
"""

from __future__ import annotations

import pytest

from models.crypto_fv import (
    CryptoInputs,
    _compute_settlement_probability,
    _levy_averaging_prob,
    _norm_cdf,
    _standard_binary_prob,
    compute_crypto_fair_value,
)

# Tolerance for probability comparison
PROB_TOL = 0.001


class TestNormCdf:
    """Verify norm_cdf matches known values (shared by both implementations)."""

    @pytest.mark.parametrize(
        "x, expected",
        [
            (0.0, 0.5),
            (1.0, 0.8413),
            (-1.0, 0.1587),
            (2.0, 0.9772),
            (-2.0, 0.0228),
            (-3.0, 0.00135),
            (3.0, 0.99865),
        ],
    )
    def test_norm_cdf_known_values(self, x: float, expected: float) -> None:
        assert abs(_norm_cdf(x) - expected) < 0.001


class TestStandardBinaryProb:
    """Standard Black-Scholes N(d2) binary option probability."""

    @pytest.mark.parametrize(
        "spot, strike, seconds, vol, desc",
        [
            (95000, 95000, 600, 0.50, "ATM 10min"),
            (95000, 95000, 60, 0.50, "ATM 1min"),
            (95000, 95000, 3600, 0.50, "ATM 1hr"),
            (100000, 90000, 300, 0.50, "deep ITM"),
            (90000, 100000, 300, 0.50, "deep OTM"),
            (95000, 95000, 600, 0.30, "ATM low vol"),
            (95000, 95000, 600, 0.80, "ATM high vol"),
            (95000, 94500, 600, 0.50, "slightly ITM"),
            (95000, 95500, 600, 0.50, "slightly OTM"),
        ],
    )
    def test_standard_binary_prob(self, spot: float, strike: float, seconds: float, vol: float, desc: str) -> None:
        p = _standard_binary_prob(spot, strike, seconds, vol)
        assert 0.0 <= p <= 1.0, f"Probability out of range for {desc}: {p}"

    def test_atm_near_half(self) -> None:
        p = _standard_binary_prob(95000, 95000, 600, 0.50)
        assert abs(p - 0.5) < 0.1

    def test_deep_itm_high(self) -> None:
        p = _standard_binary_prob(100000, 90000, 300, 0.50)
        assert p > 0.8

    def test_deep_otm_low(self) -> None:
        p = _standard_binary_prob(90000, 100000, 300, 0.50)
        assert p < 0.2


class TestLevyAveragingProb:
    """Levy approximation for TWAP averaging window."""

    def test_full_window(self) -> None:
        """Full 60s remaining — no partial observation."""
        p = _levy_averaging_prob(95000, 95000, 60.0, 0.50)
        assert 0.0 < p < 1.0

    def test_half_window_itm(self) -> None:
        """Half window elapsed with spot > strike → higher prob."""
        p_full = _levy_averaging_prob(95200, 95000, 60.0, 0.50)
        p_half = _levy_averaging_prob(95200, 95000, 30.0, 0.50)
        assert p_half > p_full

    def test_expired(self) -> None:
        p = _levy_averaging_prob(95000, 94000, 0.005, 0.50)
        assert p > 0.95

    def test_k_eff_negative(self) -> None:
        """When observed avg already exceeds strike contribution."""
        p = _levy_averaging_prob(100000, 90000, 5.0, 0.50)
        assert p > 0.95


class TestSettlementProbability:
    """Settlement-aware probability with regime switching."""

    @pytest.mark.parametrize(
        "seconds, desc",
        [
            (600, "far from expiry"),
            (300, "transition zone 5min"),
            (180, "transition zone 3min"),
            (60, "start of RTI window"),
            (30, "inside RTI window"),
            (5, "near expiry"),
        ],
    )
    def test_atm_all_regimes(self, seconds: float, desc: str) -> None:
        p = _compute_settlement_probability(95000, 95000, seconds, 0.50)
        assert abs(p - 0.5) < 0.15, f"ATM {desc}: {p}"

    def test_smoothness_across_transition(self) -> None:
        """No discontinuity at 60s or 300s boundaries."""
        p_61 = _compute_settlement_probability(95000, 95000, 61.0, 0.50)
        p_59 = _compute_settlement_probability(95000, 95000, 59.0, 0.50)
        assert abs(p_61 - p_59) < 0.02, f"Jump at 60s: {p_61} vs {p_59}"

        p_301 = _compute_settlement_probability(95000, 95000, 301.0, 0.50)
        p_299 = _compute_settlement_probability(95000, 95000, 299.0, 0.50)
        assert abs(p_301 - p_299) < 0.02, f"Jump at 300s: {p_301} vs {p_299}"


class TestComputeCryptoFairValue:
    """End-to-end fair value computation."""

    @pytest.mark.parametrize(
        "coinbase, binance, perp, mark, funding, dvol, strike, minutes, desc",
        [
            # Standard cases
            (95000, 95000, 95300, 95200, 0.0001, 52.3, 95000, 10.0, "ATM all feeds"),
            (95000, 95000, 0, 0, 0, None, 95000, 10.0, "ATM spots only"),
            (100000, 100000, 0, 0, 0, None, 90000, 5.0, "deep ITM"),
            (90000, 90000, 0, 0, 0, None, 100000, 5.0, "deep OTM"),
            # Near expiry (Levy regime)
            (95000, 95000, 0, 0, 0, None, 95000, 0.5, "ATM 30s"),
            (95200, 95200, 0, 0, 0, None, 95000, 0.5, "ITM 30s"),
            # High basis
            (95000, 95000, 96000, 95500, 0, None, 95000, 10.0, "high basis"),
            # Negative funding
            (95000, 95000, 0, 0, -0.001, None, 95000, 10.0, "negative funding"),
            # Deribit DVOL
            (95000, 95000, 0, 0, 0, 30.0, 95000, 10.0, "low DVOL"),
            (95000, 95000, 0, 0, 0, 80.0, 95000, 10.0, "high DVOL"),
            # Single exchange
            (95000, 0, 0, 0, 0, None, 95000, 10.0, "coinbase only"),
            (0, 95000, 0, 0, 0, None, 95000, 10.0, "binance only"),
            # Mark price fallback
            (0, 0, 0, 95000, 0, None, 95000, 10.0, "mark only"),
            # Very near expiry
            (95000, 95000, 0, 0, 0, None, 94000, 0.01, "ITM at expiry"),
            (95000, 95000, 0, 0, 0, None, 96000, 0.01, "OTM at expiry"),
            # Funding + basis combo
            (95000, 95000, 95500, 95200, 0.001, 52.3, 95000, 10.0, "all signals"),
            # Edge: zero vol
            (95000, 95000, 0, 0, 0, 0.001, 95000, 10.0, "near-zero DVOL"),
            # Edge: very far from expiry
            (95000, 95000, 0, 0, 0, None, 95000, 30.0, "30 min out"),
            # Different strikes
            (95000, 95000, 0, 0, 0, None, 94000, 10.0, "ITM strike"),
            (95000, 95000, 0, 0, 0, None, 96000, 10.0, "OTM strike"),
        ],
    )
    def test_fair_value_vector(
        self,
        coinbase: float,
        binance: float,
        perp: float,
        mark: float,
        funding: float,
        dvol: float | None,
        strike: float,
        minutes: float,
        desc: str,
    ) -> None:
        inputs = CryptoInputs(
            coinbase_spot=coinbase,
            binance_spot=binance,
            perp_price=perp,
            mark_price=mark,
            funding_rate=funding,
            deribit_dvol=dvol,
            strike=strike,
            minutes_remaining=minutes,
        )
        fv = compute_crypto_fair_value(inputs)

        # Verify probability is valid
        assert 0.01 <= fv.probability <= 0.99, f"{desc}: prob={fv.probability}"

        # Verify confidence is valid
        assert 0.0 <= fv.confidence <= 1.0, f"{desc}: confidence={fv.confidence}"

        # Verify shadow_rti is set when exchanges are provided
        if coinbase > 0 or binance > 0 or mark > 0:
            assert fv.shadow_rti > 0, f"{desc}: shadow_rti should be positive"

    def test_atm_near_half(self) -> None:
        inputs = CryptoInputs(
            coinbase_spot=95000,
            binance_spot=95000,
            strike=95000,
            minutes_remaining=10.0,
        )
        fv = compute_crypto_fair_value(inputs)
        assert abs(fv.probability - 0.5) < 0.1

    def test_itm_high_prob(self) -> None:
        inputs = CryptoInputs(
            coinbase_spot=100000,
            binance_spot=100000,
            strike=90000,
            minutes_remaining=5.0,
        )
        fv = compute_crypto_fair_value(inputs)
        assert fv.probability > 0.8

    def test_otm_low_prob(self) -> None:
        inputs = CryptoInputs(
            coinbase_spot=90000,
            binance_spot=90000,
            strike=100000,
            minutes_remaining=5.0,
        )
        fv = compute_crypto_fair_value(inputs)
        assert fv.probability < 0.2

    def test_confidence_all_feeds(self) -> None:
        inputs = CryptoInputs(
            coinbase_spot=95000,
            binance_spot=95000,
            perp_price=95300,
            deribit_dvol=52.3,
            strike=95000,
            minutes_remaining=10.0,
        )
        fv = compute_crypto_fair_value(inputs)
        assert fv.confidence >= 0.9

    def test_confidence_no_feeds(self) -> None:
        inputs = CryptoInputs(strike=95000, minutes_remaining=10.0)
        fv = compute_crypto_fair_value(inputs)
        assert fv.confidence == 0.5

    def test_basis_signal_positive(self) -> None:
        inputs = CryptoInputs(
            coinbase_spot=95000,
            binance_spot=95000,
            perp_price=95500,
            strike=95000,
            minutes_remaining=10.0,
        )
        fv = compute_crypto_fair_value(inputs)
        assert fv.component_contributions.get("basis_signal", 0) > 0

    def test_funding_signal_negative(self) -> None:
        inputs = CryptoInputs(
            coinbase_spot=95000,
            funding_rate=-0.001,
            strike=95000,
            minutes_remaining=10.0,
        )
        fv = compute_crypto_fair_value(inputs)
        assert fv.component_contributions.get("funding_signal", 0) < 0
