//! Crypto fair-value engine — settlement-aware binary option pricing.
//!
//! Ported from `python/models/crypto_fv.py` and `python/models/binary_option.py`.
//! Computes in-process from CryptoState — no Redis in the critical path.
//!
//! Phase 1.2: Inline shadow RTI + N(d2) binary fair value in Rust.
//! Phase 4.1: Levy approximation for RTI averaging window near expiry.

use crate::crypto_asset::CryptoAsset;
use crate::crypto_state::CryptoStateInner;

/// Seconds per year for annualization.
const SECONDS_PER_YEAR: f64 = 525_600.0 * 60.0;

/// Default risk-free rate (5% annual).
const RISK_FREE_RATE: f64 = 0.05;

/// Per-asset tuning constants for the fair-value model.
#[derive(Debug, Clone)]
pub struct AssetConfig {
    pub default_vol: f64,
    pub binary_vol_multiplier: f64,
    pub excess_kurtosis: f64,
    pub prob_floor: f64,
    pub prob_ceiling: f64,
}

impl AssetConfig {
    /// Return tuned configuration for a specific crypto asset.
    pub fn for_asset(asset: CryptoAsset) -> Self {
        match asset {
            CryptoAsset::BTC => Self {
                default_vol: 0.50,
                binary_vol_multiplier: 2.0,
                excess_kurtosis: 7.0,
                prob_floor: 0.03,
                prob_ceiling: 0.95,
            },
            CryptoAsset::ETH => Self {
                default_vol: 0.60,
                binary_vol_multiplier: 2.8,
                excess_kurtosis: 8.0,
                prob_floor: 0.03,
                prob_ceiling: 0.95,
            },
            CryptoAsset::SOL => Self {
                default_vol: 0.90,
                binary_vol_multiplier: 3.0,
                excess_kurtosis: 10.0,
                prob_floor: 0.03,
                prob_ceiling: 0.95,
            },
            CryptoAsset::XRP => Self {
                default_vol: 0.80,
                binary_vol_multiplier: 3.0,
                excess_kurtosis: 9.0,
                prob_floor: 0.03,
                prob_ceiling: 0.95,
            },
            CryptoAsset::DOGE => Self {
                default_vol: 1.00,
                binary_vol_multiplier: 3.2,
                excess_kurtosis: 12.0,
                prob_floor: 0.03,
                prob_ceiling: 0.95,
            },
        }
    }

    /// Return tuned configuration with config-driven overrides for vol multiplier and prob ceiling.
    pub fn for_asset_with_overrides(asset: CryptoAsset, vol_mult_override: f64, prob_ceiling_override: f64) -> Self {
        let mut config = Self::for_asset(asset);
        // For BTC, apply the override directly; for alts, scale proportionally
        let base_btc_mult = 2.0;
        let ratio = vol_mult_override / base_btc_mult;
        config.binary_vol_multiplier *= ratio;
        config.prob_ceiling = prob_ceiling_override;
        config
    }
}

/// Legacy constants for backward compat (BTC defaults).
const DEFAULT_VOL: f64 = 0.50;
const PROB_FLOOR: f64 = 0.03;
const PROB_CEILING: f64 = 0.95;
const EXCESS_KURTOSIS: f64 = 7.0;
const BINARY_VOL_MULTIPLIER: f64 = 2.0;

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
    compute_crypto_fair_value_with_config(state, strike, minutes_remaining, &AssetConfig::for_asset(CryptoAsset::BTC))
}

