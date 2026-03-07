"""Fetches historical Kalshi market data for backtesting."""

import httpx


class KalshiHistoryClient:
    """Downloads historical market snapshots from Kalshi."""

    def __init__(self, api_url: str = "https://api.elections.kalshi.com/trade-api/v2"):
        self.http = httpx.Client(timeout=30)
        self.api_url = api_url

    def get_market_history(self, ticker: str) -> list[dict]:
        """Fetch historical candlestick / settlement data."""
        # TODO: paginate through history endpoint
        return []
