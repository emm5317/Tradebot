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


def compute_kelly(model_prob: float, fill_price: float, direction: str) -> float:
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


def determine_direction(model_prob: float, market_price: float) -> tuple[str, float]:
    """Determine trade direction and raw edge from model vs market price.

    Returns:
        Tuple of (direction, raw_edge).
    """
    if model_prob > market_price:
        return "yes", model_prob - market_price
    else:
        return "no", market_price - model_prob