/// Asset-aware version of `compute_crypto_fair_value`.
pub fn compute_crypto_fair_value_with_config(
    state: &CryptoStateInner,
    strike: f64,
    minutes_remaining: f64,
    asset_config: &AssetConfig,
) -> CryptoFairValue {
    let shadow_rti = state.shadow_rti;
    let base_vol = estimate_volatility(state, asset_config.default_vol);
    let vol = base_vol * asset_config.binary_vol_multiplier;

    let seconds_remaining = (minutes_remaining * 60.0).max(0.01);

    // Core probability: use Levy averaging model near expiry, standard N(d2) far out
    let p_core = if shadow_rti <= 0.0 || strike <= 0.0 {
        0.5
    } else {
        compute_settlement_probability_with_config(shadow_rti, strike, seconds_remaining, vol, asset_config)
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
    // Use tanh mapping for smooth gradient across typical range (-0.05% to +0.05%).
    // tanh(rate * 4000) gives: 0.01% → 0.38, 0.03% → 0.84, 0.05% → 0.96
    // Scaled to ±0.03 max adjustment.
    let funding_signal = if state.funding_rate != 0.0 {
        (state.funding_rate * 4000.0).tanh() * 0.03
    } else {
        0.0
    };

    // Combine
    let p_adjusted = p_core + basis_signal + funding_signal;
    let p_final = p_adjusted.clamp(asset_config.prob_floor, asset_config.prob_ceiling);

    // Confidence — base 0.40, with additive bonuses per feed.
    // Single healthy spot venue is sufficient to trade (0.40 + 0.15 = 0.55 > MIN_CONFIDENCE).
    let mut confidence: f64 = 0.40;
    if state.coinbase_spot > 0.0 {
        confidence += 0.15;
    }
    if state.binance_spot > 0.0 {
        confidence += 0.15;
    }
    if state.dvol > 0.0 {
        confidence += 0.10;
    }
    if state.perp_price > 0.0 {
        confidence += 0.10;
    }
    // Bonus for multi-venue agreement (RTI reliable)
    if state.rti_reliable {
        confidence += 0.10;
    }
    confidence = confidence.clamp(0.0, 1.0);

    CryptoFairValue {
        probability: p_final,
        shadow_rti,
        vol_used: vol, // includes BINARY_VOL_MULTIPLIER
        basis,
        basis_signal,
        funding_signal,
        confidence,
    }
}

/// Compute fair value for directional "Will BTC go up?" contracts.
///
/// Unlike strike-based contracts where N(d2) provides meaningful probabilities,
/// directional contracts are ATM by definition (strike = current price), making
/// N(d2) ≈ 0.50 always — zero predictive power. Instead, use a momentum/order-flow
/// model that aggregates microstructure signals into a directional probability.
///
/// Returns probability in [0.35, 0.65] — deliberately constrained since short-term
/// directional prediction is inherently low-conviction.
pub fn compute_directional_fair_value(
    state: &CryptoStateInner,
    _minutes_remaining: f64,
    price_momentum: f64,
    trade_imbalance: f64,
    vwap_deviation: f64,
    depth_imbalance: f64,
    volume_surge_aligned: bool,
) -> CryptoFairValue {
    compute_directional_fair_value_with_config(
        state, _minutes_remaining, price_momentum, trade_imbalance,
        vwap_deviation, depth_imbalance, volume_surge_aligned,
        &AssetConfig::for_asset(CryptoAsset::BTC),
    )
}

/// Asset-aware version of `compute_directional_fair_value`.
pub fn compute_directional_fair_value_with_config(
    state: &CryptoStateInner,
    _minutes_remaining: f64,
    price_momentum: f64,
    trade_imbalance: f64,
    vwap_deviation: f64,
    depth_imbalance: f64,
    volume_surge_aligned: bool,
    asset_config: &AssetConfig,
) -> CryptoFairValue {
    let vol = estimate_volatility(state, asset_config.default_vol);

    // Weights for signal combination
    const W_MOMENTUM: f64 = 0.40;
    const W_TRADE_IMB: f64 = 0.25;
    const W_VWAP: f64 = 0.15;
    const W_DEPTH: f64 = 0.10;
    const W_VOL_SURGE: f64 = 0.10;
    const MAX_SHIFT: f64 = 0.15;

    // Normalize price momentum to [-1, 1] using 60s scale
    let momentum_norm = price_momentum.clamp(-1.0, 1.0);

    // Combine signals
    let vol_surge_signal = if volume_surge_aligned { 1.0 } else { 0.0 };
    let momentum_score = W_MOMENTUM * momentum_norm
        + W_TRADE_IMB * trade_imbalance.clamp(-1.0, 1.0)
        + W_VWAP * vwap_deviation.clamp(-1.0, 1.0)
        + W_DEPTH * ((depth_imbalance - 0.5) * 2.0).clamp(-1.0, 1.0)
        + W_VOL_SURGE * vol_surge_signal;

    // Scale by inverse volatility — higher vol = larger moves plausible
    let vol_scale = (0.50 / vol).clamp(0.5, 2.0);

    let p = (0.50 + momentum_score * vol_scale * MAX_SHIFT).clamp(0.35, 0.65);

    // Confidence: much lower than strike-based contracts
    let mut confidence: f64 = 0.25;
    // Bonus for aligned signals (momentum and trade imbalance same direction)
    if momentum_norm.signum() == trade_imbalance.signum() && momentum_norm.abs() > 0.1 {
        confidence += 0.05;
    }
    // Bonus for RTI reliability
    if state.rti_reliable {
        confidence += 0.05;
    }
    // Bonus for volume surge
    if volume_surge_aligned {
        confidence += 0.05;
    }
    // Bonus for VWAP alignment with momentum
    if vwap_deviation.signum() == momentum_norm.signum() && vwap_deviation.abs() > 0.1 {
        confidence += 0.05;
    }
    confidence = confidence.clamp(0.0, 0.45);

    CryptoFairValue {
        probability: p,
        shadow_rti: state.shadow_rti,
        vol_used: vol,
        basis: state.basis,
        basis_signal: 0.0,
        funding_signal: 0.0,
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
    if spread > 0.12 {
        effective *= 0.90;
    }
    effective
}

/// Kelly criterion for binary outcome.
pub fn compute_kelly(model_prob: f64, fill_price: f64, direction: &str) -> f64 {
    compute_kelly_with_bounds(model_prob, fill_price, direction, 0.02, 0.98)
}

/// Kelly criterion with configurable fill price bounds.
pub fn compute_kelly_with_bounds(model_prob: f64, fill_price: f64, direction: &str, fill_min: f64, fill_max: f64) -> f64 {
    // Refuse to size trades at extreme prices (terrible risk/reward)
    if !(fill_min..=fill_max).contains(&fill_price) {
        return 0.0;
    }

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
    let raw = if direction == "yes" {
        mid_price + spread / 2.0
    } else {
        mid_price - spread / 2.0
    };
    raw.clamp(0.01, 0.99)
}

/// Estimate volatility from available sources.
/// Priority: DVOL > EWMA > realized > default.
fn estimate_volatility(state: &CryptoStateInner, default_vol: f64) -> f64 {
    state.best_vol.unwrap_or(default_vol)
}

/// Settlement-aware probability computation (legacy BTC wrapper).
fn compute_settlement_probability(spot: f64, strike: f64, seconds_remaining: f64, vol: f64) -> f64 {
    compute_settlement_probability_with_config(spot, strike, seconds_remaining, vol, &AssetConfig::for_asset(CryptoAsset::BTC))
}

/// Settlement-aware probability computation with per-asset config.
///
/// Uses three regimes based on distance to settlement:
/// 1. **Far (>5 min):** Standard Black-Scholes N(d2) for point-in-time settlement.
/// 2. **Transition (1–5 min):** Smooth blend between standard and averaging model.
/// 3. **Within RTI window (≤60s):** Levy approximation for arithmetic average options.
///    The CFB RTI is a 60-second TWAP — the variance of a TWAP over interval τ
///    is σ²τ/3 (vs σ²τ for point-in-time), reducing tail risk near expiry.
fn compute_settlement_probability_with_config(spot: f64, strike: f64, seconds_remaining: f64, vol: f64, asset_config: &AssetConfig) -> f64 {
    if seconds_remaining <= 0.01 {
        // Expired — deterministic
        return if spot >= strike { 1.0 } else { 0.0 };
    }

    let p_standard = standard_binary_prob_with_config(spot, strike, seconds_remaining, vol, asset_config);

    if seconds_remaining > RTI_WINDOW_SECS + TRANSITION_SECS {
        // Far from expiry — standard model (averaging effect negligible)
        return p_standard;
    }

    let p_averaging = levy_averaging_prob_with_config(spot, strike, seconds_remaining, vol, asset_config);

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

/// Standard Black-Scholes binary option probability: N(d2) (legacy BTC wrapper).
fn standard_binary_prob(spot: f64, strike: f64, seconds_remaining: f64, vol: f64) -> f64 {
    standard_binary_prob_with_config(spot, strike, seconds_remaining, vol, &AssetConfig::for_asset(CryptoAsset::BTC))
}

/// Standard Black-Scholes binary option probability: N(d2) with per-asset config.
fn standard_binary_prob_with_config(spot: f64, strike: f64, seconds_remaining: f64, vol: f64, asset_config: &AssetConfig) -> f64 {
    let t = seconds_remaining / SECONDS_PER_YEAR;
    let vol_period = vol * t.sqrt();

    if vol_period <= 0.0 {
        return if spot >= strike { 1.0 } else { 0.0 };
    }

    let d2 = ((spot / strike).ln() + (RISK_FREE_RATE - 0.5 * vol * vol) * t) / (vol * t.sqrt());
    let p = norm_cdf(d2);
    // Far-OTM kurtosis correction: GBM underestimates tail probability
    let z_score = (spot / strike).ln().abs() / vol_period;
    let p = if z_score > 2.0 {
        let tail_adj = (asset_config.excess_kurtosis / z_score.powi(4)) * 0.01;
        p.max(tail_adj).min(1.0 - tail_adj)
    } else {
        p
    };
    p.clamp(asset_config.prob_floor, asset_config.prob_ceiling)
}

/// Levy approximation for binary option on arithmetic average (legacy BTC wrapper).
fn levy_averaging_prob(spot: f64, strike: f64, seconds_remaining: f64, vol: f64) -> f64 {
    levy_averaging_prob_with_config(spot, strike, seconds_remaining, vol, &AssetConfig::for_asset(CryptoAsset::BTC))
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
fn levy_averaging_prob_with_config(spot: f64, strike: f64, seconds_remaining: f64, vol: f64, asset_config: &AssetConfig) -> f64 {
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
    let d2 = ((spot / k_eff).ln() + (RISK_FREE_RATE - vol * vol / 6.0) * tau) / (vol_avg);
    norm_cdf(d2).clamp(asset_config.prob_floor, asset_config.prob_ceiling)
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

    if sign < 0.0 { 2.0 - result } else { result }
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
        assert!(fv.probability >= PROB_CEILING, "ITM at expiry should be at ceiling, got {}", fv.probability);

        let fv = compute_crypto_fair_value(&snap, 96000.0, 0.0);
        assert!(fv.probability <= PROB_FLOOR, "OTM at expiry should be at floor, got {}", fv.probability);
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
        assert!(
            fv.basis_signal > 0.0,
            "Contango should produce positive basis signal"
        );
    }

    #[test]
    fn test_funding_signal() {
        let cs = CryptoState::new();
        cs.update_coinbase(95000.0, 0.0, 0.0, 10.0);
        cs.update_binance_futures(0.0, 0.0, 0.001, 0.5); // positive funding

        let snap = cs.snapshot();
        let fv = compute_crypto_fair_value(&snap, 95000.0, 10.0);

        assert!(
            fv.funding_signal > 0.0,
            "Positive funding should produce positive signal"
        );
        // tanh mapping: 0.001 * 4000 = 4.0, tanh(4.0) ≈ 0.9993 → 0.03 * 0.9993 ≈ 0.030
        assert!(
            fv.funding_signal < 0.031,
            "Funding signal should be <= 0.03, got {}",
            fv.funding_signal
        );
    }

    #[test]
    fn test_funding_signal_gradient() {
        // Small funding rates should produce proportionally smaller signals (not saturated)
        let cs1 = CryptoState::new();
        cs1.update_coinbase(95000.0, 0.0, 0.0, 10.0);
        cs1.update_binance_futures(0.0, 0.0, 0.00005, 0.5); // small funding

        let cs2 = CryptoState::new();
        cs2.update_coinbase(95000.0, 0.0, 0.0, 10.0);
        cs2.update_binance_futures(0.0, 0.0, 0.0003, 0.5); // moderate funding

        let fv1 = compute_crypto_fair_value(&cs1.snapshot(), 95000.0, 10.0);
        let fv2 = compute_crypto_fair_value(&cs2.snapshot(), 95000.0, 10.0);

        // tanh preserves gradient: moderate should be meaningfully larger than small
        assert!(
            fv2.funding_signal > fv1.funding_signal * 2.0,
            "Funding signal should show gradient: small={}, moderate={}",
            fv1.funding_signal,
            fv2.funding_signal
        );
    }

    #[test]
    fn test_confidence_scoring() {
        let cs = CryptoState::new();
        let snap = cs.snapshot();
        let fv = compute_crypto_fair_value(&snap, 95000.0, 10.0);
        // No data = base 0.40, no feeds, no RTI reliable bonus = 0.40
        assert!(
            (fv.confidence - 0.40).abs() < 0.01,
            "No data = 0.40 confidence, got {}",
            fv.confidence
        );

        cs.update_coinbase(95000.0, 0.0, 0.0, 10.0);
        cs.update_binance_spot(95000.0, None, None, 0, 10.0);
        cs.update_binance_futures(95300.0, 0.0, 0.0, 0.5);
        cs.update_deribit(50.0);
        let snap = cs.snapshot();
        let fv = compute_crypto_fair_value(&snap, 95000.0, 10.0);
        // 0.40 + 0.15(CB) + 0.15(BN) + 0.10(DVOL) + 0.10(perp) + 0.10(reliable) = 1.0
        assert!(
            fv.confidence > 0.9,
            "All feeds = high confidence, got {}",
            fv.confidence
        );
    }

    #[test]
    fn test_confidence_single_venue_passes() {
        // Single Binance venue should produce confidence > 0.5 (MIN_CONFIDENCE)
        let cs = CryptoState::new();
        cs.update_binance_spot(95000.0, None, Some(0.50), 30, 10.0);
        let snap = cs.snapshot();
        let fv = compute_crypto_fair_value(&snap, 95000.0, 10.0);
        // 0.40 + 0.15(BN) = 0.55 (RTI not reliable with single venue)
        assert!(
            fv.confidence >= 0.50,
            "Single Binance venue should pass MIN_CONFIDENCE, got {}",
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

        // Spread at old threshold (0.12) — no penalty with new threshold >0.12
        let edge = compute_effective_edge(0.10, 0.12);
        // (0.10 - 0.06) = 0.04, no penalty since spread is not > 0.12
        assert!((edge - 0.04).abs() < 1e-6);

        // Wide spread penalty (> 0.12)
        let edge = compute_effective_edge(0.10, 0.14);
        // (0.10 - 0.07) * 0.90 = 0.027
        assert!((edge - 0.027).abs() < 1e-6);
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
            fv_far.probability,
            fv_near.probability
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
            p_levy,
            p_standard
        );
    }

    #[test]
    fn test_levy_partial_window_strike_shift() {
        // Use ATM-ish strike so probabilities are in a sensitive range
        // With vol multiplier, use very small moneyness to stay within [FLOOR, CEILING]
        let spot = 95000.0;
        let strike = 94999.0; // barely ITM ($1)
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
            p_half_window,
            p_full_window
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
            ("6min", p_6min),
            ("5min", p_5min),
            ("3min", p_3min),
            ("1min", p_1min),
            ("30s", p_30s),
        ] {
            assert!(
                (p - 0.5).abs() < 0.15,
                "ATM prob at {} should be near 0.5, got {}",
                label,
                p
            );
        }

        // No jumps at transition boundaries (5min and 1min)
        assert!(
            (p_5min - p_6min).abs() < 0.05,
            "No jump at 5min boundary: p_5min={}, p_6min={}",
            p_5min,
            p_6min
        );
    }

    #[test]
    fn test_levy_deterministic_at_expiry() {
        let spot = 95000.0;

        // ITM at expiry → capped at PROB_CEILING
        let p = compute_settlement_probability(spot, 94000.0, 0.001, 0.50);
        assert!(p >= PROB_CEILING, "ITM at expiry should be at ceiling, got {}", p);

        // OTM at expiry → floored at PROB_FLOOR
        let p = compute_settlement_probability(spot, 96000.0, 0.001, 0.50);
        assert!(p <= PROB_FLOOR, "OTM at expiry should be at floor, got {}", p);
    }

    // --- Directional model tests ---

    #[test]
    fn test_directional_base_neutral() {
        // Zero inputs → P = 0.50
        let cs = CryptoState::new();
        cs.update_coinbase(95000.0, 0.0, 0.0, 10.0);
        cs.update_binance_spot(95000.0, None, Some(0.50), 30, 10.0);
        let snap = cs.snapshot();

        let fv = compute_directional_fair_value(&snap, 10.0, 0.0, 0.0, 0.0, 0.5, false);
        assert!(
            (fv.probability - 0.50).abs() < 0.01,
            "Zero inputs should give P ≈ 0.50, got {}",
            fv.probability
        );
    }

    #[test]
    fn test_directional_bullish_momentum() {
        let cs = CryptoState::new();
        cs.update_coinbase(95000.0, 0.0, 0.0, 10.0);
        cs.update_binance_spot(95000.0, None, Some(0.50), 30, 10.0);
        let snap = cs.snapshot();

        let fv = compute_directional_fair_value(&snap, 10.0, 0.8, 0.6, 0.5, 0.7, true);
        assert!(
            fv.probability > 0.55,
            "Strong bullish signals should give P > 0.55, got {}",
            fv.probability
        );
    }

    #[test]
    fn test_directional_bearish_momentum() {
        let cs = CryptoState::new();
        cs.update_coinbase(95000.0, 0.0, 0.0, 10.0);
        cs.update_binance_spot(95000.0, None, Some(0.50), 30, 10.0);
        let snap = cs.snapshot();

        let fv = compute_directional_fair_value(&snap, 10.0, -0.8, -0.6, -0.5, 0.3, false);
        assert!(
            fv.probability < 0.45,
            "Strong bearish signals should give P < 0.45, got {}",
            fv.probability
        );
    }

    #[test]
    fn test_directional_clamped_range() {
        let cs = CryptoState::new();
        cs.update_coinbase(95000.0, 0.0, 0.0, 10.0);
        cs.update_binance_spot(95000.0, None, Some(0.50), 30, 10.0);
        let snap = cs.snapshot();

        // Extreme bullish
        let fv = compute_directional_fair_value(&snap, 10.0, 10.0, 10.0, 10.0, 1.0, true);
        assert!(
            fv.probability >= 0.35 && fv.probability <= 0.65,
            "P should be in [0.35, 0.65], got {}",
            fv.probability
        );

        // Extreme bearish
        let fv = compute_directional_fair_value(&snap, 10.0, -10.0, -10.0, -10.0, 0.0, false);
        assert!(
            fv.probability >= 0.35 && fv.probability <= 0.65,
            "P should be in [0.35, 0.65], got {}",
            fv.probability
        );
    }

    #[test]
    fn test_directional_confidence_low() {
        // No alignment → confidence should be low (below typical min_confidence of 0.50)
        let cs = CryptoState::new();
        cs.update_coinbase(95000.0, 0.0, 0.0, 10.0);
        let snap = cs.snapshot();

        let fv = compute_directional_fair_value(&snap, 10.0, 0.0, 0.0, 0.0, 0.5, false);
        assert!(
            fv.confidence < 0.50,
            "No aligned signals → confidence < 0.50, got {}",
            fv.confidence
        );
    }

    #[test]
    fn test_directional_confidence_aligned() {
        // Aligned signals → confidence should be higher (but still ≤0.45)
        let cs = CryptoState::new();
        cs.update_coinbase(95000.0, 0.0, 0.0, 10.0);
        cs.update_binance_spot(95000.0, None, Some(0.50), 30, 10.0);
        let snap = cs.snapshot();

        let fv = compute_directional_fair_value(&snap, 10.0, 0.5, 0.5, 0.5, 0.7, true);
        assert!(
            fv.confidence >= 0.35 && fv.confidence <= 0.45,
            "Aligned signals → confidence in [0.35, 0.45], got {}",
            fv.confidence
        );
    }

    #[test]
    fn test_levy_effective_strike_exceeds() {
        // When observed average * alpha already exceeds strike, k_eff ≤ 0
        // Should return high probability (capped at PROB_CEILING by levy clamp)
        let p = levy_averaging_prob(100000.0, 90000.0, 5.0, 0.50);
        assert!(p >= PROB_CEILING, "k_eff<=0 should give high prob, got {}", p);
    }

    #[test]
    fn test_kelly_extreme_fill_price_rejected() {
        // Extreme fill prices should return 0 Kelly (refuse to size)
        assert_eq!(
            compute_kelly(0.60, 0.99, "yes"),
            0.0,
            "fill_price=0.99 should be rejected"
        );
        assert_eq!(
            compute_kelly(0.40, 0.01, "no"),
            0.0,
            "fill_price=0.01 should be rejected"
        );
        // At the boundary (0.02/0.98) — should be accepted (inclusive bounds)
        assert!(
            compute_kelly(0.60, 0.98, "yes") >= 0.0,
            "fill_price=0.98 should be accepted at boundary"
        );
        assert!(
            compute_kelly(0.40, 0.02, "no") >= 0.0,
            "fill_price=0.02 should be accepted at boundary"
        );
        // Just inside bounds should work
        assert!(
            compute_kelly(0.60, 0.50, "yes") > 0.0,
            "fill_price=0.50 should be accepted"
        );
    }

    #[test]
    fn test_kelly_with_custom_bounds() {
        // Custom tight bounds [0.05, 0.95]
        assert_eq!(
            compute_kelly_with_bounds(0.60, 0.96, "yes", 0.05, 0.95),
            0.0,
            "fill_price=0.96 should be rejected with max=0.95"
        );
        assert_eq!(
            compute_kelly_with_bounds(0.40, 0.04, "no", 0.05, 0.95),
            0.0,
            "fill_price=0.04 should be rejected with min=0.05"
        );
        // Inside custom bounds
        assert!(
            compute_kelly_with_bounds(0.70, 0.50, "yes", 0.05, 0.95) > 0.0,
            "fill_price=0.50 should work with custom bounds"
        );
    }

    #[test]
    fn test_fill_price_clamped() {
        // Extreme spread/mid combos should be clamped to [0.01, 0.99]
        let fp = estimate_fill_price("yes", 0.95, 0.20);
        assert!(
            fp <= 0.99,
            "fill_price should be clamped to 0.99, got {}",
            fp
        );

        let fp = estimate_fill_price("no", 0.05, 0.20);
        assert!(
            fp >= 0.01,
            "fill_price should be clamped to 0.01, got {}",
            fp
        );

        // Normal case still works
        let fp = estimate_fill_price("yes", 0.50, 0.04);
        assert!(
            (fp - 0.52).abs() < 1e-6,
            "normal case: expected 0.52, got {}",
            fp
        );
    }

    #[test]
    fn test_risk_reward_ratio() {
        // Paying $0.95 to win $0.05 → lose_payout=0.95, win_payout=0.05 → 19:1 ratio
        // With new bounds [0.02, 0.98], this is accepted by fill guard but terrible risk/reward
        // The evaluator's risk/reward guard (config-driven, default 5.0) catches this
        let k = compute_kelly(0.60, 0.95, "yes");
        // win_prob=0.60, win=0.05, lose=0.95 → kelly = (0.60*0.05 - 0.40*0.95)/0.05 = (0.03 - 0.38)/0.05 < 0 → 0
        assert_eq!(k, 0.0, "terrible risk/reward should give kelly=0");

        // Fill price 0.80, yes direction: win=0.20, lose=0.80 → 4:1 exactly
        let k = compute_kelly(0.80, 0.80, "yes");
        // win_prob=0.80, win=0.20, lose=0.80 → kelly = (0.80*0.20 - 0.20*0.80)/0.20 = 0
        assert!(k.abs() < 1e-6, "edge=0 at price=prob should give kelly≈0");
    }

    #[test]
    fn test_deep_itm_bracket_capped() {
        // BTC $100K, strike $59K, 60 min — should be capped at PROB_CEILING
        let cs = CryptoState::new();
        cs.update_coinbase(100000.0, 0.0, 0.0, 10.0);
        cs.update_binance_spot(100000.0, None, Some(0.50), 30, 10.0);

        let snap = cs.snapshot();
        let fv = compute_crypto_fair_value(&snap, 59000.0, 60.0);

        assert!(
            fv.probability <= PROB_CEILING,
            "Deep ITM bracket should be capped at {}, got {}",
            PROB_CEILING,
            fv.probability
        );
    }

    #[test]
    fn test_deep_otm_bracket_floored() {
        // BTC $59K, strike $100K, 60 min — should be floored at PROB_FLOOR
        let cs = CryptoState::new();
        cs.update_coinbase(59000.0, 0.0, 0.0, 10.0);
        cs.update_binance_spot(59000.0, None, Some(0.50), 30, 10.0);

        let snap = cs.snapshot();
        let fv = compute_crypto_fair_value(&snap, 100000.0, 60.0);

        assert!(
            fv.probability >= PROB_FLOOR,
            "Deep OTM bracket should be floored at {}, got {}",
            PROB_FLOOR,
            fv.probability
        );
    }

    #[test]
    fn test_vol_multiplier_calibration() {
        // With 2.0x vol multiplier, probabilities should be spread across a reasonable range
        // instead of clustering at 0.998
        let cs = CryptoState::new();
        cs.update_coinbase(95000.0, 0.0, 0.0, 10.0);
        cs.update_binance_spot(95000.0, None, Some(0.50), 30, 10.0);
        let snap = cs.snapshot();

        // $500 ITM (strike 94500) at 30 min — with 2.0x multiplier, ~0.65-0.80
        let fv = compute_crypto_fair_value(&snap, 94500.0, 30.0);
        assert!(
            fv.probability > 0.55 && fv.probability < 0.90,
            "$500 ITM should be moderate prob, got {}",
            fv.probability
        );

        // $1000 ITM (strike 94000) at 30 min — should be ~0.80-0.95
        let fv = compute_crypto_fair_value(&snap, 94000.0, 30.0);
        assert!(
            fv.probability > 0.70 && fv.probability <= PROB_CEILING,
            "$1K ITM should be high-moderate prob, got {}",
            fv.probability
        );

        // $500 OTM (strike 95500) at 30 min — should be ~0.20-0.45
        let fv = compute_crypto_fair_value(&snap, 95500.0, 30.0);
        assert!(
            fv.probability > 0.10 && fv.probability < 0.50,
            "$500 OTM should be moderate-low prob, got {}",
            fv.probability
        );

        // ATM — should still be ~0.50
        let fv = compute_crypto_fair_value(&snap, 95000.0, 30.0);
        assert!(
            (fv.probability - 0.50).abs() < 0.10,
            "ATM should be near 0.50, got {}",
            fv.probability
        );
    }

    #[test]
    fn test_asset_config_per_asset_values() {
        let btc = AssetConfig::for_asset(CryptoAsset::BTC);
        let eth = AssetConfig::for_asset(CryptoAsset::ETH);
        let sol = AssetConfig::for_asset(CryptoAsset::SOL);

        // BTC has lowest vol
        assert!(btc.default_vol < eth.default_vol);
        assert!(eth.default_vol < sol.default_vol);

        // Higher vol multipliers for alts
        assert!(btc.binary_vol_multiplier < eth.binary_vol_multiplier);
        assert!(eth.binary_vol_multiplier <= sol.binary_vol_multiplier);

        // DOGE has highest kurtosis
        let doge = AssetConfig::for_asset(CryptoAsset::DOGE);
        assert!(doge.excess_kurtosis > btc.excess_kurtosis);
    }

    #[test]
    fn test_fv_with_config_eth_differs_from_btc() {
        let cs = CryptoState::new();
        cs.update_coinbase(3500.0, 0.0, 0.0, 10.0);
        cs.update_binance_spot(3500.0, None, Some(0.60), 30, 10.0);
        let snap = cs.snapshot();

        let btc_config = AssetConfig::for_asset(CryptoAsset::BTC);
        let eth_config = AssetConfig::for_asset(CryptoAsset::ETH);

        // Use ATM-ish strike so probability is in a sensitive range (not clamped at ceiling)
        let fv_btc = compute_crypto_fair_value_with_config(&snap, 3480.0, 30.0, &btc_config);
        let fv_eth = compute_crypto_fair_value_with_config(&snap, 3480.0, 30.0, &eth_config);

        // ETH has higher vol multiplier → more uncertainty → prob closer to 0.5
        assert!(
            (fv_btc.probability - fv_eth.probability).abs() > 0.001,
            "ETH and BTC configs should produce different probabilities: btc={}, eth={}",
            fv_btc.probability, fv_eth.probability
        );
    }

    #[test]
    fn test_asset_config_with_overrides() {
        // Default BTC config
        let btc_default = AssetConfig::for_asset(CryptoAsset::BTC);
        assert!((btc_default.binary_vol_multiplier - 2.0).abs() < 1e-6);
        assert!((btc_default.prob_ceiling - 0.95).abs() < 1e-6);

        // Override: bump vol multiplier to 2.5 (old value)
        let btc_overridden = AssetConfig::for_asset_with_overrides(CryptoAsset::BTC, 2.5, 0.90);
        assert!((btc_overridden.binary_vol_multiplier - 2.5).abs() < 1e-6);
        assert!((btc_overridden.prob_ceiling - 0.90).abs() < 1e-6);

        // ETH with override: base ETH mult=2.8, ratio=2.5/2.0=1.25, result=3.5
        let eth_overridden = AssetConfig::for_asset_with_overrides(CryptoAsset::ETH, 2.5, 0.92);
        assert!((eth_overridden.binary_vol_multiplier - 3.5).abs() < 1e-6);
        assert!((eth_overridden.prob_ceiling - 0.92).abs() < 1e-6);
    }

    #[test]
    fn test_effective_edge_negative_spread() {
        // Negative spread should not be passed to compute_effective_edge in practice
        // (evaluator clamps it), but if it were, the result would inflate edge.
        // This test documents the behavior pre-guard.
        let edge_normal = compute_effective_edge(0.10, 0.04);
        let edge_negative = compute_effective_edge(0.10, -0.50);
        // With negative spread, spread_cost = -0.25, effective = 0.10 + 0.25 = 0.35
        assert!(
            edge_negative > edge_normal,
            "negative spread inflates edge (bug this phase fixes)"
        );
        // After the evaluator guard, spread is clamped to 0.10, so:
        let edge_guarded = compute_effective_edge(0.10, 0.10);
        assert!(
            edge_guarded < edge_normal,
            "guarded spread (0.10) correctly reduces edge vs tight spread"
        );
    }
}
