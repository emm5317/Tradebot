"""Scans Kalshi markets for tradeable opportunities."""


class MarketScanner:
    """Periodically scans available markets and filters by criteria."""

    def __init__(self, min_volume: int = 100, min_spread: float = 0.02):
        self.min_volume = min_volume
        self.min_spread = min_spread

    def scan(self) -> list[dict]:
        """Return markets that pass the filter criteria."""
        # TODO: fetch markets from Kalshi, apply filters
        return []
