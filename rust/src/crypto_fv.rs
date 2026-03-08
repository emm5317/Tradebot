//! Crypto fair-value engine — settlement-aware binary option pricing.
//!
//! Ported from `python/models/crypto_fv.py` and `python/models/binary_option.py`.
//! Computes in-process from CryptoState — no Redis in the critical path.
//!
//! Phase 1.2: Inline shadow RTI + N(d2) binary fair value in Rust.

use crate::crypto_state::CryptoStateInner;

/// Minutes per year for annualization.
const MINUTES_PER_YEAR: f64 = 525_600.0;

/// Default risk-free rate (5% annual).
const RISK_FREE_RATE: f64 = 0.05;

/// Default BTC annualized vol when no data available.
const DEFAULT_VOL: f64 = 0.50;

/// Result of a crypto fair-value computation.
#[derive(Debug, Clone)]
pub struct CryptoFairValue {
    /// P(contract settles YES)
    pub probability: f64,
    /// Estimated CFB RTI value
    pub shadow_rti: f64,
    /// Annualized volatility used
    pub vol_used: f64,
    /// Perp - spot basis
    pub basis: f64,
    /// Basis signal adjustment
    pub basis_signal: f64,
    /// Funding rate signal adjustment
    pub funding_signal: f64,
    /// Model confidence 0-1
    pub confidence: f64,
}

/// Compute settlement-aware probability for a crypto binary contract.
///
/// The contract settles YES if the CFB Real-Time Index (60-second average
/// from constituent exchanges) is above the strike at expiry.
pub fn compute_crypto_fair_value(
    state: &CryptoStateInner,
    strike: f64,
    minutes_remaining: f64,
) -> CryptoFairValue {
    let shadow_rti = state.shadow_rti;
    let vol = estimate_volatility(state);

    // Time-scaled volatility
    let minutes = minutes_remaining.max(0.01);
    let t = minutes / MINUTES_PER_YEAR;
    let sqrt_t = t.sqrt();
    let vol_period = vol * sqrt_t;

    // Core probability via N(d2) — Black-Scholes binary
    let p_core = if shadow_rti <= 0.0 || strike <= 0.0 {
        0.5
    } else if vol_period <= 0.0 {
        if shadow_rti >= strike {
            1.0
        } else {
            0.0
        }
    } else {
        let d2 = ((shadow_rti / strike).ln() + (RISK_FREE_RATE - 0.5 * vol * vol) * t)
            / (vol * sqrt_t);
        norm_cdf(d2)
    };

    // Basis signal: positive basis (contango) → bullish
    let basis = state.basis;
    let basis_signal = if shadow_rti > 0.0 && basis.abs() > 0.0 {
        let basis_pct = basis / shadow_rti;
        (basis_pct * 4.0).clamp(-0.05, 0.05)
    } else {
        0.0
    };

    // Funding rate signal: positive funding = longs pay = bullish sentiment
    let funding_signal = if state.funding_rate != 0.0 {
        (state.funding_rate * 300.0).clamp(-0.03, 0.03)
    } else {
        0.0
    };

    // RTI averaging window dampening near expiry
    let averaging_dampening = if minutes < 5.0 {
        0.85 + 0.15 * (minutes / 5.0)
    } else {
        1.0
    };

    // Combine
    let p_adjusted = p_core + basis_signal + funding_signal;
    let p_final = 0.5 + (p_adjusted - 0.5) * averaging_dampening;
    let p_final = p_final.clamp(0.01, 0.99);

    // Confidence
    let mut confidence: f64 = 0.5;
    if state.coinbase_spot > 0.0 {
        confidence += 0.15;
    }
    if state.binance_spot > 0.0 {
        confidence += 0.1;
    }
    if state.dvol > 0.0 {
        confidence += 0.1;
    }
    if state.perp_price > 0.0 {
        confidence += 0.1;
    }
    confidence = confidence.min(1.0);

    CryptoFairValue {
        probability: p_final,
        shadow_rti,
        vol_used: vol,
        basis,
        basis_signal,
        funding_signal,
        confidence,
    }
}

/// Determine trade direction and raw edge.
pub fn determine_direction(model_prob: f64, market_price: f64) -> (&'static str, f64) {
    if model_prob > market_price {
        ("yes", model_prob - market_price)
    } else {
        ("no", market_price - model_prob)
    }
}

/// Spread-adjusted effective edge.
pub fn compute_effective_edge(raw_edge: f64, spread: f64) -> f64 {
    let spread_cost = spread / 2.0;
    let mut effective = raw_edge - spread_cost;
    if spread > 0.10 {
        effective *= 0.85;
    }
    effective
}

