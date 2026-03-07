"""Binance WebSocket client for real-time crypto prices."""

import asyncio
import websockets
import json


class BinanceWS:
    """Streams real-time price data from Binance."""

    WS_URL = "wss://stream.binance.com:9443/ws"

    def __init__(self, symbol: str = "btcusdt"):
        self.symbol = symbol
        self.stream = f"{symbol}@trade"

    async def listen(self, callback):
        """Connect and stream trade events to callback."""
        url = f"{self.WS_URL}/{self.stream}"
        async with websockets.connect(url) as ws:
            async for msg in ws:
                data = json.loads(msg)
                await callback(data)
