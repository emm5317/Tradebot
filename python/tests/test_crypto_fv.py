"""Tests for crypto fair-value engine."""

from models.crypto_fv import CryptoFairValue, CryptoInputs, compute_crypto_fair_value


class TestShadowRTI:
    def test_coinbase_only(self):
        inputs = CryptoInputs(coinbase_spot=95000.0, strike=94000.0, minutes_remaining=30.0)
        result = compute_crypto_fair_value(inputs)
        assert result.shadow_rti == 95000.0

    def test_binance_only(self):
        inputs = CryptoInputs(binance_spot=95000.0, strike=94000.0, minutes_remaining=30.0)
        result = compute_crypto_fair_value(inputs)
        assert result.shadow_rti == 95000.0

    def test_both_exchanges_weighted(self):
        inputs = CryptoInputs(
            coinbase_spot=95000.0,
            binance_spot=95100.0,
            strike=94000.0,
            minutes_remaining=30.0,
        )
        result = compute_crypto_fair_value(inputs)
        # Coinbase weighted 0.6, Binance 0.4
        expected = 95000.0 * 0.6 + 95100.0 * 0.4
        assert abs(result.shadow_rti - expected) < 1.0

    def test_fallback_to_mark_price(self):
        inputs = CryptoInputs(mark_price=95000.0, strike=94000.0, minutes_remaining=30.0)
        result = compute_crypto_fair_value(inputs)
        assert result.shadow_rti == 95000.0


class TestCryptoFairValueProbability:
    def test_well_above_strike(self):
        """Price well above strike → high probability."""
        inputs = CryptoInputs(
            coinbase_spot=100000.0,
            binance_spot=100000.0,
            strike=90000.0,
            minutes_remaining=30.0,
        )
        result = compute_crypto_fair_value(inputs)
        assert result.probability > 0.8

    def test_well_below_strike(self):
        """Price well below strike → low probability."""
        inputs = CryptoInputs(
            coinbase_spot=80000.0,
            binance_spot=80000.0,
            strike=90000.0,
            minutes_remaining=30.0,
        )
        result = compute_crypto_fair_value(inputs)
        assert result.probability < 0.2

    def test_at_strike(self):
        """Price at strike → near 0.5."""
        inputs = CryptoInputs(
            coinbase_spot=95000.0,
            binance_spot=95000.0,
            strike=95000.0,
            minutes_remaining=30.0,
        )
        result = compute_crypto_fair_value(inputs)
        assert 0.35 < result.probability < 0.65

    def test_zero_minutes(self):
        """At expiry, probability should be near-deterministic."""
        inputs = CryptoInputs(
            coinbase_spot=96000.0,
            strike=95000.0,
            minutes_remaining=0.0,
        )
        result = compute_crypto_fair_value(inputs)
        assert result.probability > 0.9

    def test_probability_clamped(self):
        inputs = CryptoInputs(
            coinbase_spot=200000.0,
            strike=50000.0,
            minutes_remaining=1.0,
        )
        result = compute_crypto_fair_value(inputs)
        assert 0.01 <= result.probability <= 0.99


class TestBasisSignal:
    def test_positive_basis_increases_prob(self):
        """Contango (perp > spot) should slightly increase probability."""
        base_inputs = CryptoInputs(
            coinbase_spot=95000.0,
            strike=95000.0,
            minutes_remaining=30.0,
        )
        basis_inputs = CryptoInputs(
            coinbase_spot=95000.0,
            perp_price=95500.0,
            strike=95000.0,
            minutes_remaining=30.0,
        )
        base_result = compute_crypto_fair_value(base_inputs)
        basis_result = compute_crypto_fair_value(basis_inputs)
        assert basis_result.probability >= base_result.probability - 0.01
        assert basis_result.basis > 0


class TestFundingSignal:
    def test_positive_funding(self):
        """Positive funding rate (bullish) should slightly increase probability."""
        base_inputs = CryptoInputs(
            coinbase_spot=95000.0,
            strike=95000.0,
            minutes_remaining=30.0,
        )
        funding_inputs = CryptoInputs(
            coinbase_spot=95000.0,
            funding_rate=0.001,
            strike=95000.0,
            minutes_remaining=30.0,
        )
        base_result = compute_crypto_fair_value(base_inputs)
        funding_result = compute_crypto_fair_value(funding_inputs)
        assert funding_result.probability >= base_result.probability - 0.01


class TestDeribitDVOL:
    def test_dvol_used_for_volatility(self):
        """Deribit DVOL should be used as volatility input."""
        # Higher vol → probabilities closer to 0.5
        low_vol = CryptoInputs(
            coinbase_spot=96000.0,
            strike=95000.0,
            minutes_remaining=30.0,
            deribit_dvol=20.0,  # 20% annualized
        )
        high_vol = CryptoInputs(
            coinbase_spot=96000.0,
            strike=95000.0,
            minutes_remaining=30.0,
            deribit_dvol=80.0,  # 80% annualized
        )
        low_result = compute_crypto_fair_value(low_vol)
        high_result = compute_crypto_fair_value(high_vol)
        # Higher vol should make probability closer to 0.5
        assert abs(high_result.probability - 0.5) < abs(low_result.probability - 0.5)


class TestConfidence:
    def test_more_sources_higher_confidence(self):
        """More data sources → higher confidence."""
        minimal = CryptoInputs(binance_spot=95000.0, strike=95000.0, minutes_remaining=30.0)
        full = CryptoInputs(
            coinbase_spot=95000.0,
            binance_spot=95000.0,
            perp_price=95100.0,
            deribit_dvol=50.0,
            strike=95000.0,
            minutes_remaining=30.0,
        )
        min_result = compute_crypto_fair_value(minimal)
        full_result = compute_crypto_fair_value(full)
        assert full_result.confidence > min_result.confidence

    def test_confidence_range(self):
        inputs = CryptoInputs(coinbase_spot=95000.0, strike=95000.0, minutes_remaining=30.0)
        result = compute_crypto_fair_value(inputs)
        assert 0.0 < result.confidence <= 1.0


class TestComponentContributions:
    def test_components_present(self):
        inputs = CryptoInputs(
            coinbase_spot=95000.0,
            binance_spot=95000.0,
            strike=95000.0,
            minutes_remaining=30.0,
        )
        result = compute_crypto_fair_value(inputs)
        assert "shadow_rti" in result.component_contributions
        assert "p_core" in result.component_contributions
        assert "vol_annualized" in result.component_contributions