/// Kelly criterion for binary outcome.
pub fn compute_kelly(model_prob: f64, fill_price: f64, direction: &str) -> f64 {
    let (win_prob, win_payout, lose_payout) = if direction == "yes" {
        (model_prob, 1.0 - fill_price, fill_price)
    } else {
        (1.0 - model_prob, fill_price, 1.0 - fill_price)
    };

    if win_payout <= 0.0 {
        return 0.0;
    }

    let lose_prob = 1.0 - win_prob;
    let kelly = (win_prob * win_payout - lose_prob * lose_payout) / win_payout;
    kelly.max(0.0)
}

/// Estimate fill price from orderbook (simplified).
pub fn estimate_fill_price(direction: &str, mid_price: f64, spread: f64) -> f64 {
    if direction == "yes" {
        mid_price + spread / 2.0
    } else {
        mid_price - spread / 2.0
    }
}

/// Estimate volatility from available sources.
/// Priority: DVOL > EWMA > realized > default.
fn estimate_volatility(state: &CryptoStateInner) -> f64 {
    state.best_vol.unwrap_or(DEFAULT_VOL)
}

/// Standard normal CDF using erfc (matches Python's math.erfc implementation).
fn norm_cdf(x: f64) -> f64 {
    0.5 * erfc(-x / std::f64::consts::SQRT_2)
}

