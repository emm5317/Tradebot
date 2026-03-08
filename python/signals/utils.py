"""Shared trading utilities for signal evaluators.

Fill price estimation, Kelly criterion, and spread-adjusted edge calculation.
Used by all evaluator implementations.
"""

from __future__ import annotations

from signals.types import OrderbookState


def estimate_fill_price(direction: str, orderbook: OrderbookState) -> float:
    """Estimate the actual fill price from the orderbook.

    Buying YES -> pay the best ask.
    Buying NO -> pay (1 - best bid) effectively.
    Falls back to mid + half spread.
    """
    if direction == "yes":
        if orderbook.best_ask is not None:
            return orderbook.best_ask
        return orderbook.mid_price + orderbook.spread / 2.0
    else:
        if orderbook.best_bid is not None:
            return orderbook.best_bid
        return orderbook.mid_price - orderbook.spread / 2.0


def compute_kelly(
    model_prob: float, fill_price: float, direction: str
) -> float:
    """Kelly criterion for binary outcome using estimated fill price.

    For YES: pay fill_price, win (1 - fill_price) if correct.
    For NO: pay (1 - fill_price), win fill_price if correct.
    """
    if direction == "yes":
        win_prob = model_prob
        win_payout = 1.0 - fill_price
        lose_payout = fill_price
    else:
        win_prob = 1.0 - model_prob
        win_payout = fill_price
        lose_payout = 1.0 - fill_price

    if win_payout <= 0:
        return 0.0

    lose_prob = 1.0 - win_prob
    kelly = (win_prob * win_payout - lose_prob * lose_payout) / win_payout

    return max(0.0, kelly)


def compute_effective_edge(
    raw_edge: float,
    spread: float,
    wide_spread_threshold: float = 0.10,
) -> float:
    """Compute spread-adjusted edge.

    Subtracts half-spread cost and applies a 15% discount when spread
    exceeds the wide threshold.
    """
    spread_cost = spread / 2.0
    effective = raw_edge - spread_cost

    if spread > wide_spread_threshold:
        effective *= 0.85

    return effective


def compute_dynamic_kelly_multiplier(
    base_multiplier: float,
    recent_accuracy: float | None,
    signal_count: int,
) -> float:
    """Scale Kelly multiplier based on recent signal accuracy.

    If accuracy > 60% over last 50+ signals, scale up (1.5x).
    If accuracy < 45%, scale down (0.5x).
    Otherwise use base multiplier.

    Returns multiplier clamped to [0.1, 0.5].
    """
    if recent_accuracy is None or signal_count < 20:
        return base_multiplier

    if recent_accuracy > 0.60:
        scaled = base_multiplier * 1.5
    elif recent_accuracy < 0.45:
        scaled = base_multiplier * 0.5
    else:
        scaled = base_multiplier

    return max(0.1, min(0.5, scaled))


def compute_confidence(spread: float, minutes_remaining: float) -> float:
    """Compute signal confidence from spread and time to expiry.

    Tight spread = high confidence, wide spread = low confidence.
    More time = higher confidence, less time = lower confidence.

    Returns a value in [0.1, 1.0].
    """
    # Spread factor: 1.0 at spread=0, 0.0 at spread=0.20
    spread_factor = max(0.0, 1.0 - spread / 0.20)

    # Time decay: 1.0 at 30 min, 0.5 at 2 min
    if minutes_remaining >= 30.0:
        time_factor = 1.0
    elif minutes_remaining <= 2.0:
        time_factor = 0.5
    else:
        time_factor = 0.5 + 0.5 * (minutes_remaining - 2.0) / 28.0

    confidence = spread_factor * time_factor
    return max(0.1, min(1.0, confidence))


def determine_direction(
    model_prob: float, market_price: float
) -> tuple[str, float]:
    """Determine trade direction and raw edge from model vs market price.

    Returns:
        Tuple of (direction, raw_edge).
    """
    if model_prob > market_price:
        return "yes", model_prob - market_price
    else:
        return "no", market_price - model_prob
