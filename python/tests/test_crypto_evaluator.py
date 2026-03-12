"""Tests for crypto signal evaluator."""

from datetime import UTC, datetime, timedelta

from signals.crypto import BlackoutWindow, CryptoSignalEvaluator
from signals.types import Contract, OrderbookState


def _make_contract(minutes_ahead: float = 10.0, threshold: float = 65000.0) -> Contract:
    return Contract(
        ticker="BTC-65K-T1200",
        category="crypto",
        threshold=threshold,
        settlement_time=datetime.now(UTC) + timedelta(minutes=minutes_ahead),
    )


def _make_orderbook(mid: float = 0.50, spread: float = 0.04) -> OrderbookState:
    return OrderbookState(
        mid_price=mid,
        spread=spread,
        best_bid=mid - spread / 2,
        best_ask=mid + spread / 2,
        bid_depth=100,
        ask_depth=100,
    )


class TestCryptoEvaluator:
    def test_outside_time_window(self):
        evaluator = CryptoSignalEvaluator()
        contract = _make_contract(minutes_ahead=20.0)
        book = _make_orderbook()
        now = datetime.now(UTC)

        signal, rejection, state = evaluator.evaluate(contract, 65500.0, 0.60, now, book)
        assert signal is None
        assert rejection is None

    def test_blackout_rejects(self):
        now = datetime.now(UTC)
        blackout = BlackoutWindow(
            "FOMC",
            now - timedelta(minutes=5),
            now + timedelta(minutes=30),
        )
        evaluator = CryptoSignalEvaluator(blackout_windows=[blackout])
        contract = _make_contract()
        book = _make_orderbook()

        signal, rejection, state = evaluator.evaluate(contract, 65500.0, 0.60, now, book)
        assert signal is None
        assert rejection is not None
        assert "blackout" in rejection.rejection_reason
        assert "FOMC" in rejection.rejection_reason

    def test_stale_btc_feed_rejects(self):
        evaluator = CryptoSignalEvaluator()
        contract = _make_contract()
        book = _make_orderbook()
        stale_time = datetime.now(UTC) - timedelta(seconds=60)

        signal, rejection, state = evaluator.evaluate(contract, 65500.0, 0.60, stale_time, book)
        assert signal is None
        assert rejection is not None
        assert "stale" in rejection.rejection_reason

    def test_missing_vol_uses_fallback(self):
        evaluator = CryptoSignalEvaluator()
        contract = _make_contract()
        book = _make_orderbook()
        now = datetime.now(UTC)

        # Phase 12.3: missing vol now falls back to DEFAULT_VOL=0.60 instead of rejecting
        signal, rejection, state = evaluator.evaluate(contract, 65500.0, None, now, book)
        assert signal is not None
        assert signal.signal_type == "crypto"

    def test_signal_generated_with_edge(self):
        evaluator = CryptoSignalEvaluator()
        # Spot well above strike → model_prob high, market at 0.50 → big edge
        contract = _make_contract(threshold=60000.0)
        book = _make_orderbook(mid=0.50, spread=0.04)
        now = datetime.now(UTC)

        signal, rejection, state = evaluator.evaluate(contract, 65000.0, 0.60, now, book)
        assert signal is not None
        assert signal.direction == "yes"
        assert signal.signal_type == "crypto"
        assert signal.edge > 0.06

    def test_insufficient_edge_rejected(self):
        evaluator = CryptoSignalEvaluator()
        # Spot at strike → model prob ~0.50, market ~0.50 → no edge
        contract = _make_contract(threshold=65000.0)
        book = _make_orderbook(mid=0.50, spread=0.04)
        now = datetime.now(UTC)

        signal, rejection, state = evaluator.evaluate(contract, 65000.0, 0.60, now, book)
        assert signal is None

    def test_cooldown(self):
        evaluator = CryptoSignalEvaluator()
        contract = _make_contract(threshold=55000.0)
        book = _make_orderbook(mid=0.50, spread=0.04)
        now = datetime.now(UTC)

        signal1, _, _ = evaluator.evaluate(contract, 65000.0, 0.60, now, book)
        assert signal1 is not None

        signal2, rejection2, _ = evaluator.evaluate(contract, 65000.0, 0.60, now, book)
        assert signal2 is None
        assert rejection2 is not None
        assert "cooldown" in rejection2.rejection_reason

    def test_exit_signal(self):
        evaluator = CryptoSignalEvaluator()
        # Bought YES at 0.50 but spot now way below strike
        contract = _make_contract(threshold=70000.0, minutes_ahead=8.0)
        book = _make_orderbook(mid=0.10, spread=0.04)
        now = datetime.now(UTC)

        exit_signal = evaluator.evaluate_exit(
            contract,
            60000.0,
            0.60,
            now,
            book,
            held_direction="yes",
            entry_price=0.50,
        )
        assert exit_signal is not None
        assert exit_signal.action.value == "exit"

    def test_model_state_returned(self):
        evaluator = CryptoSignalEvaluator()
        contract = _make_contract()
        book = _make_orderbook()
        now = datetime.now(UTC)

        _, _, state = evaluator.evaluate(contract, 65500.0, 0.60, now, book)
        assert state.ticker == contract.ticker
        assert state.signal_type == "crypto"


class TestBlackoutWindow:
    def test_active_during_window(self):
        now = datetime.now(UTC)
        w = BlackoutWindow("FOMC", now - timedelta(hours=1), now + timedelta(hours=1))
        assert w.is_active(now)

    def test_inactive_before_window(self):
        now = datetime.now(UTC)
        w = BlackoutWindow("FOMC", now + timedelta(hours=1), now + timedelta(hours=2))
        assert not w.is_active(now)

    def test_inactive_after_window(self):
        now = datetime.now(UTC)
        w = BlackoutWindow("FOMC", now - timedelta(hours=2), now - timedelta(hours=1))
        assert not w.is_active(now)
