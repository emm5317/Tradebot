"""Binary option pricing helpers."""

from scipy import stats
import numpy as np


def binary_probability(
    current: float, strike: float, vol: float, time_to_expiry: float
) -> float:
    """Probability that value ends above strike (digital call)."""
    if time_to_expiry <= 0 or vol <= 0:
        return 1.0 if current >= strike else 0.0
    d2 = (np.log(current / strike) - 0.5 * vol**2 * time_to_expiry) / (
        vol * np.sqrt(time_to_expiry)
    )
    return float(stats.norm.cdf(d2))


def implied_edge(model_prob: float, market_price: float) -> float:
    """Compute edge as model probability minus market price."""
    return model_prob - market_price
