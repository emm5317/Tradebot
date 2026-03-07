"""Kelly criterion position sizing."""


def kelly_fraction(edge: float, odds: float) -> float:
    """Compute optimal Kelly fraction.

    Args:
        edge: Estimated probability of winning minus market implied probability.
        odds: Payout odds (e.g., 1.0 for even money).

    Returns:
        Fraction of bankroll to wager (clamped to [0, 1]).
    """
    if odds <= 0:
        return 0.0
    f = (edge * odds - (1 - edge)) / odds
    return max(0.0, min(1.0, f))


def half_kelly(edge: float, odds: float) -> float:
    """Conservative half-Kelly sizing."""
    return kelly_fraction(edge, odds) / 2
