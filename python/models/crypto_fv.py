"""Settlement-aware crypto fair-value engine.

Replaces generic Black-Scholes N(d2) with a model that understands:
1. CFB RTI settlement mechanics (60-second weighted average from constituent exchanges)
2. Shadow RTI estimation from available constituent venues (Coinbase + Binance)
3. Futures basis and funding rate signals
4. Deribit DVOL for implied volatility
5. Kalshi orderbook microstructure signals
"""

from __future__ import annotations

import math
from dataclasses import dataclass, field
from typing import Literal

import structlog

logger = structlog.get_logger()


@dataclass
class CryptoInputs:
    """All inputs needed for crypto fair-value computation."""

    binance_spot: float = 0.0
    coinbase_spot: float = 0.0
    perp_price: float = 0.0
    mark_price: float = 0.0
    funding_rate: float = 0.0
    deribit_dvol: float | None = None
    kalshi_mid: float | None = None
    kalshi_spread: float | None = None
    kalshi_bid_depth: int = 0
    kalshi_ask_depth: int = 0
    strike: float = 0.0
    minutes_remaining: float = 0.0
    realized_vol_30m: float | None = None


@dataclass
class CryptoFairValue:
    """Output of the crypto fair-value engine."""

    probability: float              # P(contract settles YES)
    confidence: float               # 0-1 confidence in estimate
    shadow_rti: float               # estimated RTI value
    basis: float                    # perp - spot basis
    component_contributions: dict[str, float] = field(default_factory=dict)


def compute_crypto_fair_value(inputs: CryptoInputs) -> CryptoFairValue:
    """Compute settlement-aware probability for a crypto binary contract.

    The contract settles YES if the CFB Real-Time Index (60-second average
    from constituent exchanges) is above the strike at expiry.

    We estimate the RTI using available exchange prices and model the
    probability that this estimate will be above/below strike at settlement.
    """
    components: dict[str, float] = {}

    # --- Step 1: Shadow RTI estimation ---
    shadow_rti = _estimate_shadow_rti(inputs)
    components["shadow_rti"] = shadow_rti

    # --- Step 2: Volatility estimate ---
    vol = _estimate_volatility(inputs)
    components["vol_annualized"] = vol

    # --- Step 3: Time-scaled volatility ---
    minutes = max(inputs.minutes_remaining, 0.01)
    # Convert annualized vol to vol for remaining time
    # vol_period = vol_annual * sqrt(minutes / (365.25 * 24 * 60))
    vol_period = vol * math.sqrt(minutes / (365.25 * 24.0 * 60.0))

    # --- Step 4: Core probability via settlement-aware model ---
    # Uses Levy approximation for RTI averaging window near expiry,
    # standard N(d2) far from settlement, smooth blend in between.
    seconds_remaining = max(minutes * 60.0, 0.01)
    if shadow_rti <= 0 or inputs.strike <= 0:
        p_core = 0.5
    else:
        p_core = _compute_settlement_probability(
            shadow_rti, inputs.strike, seconds_remaining, vol
        )

    components["p_core"] = p_core

    # --- Step 5: Basis/funding signal ---
    basis = inputs.perp_price - shadow_rti if inputs.perp_price > 0 else 0.0
    basis_signal = 0.0
    if shadow_rti > 0 and abs(basis) > 0:
        basis_pct = basis / shadow_rti
        basis_signal = max(-0.05, min(0.05, basis_pct * 4.0))

    components["basis_signal"] = basis_signal

    # --- Step 6: Funding rate signal ---
    funding_signal = 0.0
    if inputs.funding_rate != 0:
        funding_signal = max(-0.03, min(0.03, inputs.funding_rate * 300.0))

    components["funding_signal"] = funding_signal

    # --- Step 7: Combine ---
    p_adjusted = p_core + basis_signal + funding_signal
    p_final = max(0.01, min(0.99, p_adjusted))

    # --- Step 9: Confidence ---
    confidence = 0.5
    if inputs.coinbase_spot > 0:
        confidence += 0.15  # have a CFB RTI constituent
    if inputs.binance_spot > 0:
        confidence += 0.1
    if inputs.deribit_dvol is not None:
        confidence += 0.1
    if inputs.perp_price > 0:
        confidence += 0.1
    confidence = min(1.0, confidence)

    return CryptoFairValue(
        probability=p_final,
        confidence=confidence,
        shadow_rti=shadow_rti,
        basis=basis,
        component_contributions=components,
    )