/// Complementary error function using Abramowitz & Stegun rational approximation.
/// Maximum error: 1.5e-7. Zero external dependencies.
fn erfc(x: f64) -> f64 {
    // For negative x: erfc(-x) = 2 - erfc(x)
    let (sign, x) = if x < 0.0 { (-1.0, -x) } else { (1.0, x) };

    let t = 1.0 / (1.0 + 0.3275911 * x);
    let poly = t
        * (0.254829592
            + t * (-0.284496736 + t * (1.421413741 + t * (-1.453152027 + t * 1.061405429))));
    let result = poly * (-x * x).exp();

    if sign < 0.0 {
        2.0 - result
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto_state::CryptoState;

    #[test]
    fn test_norm_cdf_standard_values() {
        // N(0) = 0.5
        assert!((norm_cdf(0.0) - 0.5).abs() < 1e-6);
        // N(1) ≈ 0.8413
        assert!((norm_cdf(1.0) - 0.8413).abs() < 1e-3);
        // N(-1) ≈ 0.1587
        assert!((norm_cdf(-1.0) - 0.1587).abs() < 1e-3);
        // N(2) ≈ 0.9772
        assert!((norm_cdf(2.0) - 0.9772).abs() < 1e-3);
        // N(-3) ≈ 0.0013
        assert!((norm_cdf(-3.0) - 0.00135).abs() < 1e-3);
    }

    #[test]
    fn test_fair_value_at_the_money() {
        let cs = CryptoState::new();
        cs.update_coinbase(95000.0, 0.0, 0.0);
        cs.update_binance_spot(95000.0, None, Some(0.50), 30);

        let snap = cs.snapshot();
        let fv = compute_crypto_fair_value(&snap, 95000.0, 10.0);

        // At-the-money with vol should be close to 0.5
        assert!(
            (fv.probability - 0.5).abs() < 0.1,
            "ATM prob should be near 0.5, got {}",
            fv.probability
        );
    }

    #[test]
    fn test_fair_value_deep_itm() {
        let cs = CryptoState::new();
        cs.update_coinbase(100000.0, 0.0, 0.0);
        cs.update_binance_spot(100000.0, None, Some(0.50), 30);

        let snap = cs.snapshot();
        let fv = compute_crypto_fair_value(&snap, 90000.0, 5.0);

        // Deep in-the-money should be high probability
        assert!(
            fv.probability > 0.8,
            "Deep ITM prob should be >0.8, got {}",
            fv.probability
        );
    }

    #[test]
    fn test_fair_value_deep_otm() {
        let cs = CryptoState::new();
        cs.update_coinbase(90000.0, 0.0, 0.0);
        cs.update_binance_spot(90000.0, None, Some(0.50), 30);

        let snap = cs.snapshot();
        let fv = compute_crypto_fair_value(&snap, 100000.0, 5.0);

        // Deep out-of-the-money should be low probability
        assert!(
            fv.probability < 0.2,
            "Deep OTM prob should be <0.2, got {}",
            fv.probability
        );
    }

    #[test]
    fn test_fair_value_zero_time() {
        let cs = CryptoState::new();
        cs.update_coinbase(95000.0, 0.0, 0.0);

        let snap = cs.snapshot();
        let fv = compute_crypto_fair_value(&snap, 94000.0, 0.0);
        assert!(fv.probability > 0.9, "ITM at expiry should be ~1.0");

        let fv = compute_crypto_fair_value(&snap, 96000.0, 0.0);
        assert!(fv.probability < 0.1, "OTM at expiry should be ~0.0");
    }

    #[test]
    fn test_basis_signal_contango() {
        let cs = CryptoState::new();
        cs.update_coinbase(95000.0, 0.0, 0.0);
        cs.update_binance_spot(95000.0, None, Some(0.50), 30);
        cs.update_binance_futures(95500.0, 95200.0, 0.0, 0.5); // +500 basis

        let snap = cs.snapshot();
        let fv = compute_crypto_fair_value(&snap, 95000.0, 10.0);

        // Contango should push probability slightly higher
        assert!(fv.basis_signal > 0.0, "Contango should produce positive basis signal");
    }

    #[test]
    fn test_funding_signal() {
        let cs = CryptoState::new();
        cs.update_coinbase(95000.0, 0.0, 0.0);
        cs.update_binance_futures(0.0, 0.0, 0.001, 0.5); // positive funding

        let snap = cs.snapshot();
        let fv = compute_crypto_fair_value(&snap, 95000.0, 10.0);

        assert!(fv.funding_signal > 0.0, "Positive funding should produce positive signal");
    }

    #[test]
    fn test_confidence_scoring() {
        let cs = CryptoState::new();
        let snap = cs.snapshot();
        let fv = compute_crypto_fair_value(&snap, 95000.0, 10.0);
        assert!((fv.confidence - 0.5).abs() < 0.01, "No data = 0.5 confidence");

        cs.update_coinbase(95000.0, 0.0, 0.0);
        cs.update_binance_spot(95000.0, None, None, 0);
        cs.update_binance_futures(95300.0, 0.0, 0.0, 0.5);
        cs.update_deribit(50.0);
        let snap = cs.snapshot();
        let fv = compute_crypto_fair_value(&snap, 95000.0, 10.0);
        assert!(
            fv.confidence > 0.9,
            "All feeds = high confidence, got {}",
            fv.confidence
        );
    }

    #[test]
    fn test_determine_direction() {
        let (dir, edge) = determine_direction(0.7, 0.5);
        assert_eq!(dir, "yes");
        assert!((edge - 0.2).abs() < 1e-6);

        let (dir, edge) = determine_direction(0.3, 0.5);
        assert_eq!(dir, "no");
        assert!((edge - 0.2).abs() < 1e-6);
    }

    #[test]
    fn test_effective_edge() {
        // Normal spread
        let edge = compute_effective_edge(0.10, 0.04);
        assert!((edge - 0.08).abs() < 1e-6);

        // Wide spread penalty
        let edge = compute_effective_edge(0.10, 0.12);
        // (0.10 - 0.06) * 0.85 = 0.034
        assert!((edge - 0.034).abs() < 1e-6);
    }

    #[test]
    fn test_kelly_yes_direction() {
        // model_prob=0.7, fill_price=0.55, direction=yes
        // win_prob=0.7, win_payout=0.45, lose_payout=0.55
        // kelly = (0.7*0.45 - 0.3*0.55) / 0.45 = (0.315 - 0.165) / 0.45 = 0.333
        let k = compute_kelly(0.7, 0.55, "yes");
        assert!((k - 0.333).abs() < 0.01);
    }

    #[test]
    fn test_kelly_no_edge() {
        // model_prob=0.5, fill_price=0.5
        let k = compute_kelly(0.5, 0.5, "yes");
        assert!(k.abs() < 1e-6, "No edge = zero Kelly");
    }

    #[test]
    fn test_averaging_dampening_near_expiry() {
        let cs = CryptoState::new();
        cs.update_coinbase(100000.0, 0.0, 0.0);
        cs.update_binance_spot(100000.0, None, Some(0.50), 30);

        let snap = cs.snapshot();

        // Far from expiry: full probability
        let fv_far = compute_crypto_fair_value(&snap, 90000.0, 10.0);
        // Near expiry: dampened toward 0.5
        let fv_near = compute_crypto_fair_value(&snap, 90000.0, 2.0);

        // Near-expiry probability should be closer to 0.5 than far-expiry
        assert!(
            (fv_near.probability - 0.5).abs() < (fv_far.probability - 0.5).abs(),
            "Dampening should pull near-expiry closer to 0.5"
        );
    }
}
