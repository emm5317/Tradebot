"""Crypto price signal generation for BTC / ETH range contracts."""


class CryptoSignal:
    """Generates trading signals from crypto price data."""

    def __init__(self, symbol: str = "BTCUSDT"):
        self.symbol = symbol

    def generate(self) -> dict | None:
        """Produce a signal dict or None if no edge detected."""
        # TODO: compare model forecast vs. market implied probability
        return None
