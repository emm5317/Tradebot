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

    # --- Step 4: Core probability via Gaussian model ---
    # P(RTI > strike) = N(d2) where d2 = ln(RTI/strike) / vol_period
    if shadow_rti <= 0 or inputs.strike <= 0:
        p_core = 0.5
    elif vol_period <= 0:
        p_core = 1.0 if shadow_rti >= inputs.strike else 0.0
    else:
        d2 = math.log(shadow_rti / inputs.strike) / vol_period
        p_core = _norm_cdf(d2)

    components["p_core"] = p_core

    # --- Step 5: Basis/funding signal ---
    basis = inputs.perp_price - shadow_rti if inputs.perp_price > 0 else 0.0
    basis_signal = 0.0
    if shadow_rti > 0 and abs(basis) > 0:
        # Positive basis (contango) suggests market expects higher prices
        basis_pct = basis / shadow_rti
        # Dampen: ±0.5% basis → ±0.02 probability adjustment
        basis_signal = max(-0.05, min(0.05, basis_pct * 4.0))

    components["basis_signal"] = basis_signal

    # --- Step 6: Funding rate signal ---
    funding_signal = 0.0
    if inputs.funding_rate != 0:
        # Positive funding = longs pay shorts = bullish sentiment
        # Scale: 0.01% funding → ~0.01 probability adjustment
        funding_signal = max(-0.03, min(0.03, inputs.funding_rate * 300.0))

    components["funding_signal"] = funding_signal

    # --- Step 7: RTI averaging window effect ---
    # RTI uses 60-second average, which reduces tail risk near expiry
    # The averaging smooths out spikes, making extreme outcomes less likely
    averaging_dampening = 1.0
    if minutes < 5.0:
        # Within 5 minutes of expiry, the 60s averaging window matters more
        # It pulls extreme probabilities toward 0.5
        averaging_dampening = 0.85 + 0.15 * (minutes / 5.0)

    # --- Step 8: Combine ---
    # Adjust core probability with signals
    p_adjusted = p_core + basis_signal + funding_signal

    # Apply averaging dampening (pulls toward 0.5)
    p_final = 0.5 + (p_adjusted - 0.5) * averaging_dampening

    # Clamp
    p_final = max(0.01, min(0.99, p_final))

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


def _norm_cdf(x: float) -> float:
    """Standard normal CDF using erfc."""
    return 0.5 * math.erfc(-x / math.sqrt(2.0))
