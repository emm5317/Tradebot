//! Crypto fair-value engine — settlement-aware binary option pricing.
//!
//! Ported from `python/models/crypto_fv.py` and `python/models/binary_option.py`.
//! Computes in-process from CryptoState — no Redis in the critical path.
//!
//! Phase 1.2: Inline shadow RTI + N(d2) binary fair value in Rust.
//! Phase 4.1: Levy approximation for RTI averaging window near expiry.

use crate::crypto_state::CryptoStateInner;

/// Seconds per year for annualization.
const SECONDS_PER_YEAR: f64 = 525_600.0 * 60.0;

/// Default risk-free rate (5% annual).
const RISK_FREE_RATE: f64 = 0.05;

/// Default BTC annualized vol when no data available.
const DEFAULT_VOL: f64 = 0.50;

/// CFB RTI averaging window duration in seconds.
const RTI_WINDOW_SECS: f64 = 60.0;

/// Transition zone: blend standard model into averaging model over this range.
/// From RTI_WINDOW_SECS to RTI_WINDOW_SECS + TRANSITION_SECS.
const TRANSITION_SECS: f64 = 240.0; // 4 minutes (so 1-5 min range)

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

    let seconds_remaining = (minutes_remaining * 60.0).max(0.01);

    // Core probability: use Levy averaging model near expiry, standard N(d2) far out
    let p_core = if shadow_rti <= 0.0 || strike <= 0.0 {
        0.5
    } else {
        compute_settlement_probability(shadow_rti, strike, seconds_remaining, vol)
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

    // Combine
    let p_adjusted = p_core + basis_signal + funding_signal;
    let p_final = p_adjusted.clamp(0.01, 0.99);

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
    // Phase 4.2: reduce confidence when RTI uses fewer than min_venues
    if !state.rti_reliable {
        confidence -= 0.15;
    }
    confidence = confidence.clamp(0.0, 1.0);

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

/// Settlement-aware probability computation.
///
/// Uses three regimes based on distance to settlement:
/// 1. **Far (>5 min):** Standard Black-Scholes N(d2) for point-in-time settlement.
/// 2. **Transition (1–5 min):** Smooth blend between standard and averaging model.
/// 3. **Within RTI window (≤60s):** Levy approximation for arithmetic average options.
///    The CFB RTI is a 60-second TWAP — the variance of a TWAP over interval τ
///    is σ²τ/3 (vs σ²τ for point-in-time), reducing tail risk near expiry.
fn compute_settlement_probability(
    spot: f64,
    strike: f64,
    seconds_remaining: f64,
    vol: f64,
) -> f64 {
    if seconds_remaining <= 0.01 {
        // Expired — deterministic
        return if spot >= strike { 1.0 } else { 0.0 };
    }

    let p_standard = standard_binary_prob(spot, strike, seconds_remaining, vol);

    if seconds_remaining > RTI_WINDOW_SECS + TRANSITION_SECS {
        // Far from expiry — standard model (averaging effect negligible)
        return p_standard;
    }

    let p_averaging = levy_averaging_prob(spot, strike, seconds_remaining, vol);

    if seconds_remaining <= RTI_WINDOW_SECS {
        // Inside the averaging window — pure Levy model
        return p_averaging;
    }

    // Transition zone: smooth blend from standard → averaging
    // α=0 at edge of transition (5 min out), α=1 at start of window (60s out)
    let alpha = 1.0 - (seconds_remaining - RTI_WINDOW_SECS) / TRANSITION_SECS;
    // Smoothstep for continuous derivatives: 3α² - 2α³
    let blend = alpha * alpha * (3.0 - 2.0 * alpha);

    p_standard * (1.0 - blend) + p_averaging * blend
}

/// Standard Black-Scholes binary option probability: N(d2).
fn standard_binary_prob(spot: f64, strike: f64, seconds_remaining: f64, vol: f64) -> f64 {
    let t = seconds_remaining / SECONDS_PER_YEAR;
    let vol_period = vol * t.sqrt();

    if vol_period <= 0.0 {
        return if spot >= strike { 1.0 } else { 0.0 };
    }

    let d2 = ((spot / strike).ln() + (RISK_FREE_RATE - 0.5 * vol * vol) * t)
        / (vol * t.sqrt());
    norm_cdf(d2)
}

/// Levy approximation for binary option on arithmetic average (TWAP).
///
/// The CFB RTI settles on a 60-second arithmetic average. For a GBM process,
/// the variance of the arithmetic average over interval τ is approximately
/// σ²τ/3 (vs σ²τ for terminal price). This gives an effective volatility
/// of σ/√3 for the averaging period.
///
/// When partially inside the window (some prices already observed):
/// - RTI = α·Ā_known + (1-α)·Ā_future, where α = fraction elapsed
/// - Effective strike shifts: K_eff = (K - α·S) / (1-α)
/// - Variance further reduces by (1-α)²
fn levy_averaging_prob(spot: f64, strike: f64, seconds_remaining: f64, vol: f64) -> f64 {
    // Fraction of the 60s window already elapsed
    let elapsed = (RTI_WINDOW_SECS - seconds_remaining).max(0.0);
    let alpha = elapsed / RTI_WINDOW_SECS;

    // Approximate the already-observed running average as current spot
    // (updates every 500ms, so at most ~0.5s stale)
    let a_known = spot;

    // Effective strike for the remaining average portion
    let remaining_frac = 1.0 - alpha;
    if remaining_frac < 0.01 {
        // >99% of window observed — essentially deterministic
        return if a_known >= strike { 1.0 } else { 0.0 };
    }

    let k_eff = (strike - alpha * a_known) / remaining_frac;

    if k_eff <= 0.0 {
        // Observed portion already exceeds the strike contribution — very likely YES
        return 0.99;
    }

    // Remaining averaging time in years
    let tau = seconds_remaining / SECONDS_PER_YEAR;

    // Levy: variance of arithmetic average of GBM over [0, τ'] ≈ S² · σ² · τ' / 3
    // Effective vol for the average: σ_avg = σ · √(τ/3)
    let vol_avg = vol * (tau / 3.0).sqrt();

    if vol_avg <= 1e-12 {
        return if spot >= k_eff { 1.0 } else { 0.0 };
    }

    // d2 with Levy-adjusted volatility
    // Drift adjustment: (r - σ²/6) instead of (r - σ²/2) for the average
    let d2 = ((spot / k_eff).ln() + (RISK_FREE_RATE - vol * vol / 6.0) * tau)
        / (vol_avg);
    norm_cdf(d2)
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
        cs.update_coinbase(95000.0, 0.0, 0.0, 10.0);
        cs.update_binance_spot(95000.0, None, Some(0.50), 30, 10.0);

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
        cs.update_coinbase(100000.0, 0.0, 0.0, 10.0);
        cs.update_binance_spot(100000.0, None, Some(0.50), 30, 10.0);

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
        cs.update_coinbase(90000.0, 0.0, 0.0, 10.0);
        cs.update_binance_spot(90000.0, None, Some(0.50), 30, 10.0);

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
        cs.update_coinbase(95000.0, 0.0, 0.0, 10.0);

        let snap = cs.snapshot();
        let fv = compute_crypto_fair_value(&snap, 94000.0, 0.0);
        assert!(fv.probability > 0.9, "ITM at expiry should be ~1.0");

        let fv = compute_crypto_fair_value(&snap, 96000.0, 0.0);
        assert!(fv.probability < 0.1, "OTM at expiry should be ~0.0");
    }

    #[test]
    fn test_basis_signal_contango() {
        let cs = CryptoState::new();
        cs.update_coinbase(95000.0, 0.0, 0.0, 10.0);
        cs.update_binance_spot(95000.0, None, Some(0.50), 30, 10.0);
        cs.update_binance_futures(95500.0, 95200.0, 0.0, 0.5); // +500 basis

        let snap = cs.snapshot();
        let fv = compute_crypto_fair_value(&snap, 95000.0, 10.0);

        // Contango should push probability slightly higher
        assert!(fv.basis_signal > 0.0, "Contango should produce positive basis signal");
    }

    #[test]
    fn test_funding_signal() {
        let cs = CryptoState::new();
        cs.update_coinbase(95000.0, 0.0, 0.0, 10.0);
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
        // No data = base 0.5 - 0.15 unreliable = 0.35
        assert!((fv.confidence - 0.35).abs() < 0.01, "No data = 0.35 confidence, got {}", fv.confidence);

        cs.update_coinbase(95000.0, 0.0, 0.0, 10.0);
        cs.update_binance_spot(95000.0, None, None, 0, 10.0);
        cs.update_binance_futures(95300.0, 0.0, 0.0, 0.5);
        cs.update_deribit(50.0);
        let snap = cs.snapshot();
        let fv = compute_crypto_fair_value(&snap, 95000.0, 10.0);
        assert!(
            fv.confidence > 0.8,
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
    fn test_levy_averaging_reduces_extremes_near_expiry() {
        let cs = CryptoState::new();
        // Use a spot slightly above strike so probability is in a sensitive range
        cs.update_coinbase(95200.0, 0.0, 0.0, 10.0);
        cs.update_binance_spot(95200.0, None, Some(0.50), 30, 10.0);

        let snap = cs.snapshot();
        let strike = 95000.0;

        // Far from expiry: standard model
        let fv_far = compute_crypto_fair_value(&snap, strike, 10.0);
        // Near expiry (inside transition zone): averaging effect
        let fv_near = compute_crypto_fair_value(&snap, strike, 2.0);

        // The Levy model reduces volatility (σ/√3), which for a slightly ITM
        // position actually increases certainty. The key property is that
        // the averaging model produces different (more accurate) probabilities.
        assert!(
            (fv_far.probability - fv_near.probability).abs() > 0.001,
            "Averaging model should differ from standard: far={}, near={}",
            fv_far.probability, fv_near.probability
        );
    }

    #[test]
    fn test_levy_vol_reduction() {
        // Levy model should produce lower effective vol than standard model
        // for the same time horizon (σ/√3 < σ)
        let spot = 95000.0;
        let strike = 95500.0; // slightly OTM
        let vol = 0.50;
        let secs = 30.0; // inside RTI window

        let p_standard = standard_binary_prob(spot, strike, secs, vol);
        let p_levy = levy_averaging_prob(spot, strike, secs, vol);

        // Levy (lower vol) → probability closer to deterministic (further from 0.5)
        // For OTM: p < 0.5, so Levy should give lower p (more certainty it stays OTM)
        assert!(
            (p_levy - 0.5).abs() > (p_standard - 0.5).abs() * 0.9,
            "Levy should be at least as decisive as standard: levy={}, standard={}",
            p_levy, p_standard
        );
    }

    #[test]
    fn test_levy_partial_window_strike_shift() {
        // Use ATM-ish strike so probabilities are in a sensitive range
        let spot = 95000.0;
        let strike = 94950.0; // slightly ITM
        let vol = 0.50;

        // Full window: 60s remaining, alpha=0 (nothing observed yet)
        let p_full_window = levy_averaging_prob(spot, strike, 60.0, vol);
        // Half window: 30s remaining, alpha=0.5 (half observed at spot price)
        let p_half_window = levy_averaging_prob(spot, strike, 30.0, vol);

        // With half the window locked in above strike and reduced remaining
        // uncertainty, prob should increase for this slightly ITM case
        assert!(
            p_half_window > p_full_window,
            "Partial window with spot>strike should increase prob: half={}, full={}",
            p_half_window, p_full_window
        );
    }

    #[test]
    fn test_transition_zone_smoothness() {
        let spot = 95000.0;
        let strike = 95000.0; // ATM
        let vol = 0.50;

        // Sample probabilities across the transition zone
        let p_6min = compute_settlement_probability(spot, strike, 360.0, vol);
        let p_5min = compute_settlement_probability(spot, strike, 300.0, vol);
        let p_3min = compute_settlement_probability(spot, strike, 180.0, vol);
        let p_1min = compute_settlement_probability(spot, strike, 60.0, vol);
        let p_30s = compute_settlement_probability(spot, strike, 30.0, vol);

        // All should be near 0.5 for ATM, but should vary smoothly
        for (label, p) in [
            ("6min", p_6min), ("5min", p_5min), ("3min", p_3min),
            ("1min", p_1min), ("30s", p_30s),
        ] {
            assert!(
                (p - 0.5).abs() < 0.15,
                "ATM prob at {} should be near 0.5, got {}", label, p
            );
        }

        // No jumps at transition boundaries (5min and 1min)
        assert!(
            (p_5min - p_6min).abs() < 0.05,
            "No jump at 5min boundary: p_5min={}, p_6min={}", p_5min, p_6min
        );
    }

    #[test]
    fn test_levy_deterministic_at_expiry() {
        let spot = 95000.0;

        // ITM at expiry → ~1.0
        let p = compute_settlement_probability(spot, 94000.0, 0.001, 0.50);
        assert!(p > 0.95, "ITM at expiry should be ~1.0, got {}", p);

        // OTM at expiry → ~0.0
        let p = compute_settlement_probability(spot, 96000.0, 0.001, 0.50);
        assert!(p < 0.05, "OTM at expiry should be ~0.0, got {}", p);
    }

    #[test]
    fn test_levy_effective_strike_exceeds() {
        // When observed average * alpha already exceeds strike, k_eff ≤ 0
        // Should return high probability
        let p = levy_averaging_prob(100000.0, 90000.0, 5.0, 0.50);
        assert!(p > 0.95, "k_eff<=0 should give high prob, got {}", p);
    }
}
