"""Tests for Binance BTC WebSocket Feed."""

import math
from datetime import datetime, timezone

from data.binance_ws import BinanceFeed, CryptoState


def _make_trade(price: float, qty: float, ts_ms: int) -> dict:
    return {"p": str(price), "q": str(qty), "T": ts_ms}


def test_handle_trade_updates_spot():
    feed = BinanceFeed()
    ts = 1700000000000  # some timestamp in ms

    feed._handle_trade(_make_trade(42000.0, 0.5, ts))
    assert feed.spot_price == 42000.0

    feed._handle_trade(_make_trade(42100.0, 0.1, ts + 1000))
    assert feed.spot_price == 42100.0


def test_bar_rolls_on_minute_boundary():
    feed = BinanceFeed()
    base_ms = 1700000000000
    minute_ms = 60_000

    # All trades in minute 0
    feed._handle_trade(_make_trade(100.0, 1.0, base_ms))
    feed._handle_trade(_make_trade(105.0, 1.0, base_ms + 10_000))
    assert len(feed.bars_1m) == 0  # bar not closed yet

    # First trade in minute 1 closes minute 0's bar
    feed._handle_trade(_make_trade(110.0, 1.0, base_ms + minute_ms))
    assert len(feed.bars_1m) == 1
    bar = feed.bars_1m[0]
    assert bar.open == 100.0
    assert bar.high == 105.0
    assert bar.close == 105.0


def test_get_state_snapshot():
    feed = BinanceFeed()
    feed.spot_price = 50000.0
    feed.realized_vol_30m = 0.65
    feed.ewma_vol_30m = 0.70

    state = feed.get_state()
    assert isinstance(state, CryptoState)
    assert state.spot_price == 50000.0
    assert state.realized_vol_30m == 0.65
    assert state.ewma_vol_30m == 0.70


def test_vol_none_with_insufficient_bars():
    feed = BinanceFeed()
    base_ms = 1700000000000
    minute_ms = 60_000

    # Only create 5 bars — not enough for vol calculation (need 31 closes)
    for i in range(6):
        feed._handle_trade(_make_trade(100.0 + i, 1.0, base_ms + i * minute_ms))

    assert feed.realized_vol_30m is None


def test_vol_computed_with_enough_bars():
    feed = BinanceFeed()
    base_ms = 1700000000000
    minute_ms = 60_000

    # Create 32 bars (need 31 closes for 30 returns)
    for i in range(33):
        price = 50000.0 + (i % 3) * 10  # small oscillation
        feed._handle_trade(_make_trade(price, 1.0, base_ms + i * minute_ms))

    assert feed.realized_vol_30m is not None
    assert feed.realized_vol_30m > 0


def test_ewma_vol_computed_with_enough_bars():
    feed = BinanceFeed()
    base_ms = 1700000000000
    minute_ms = 60_000

    # Create 15 bars — enough for EWMA initialization (10+ bars)
    for i in range(16):
        price = 50000.0 + (i % 3) * 10
        feed._handle_trade(_make_trade(price, 1.0, base_ms + i * minute_ms))

    assert feed.ewma_vol_30m is not None
    assert feed.ewma_vol_30m > 0


def test_ewma_vol_responds_to_volatility_spike():
    feed = BinanceFeed()
    base_ms = 1700000000000
    minute_ms = 60_000

    # Calm period: small oscillations
    for i in range(20):
        price = 50000.0 + (i % 2) * 5
        feed._handle_trade(_make_trade(price, 1.0, base_ms + i * minute_ms))

    calm_ewma = feed.ewma_vol_30m

    # Volatile period: large swings
    for i in range(20, 30):
        price = 50000.0 + (i % 2) * 500  # 10x larger moves
        feed._handle_trade(_make_trade(price, 1.0, base_ms + i * minute_ms))

    volatile_ewma = feed.ewma_vol_30m

    # EWMA should have increased significantly
    assert volatile_ewma is not None
    assert calm_ewma is not None
    assert volatile_ewma > calm_ewma
