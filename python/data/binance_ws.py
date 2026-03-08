"""BE-3.2: Binance BTC WebSocket Feed — real-time spot price & 30-min volatility."""

from __future__ import annotations

import asyncio
import json
import math
from collections import deque
from dataclasses import dataclass, field
from datetime import datetime, timezone

import structlog
import websockets
import websockets.exceptions

logger = structlog.get_logger()


@dataclass(frozen=True, slots=True)
class OHLCBar:
    open: float
    high: float
    low: float
    close: float
    timestamp: datetime
    volume: float


@dataclass(frozen=True, slots=True)
class CryptoState:
    spot_price: float
    realized_vol_30m: float | None
    ewma_vol_30m: float | None
    bars_count: int
    last_updated: datetime


@dataclass
class BinanceFeed:
    """Maintains real-time BTC spot price and 30-min realized volatility.

    Connects to Binance trade stream, accumulates 1-minute OHLC bars,
    and computes annualized realized volatility from 30 1-min log returns.
    """

    ws_url: str = "wss://stream.binance.com:9443/ws/btcusdt@trade"

    spot_price: float = 0.0
    bars_1m: deque[OHLCBar] = field(default_factory=lambda: deque(maxlen=60))
    realized_vol_30m: float | None = None
    ewma_vol_30m: float | None = None
    last_updated: datetime = field(default_factory=lambda: datetime.now(timezone.utc))

    # EWMA decay factor (0.94 = RiskMetrics standard for daily, adapted for 1-min)
    _ewma_lambda: float = 0.94
    _ewma_variance: float = 0.0

    _current_bar_minute: int = -1
    _current_open: float = 0.0
    _current_high: float = 0.0
    _current_low: float = float("inf")
    _current_close: float = 0.0
    _current_volume: float = 0.0
    _running: bool = False

    async def connect(self) -> None:
        """Connect to Binance WS with auto-reconnect. Runs until cancelled."""
        self._running = True
        backoff = 1.0
        max_backoff = 30.0

        while self._running:
            try:
                await self._stream()
            except (
                websockets.exceptions.ConnectionClosed,
                ConnectionError,
                OSError,
            ) as exc:
                logger.warning(
                    "binance_ws_disconnected",
                    error=str(exc),
                    backoff=backoff,
                )
                await asyncio.sleep(backoff)
                backoff = min(backoff * 2, max_backoff)
            except asyncio.CancelledError:
                logger.info("binance_ws_cancelled")
                self._running = False
                return

    async def _stream(self) -> None:
        async with websockets.connect(
            self.ws_url,
            ping_interval=20,
            ping_timeout=10,
            close_timeout=5,
        ) as ws:
            logger.info("binance_ws_connected", url=self.ws_url)
            # Reset backoff on successful connect (caller handles this via loop)

            async for raw_msg in ws:
                msg = json.loads(raw_msg)
                self._handle_trade(msg)

    def _handle_trade(self, msg: dict) -> None:
        price = float(msg["p"])
        qty = float(msg["q"])
        trade_time_ms = msg["T"]
        trade_time = datetime.fromtimestamp(trade_time_ms / 1000, tz=timezone.utc)
        current_minute = trade_time_ms // 60_000

        self.spot_price = price
        self.last_updated = trade_time

        # Roll bar on new minute boundary
        if current_minute != self._current_bar_minute:
            if self._current_bar_minute >= 0:
                # Close out previous bar
                bar = OHLCBar(
                    open=self._current_open,
                    high=self._current_high,
                    low=self._current_low,
                    close=self._current_close,
                    timestamp=trade_time,
                    volume=self._current_volume,
                )
                self.bars_1m.append(bar)
                self._recompute_vol()
                self._recompute_vol_ewma(bar)

            # Start new bar
            self._current_bar_minute = current_minute
            self._current_open = price
            self._current_high = price
            self._current_low = price
            self._current_close = price
            self._current_volume = qty
        else:
            self._current_high = max(self._current_high, price)
            self._current_low = min(self._current_low, price)
            self._current_close = price
            self._current_volume += qty

    def _recompute_vol(self) -> None:
        """Compute 30-min annualized realized volatility from 1-min log returns."""
        if len(self.bars_1m) < 31:
            self.realized_vol_30m = None
            return

        # Use last 30 bars (need 31 closes for 30 returns)
        closes = [bar.close for bar in list(self.bars_1m)[-31:]]
        log_returns = [
            math.log(closes[i] / closes[i - 1])
            for i in range(1, len(closes))
            if closes[i - 1] > 0
        ]

        if len(log_returns) < 2:
            self.realized_vol_30m = None
            return

        mean = sum(log_returns) / len(log_returns)
        variance = sum((r - mean) ** 2 for r in log_returns) / (len(log_returns) - 1)
        sigma_1min = math.sqrt(variance)

        # Annualize: sqrt(minutes_per_year)
        self.realized_vol_30m = sigma_1min * math.sqrt(525_600)

    def _recompute_vol_ewma(self, bar: OHLCBar) -> None:
        """Update EWMA volatility with the latest bar's return.

        Uses exponentially weighted moving average: more responsive to
        recent volatility changes than equal-weighted realized vol.
        Formula: variance_t = lambda * variance_{t-1} + (1-lambda) * r_t^2
        """
        if len(self.bars_1m) < 2:
            return

        bars = list(self.bars_1m)
        prev_close = bars[-2].close
        if prev_close <= 0:
            return

        log_return = math.log(bar.close / prev_close)

        if self._ewma_variance == 0.0:
            # Bootstrap from available returns (don't wait for 10 bars)
            if len(self.bars_1m) >= 10:
                closes = [b.close for b in bars[-11:]]
                returns = [
                    math.log(closes[i] / closes[i - 1])
                    for i in range(1, len(closes))
                    if closes[i - 1] > 0
                ]
                if returns:
                    self._ewma_variance = sum(r * r for r in returns) / len(returns)
            else:
                # Single-return bootstrap to avoid staying at 0
                self._ewma_variance = log_return * log_return

        self._ewma_variance = (
            self._ewma_lambda * self._ewma_variance
            + (1.0 - self._ewma_lambda) * log_return * log_return
        )

        sigma_1min = math.sqrt(self._ewma_variance)
        self.ewma_vol_30m = sigma_1min * math.sqrt(525_600)

    def get_state(self) -> CryptoState:
        """Snapshot of current spot, vol, bar data."""
        return CryptoState(
            spot_price=self.spot_price,
            realized_vol_30m=self.realized_vol_30m,
            ewma_vol_30m=self.ewma_vol_30m,
            bars_count=len(self.bars_1m),
            last_updated=self.last_updated,
        )

    def stop(self) -> None:
        self._running = False
