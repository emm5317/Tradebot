"""Tests for signal publisher."""

from datetime import datetime, timezone

from signals.publisher import SignalPublisher
from signals.types import ModelState, RejectedSignal, SignalSchema


class TestSignalPublisher:
    def test_creates_without_deps(self):
        """Publisher should work without NATS/DB/Redis for testing."""
        publisher = SignalPublisher()
        assert publisher.nats is None
        assert publisher.db_pool is None
        assert publisher.redis is None


class TestSignalSchema:
    def test_valid_signal(self):
        signal = SignalSchema(
            ticker="TEMP-KORD-70-T1200",
            signal_type="weather",
            direction="yes",
            model_prob=0.65,
            market_price=0.50,
            edge=0.13,
            kelly_fraction=0.10,
            minutes_remaining=12.0,
            spread=0.04,
            order_imbalance=0.6,
        )
        assert signal.ticker == "TEMP-KORD-70-T1200"
        assert signal.action.value == "entry"

    def test_json_roundtrip(self):
        signal = SignalSchema(
            ticker="BTC-65K",
            signal_type="crypto",
            direction="no",
            model_prob=0.30,
            market_price=0.45,
            edge=0.08,
            kelly_fraction=0.06,
            minutes_remaining=8.0,
        )
        json_str = signal.model_dump_json()
        parsed = SignalSchema.model_validate_json(json_str)
        assert parsed.ticker == signal.ticker
        assert parsed.direction == signal.direction
        assert abs(parsed.edge - signal.edge) < 1e-10

    def test_invalid_prob_rejected(self):
        import pytest

        with pytest.raises(Exception):
            SignalSchema(
                ticker="X",
                signal_type="weather",
                direction="yes",
                model_prob=1.5,  # invalid: > 1.0
                market_price=0.50,
                edge=0.10,
                kelly_fraction=0.05,
                minutes_remaining=10.0,
            )

    def test_negative_edge_rejected(self):
        import pytest

        with pytest.raises(Exception):
            SignalSchema(
                ticker="X",
                signal_type="weather",
                direction="yes",
                model_prob=0.60,
                market_price=0.50,
                edge=-0.10,  # invalid: negative
                kelly_fraction=0.05,
                minutes_remaining=10.0,
            )


class TestRejectedSignal:
    def test_valid_rejection(self):
        r = RejectedSignal(
            ticker="TEMP-KORD-70",
            signal_type="weather",
            rejection_reason="stale_observation",
        )
        assert r.rejection_reason == "stale_observation"
        assert r.model_prob is None

    def test_json_roundtrip(self):
        r = RejectedSignal(
            ticker="BTC-65K",
            signal_type="crypto",
            rejection_reason="blackout (FOMC)",
            model_prob=0.55,
            market_price=0.50,
            edge=0.03,
            minutes_remaining=10.0,
        )
        json_str = r.model_dump_json()
        parsed = RejectedSignal.model_validate_json(json_str)
        assert parsed.rejection_reason == r.rejection_reason


class TestModelState:
    def test_model_state(self):
        state = ModelState(
            ticker="TEMP-KORD-70",
            signal_type="weather",
            model_prob=0.65,
            physics_prob=0.70,
            climo_prob=0.55,
            trend_prob=0.60,
            market_price=0.50,
            edge=0.15,
            spread=0.04,
        )
        assert state.model_prob == 0.65
        json_str = state.model_dump_json()
        assert "physics_prob" in json_str
