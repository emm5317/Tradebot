"""BE-4.2: Black-Scholes Binary Option Model.

Near-expiry binary option pricing using realized 30-min volatility
from the Binance feed. Uses math.erfc for fast CDF evaluation.
"""

from __future__ import annotations

import math

from models.physics import fast_norm_cdf

# Minutes per year for time conversion
_MINUTES_PER_YEAR = 525_600.0


def compute_binary_probability(
    spot: float,
    strike: float,
    minutes_remaining: float,
    sigma_annual: float,
    risk_free_rate: float = 0.05,
) -> float:
    """N(d2) for near-expiry binary call option.

    Computes the risk-neutral probability that the spot price will be
    above the strike at expiration using Black-Scholes d2.

    Args:
        spot: Current BTC spot price.
        strike: Contract strike price.
        minutes_remaining: Minutes until settlement.
        sigma_annual: Annualized realized volatility (e.g., 0.60 for 60%).
        risk_free_rate: Annual risk-free rate (default 5%).

    Returns:
        Probability [0, 1] that spot >= strike at settlement.
    """
    if minutes_remaining <= 0:
        return 1.0 if spot >= strike else 0.0

    if spot <= 0 or strike <= 0:
        return 0.0

    if sigma_annual <= 0:
        # Zero vol — deterministic
        return 1.0 if spot >= strike else 0.0

    T = minutes_remaining / _MINUTES_PER_YEAR

    sqrt_T = math.sqrt(T)
    d2 = (
        math.log(spot / strike) + (risk_free_rate - 0.5 * sigma_annual**2) * T
    ) / (sigma_annual * sqrt_T)

    return fast_norm_cdf(d2)


def compute_binary_put_probability(
    spot: float,
    strike: float,
    minutes_remaining: float,
    sigma_annual: float,
    risk_free_rate: float = 0.05,
) -> float:
    """Probability that spot < strike at settlement (binary put)."""
    return 1.0 - compute_binary_probability(
        spot, strike, minutes_remaining, sigma_annual, risk_free_rate
    )