def _estimate_shadow_rti(inputs: CryptoInputs) -> float:
    """Estimate the CFB RTI from available exchange prices.

    The actual CFB RTI is a weighted average across constituent exchanges
    (Coinbase, Bitstamp, Kraken, etc.). We approximate using what we have.

    Weighting: Coinbase gets higher weight since it's a known constituent.
    """
    prices = []
    weights = []

    if inputs.coinbase_spot > 0:
        prices.append(inputs.coinbase_spot)
        weights.append(0.6)  # Coinbase is a CFB RTI constituent

    if inputs.binance_spot > 0:
        prices.append(inputs.binance_spot)
        weights.append(0.4)  # Binance is highly correlated but not a constituent

    if not prices:
        # Fallback to perp/mark price
        if inputs.mark_price > 0:
            return inputs.mark_price
        if inputs.perp_price > 0:
            return inputs.perp_price
        return 0.0

    # Weighted average
    total_weight = sum(weights)
    return sum(p * w for p, w in zip(prices, weights)) / total_weight


def _estimate_volatility(inputs: CryptoInputs) -> float:
    """Estimate annualized volatility for the probability model.

    Priority:
    1. Deribit DVOL (market-implied, most accurate)
    2. Realized vol from Binance bars
    3. Default
    """
    if inputs.deribit_dvol is not None and inputs.deribit_dvol > 0:
        # DVOL is already annualized percentage
        return inputs.deribit_dvol / 100.0

    if inputs.realized_vol_30m is not None and inputs.realized_vol_30m > 0:
        return inputs.realized_vol_30m

    # Default: ~50% annualized (typical BTC)
    return 0.50


_RTI_WINDOW_SECS = 60.0
_TRANSITION_SECS = 240.0
_SECONDS_PER_YEAR = 525_600.0 * 60.0
_RISK_FREE_RATE = 0.05


def _compute_settlement_probability(
    spot: float, strike: float, seconds_remaining: float, vol: float
) -> float:
    """Settlement-aware probability using Levy averaging near expiry."""
    if seconds_remaining <= 0.01:
        return 1.0 if spot >= strike else 0.0

    p_standard = _standard_binary_prob(spot, strike, seconds_remaining, vol)

    if seconds_remaining > _RTI_WINDOW_SECS + _TRANSITION_SECS:
        return p_standard

    p_averaging = _levy_averaging_prob(spot, strike, seconds_remaining, vol)

    if seconds_remaining <= _RTI_WINDOW_SECS:
        return p_averaging

    # Transition zone: smoothstep blend
    alpha = 1.0 - (seconds_remaining - _RTI_WINDOW_SECS) / _TRANSITION_SECS
    blend = alpha * alpha * (3.0 - 2.0 * alpha)
    return p_standard * (1.0 - blend) + p_averaging * blend


def _standard_binary_prob(
    spot: float, strike: float, seconds_remaining: float, vol: float
) -> float:
    """Standard Black-Scholes binary: N(d2)."""
    t = seconds_remaining / _SECONDS_PER_YEAR
    vol_period = vol * math.sqrt(t)
    if vol_period <= 0:
        return 1.0 if spot >= strike else 0.0
    d2 = (math.log(spot / strike) + (_RISK_FREE_RATE - 0.5 * vol * vol) * t) / (
        vol * math.sqrt(t)
    )
    return _norm_cdf(d2)


def _levy_averaging_prob(
    spot: float, strike: float, seconds_remaining: float, vol: float
) -> float:
    """Levy approximation for binary option on arithmetic average (TWAP).

    Var(TWAP over τ) ≈ S²·σ²·τ/3, so effective vol = σ·√(τ/3).
    When partially inside the window, observed prices shift the effective strike.
    """
    elapsed = max(_RTI_WINDOW_SECS - seconds_remaining, 0.0)
    alpha = elapsed / _RTI_WINDOW_SECS
    remaining_frac = 1.0 - alpha

    if remaining_frac < 0.01:
        return 1.0 if spot >= strike else 0.0

    k_eff = (strike - alpha * spot) / remaining_frac
    if k_eff <= 0.0:
        return 0.99

    tau = seconds_remaining / _SECONDS_PER_YEAR
    vol_avg = vol * math.sqrt(tau / 3.0)

    if vol_avg <= 1e-12:
        return 1.0 if spot >= k_eff else 0.0

    d2 = (math.log(spot / k_eff) + (_RISK_FREE_RATE - vol * vol / 6.0) * tau) / vol_avg
    return _norm_cdf(d2)


def _norm_cdf(x: float) -> float:
    """Standard normal CDF using erfc."""
    return 0.5 * math.erfc(-x / math.sqrt(2.0))
