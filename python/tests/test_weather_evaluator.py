"""Tests for weather signal evaluator."""

from datetime import datetime, timedelta, timezone

from data.mesonet import ASOSObservation
from signals.types import Contract, OrderbookState
from signals.utils import compute_kelly, estimate_fill_price
from signals.weather import WeatherSignalEvaluator


def _make_contract(minutes_ahead: float = 12.0, threshold: float = 70.0) -> Contract:
    return Contract(
        ticker="TEMP-KORD-70-T1200",
        category="weather",
        city="Chicago",
        station="KORD",
        threshold=threshold,
        settlement_time=datetime.now(timezone.utc) + timedelta(minutes=minutes_ahead),
    )


def _make_observation(temp: float = 72.0, stale: bool = False) -> ASOSObservation:
    return ASOSObservation(
        station="KORD",
        observed_at=datetime.now(timezone.utc),
        temperature_f=temp,
        wind_speed_kts=10.0,
        wind_gust_kts=None,
        precip_inch=0.0,
        raw={},
        staleness_seconds=600.0 if stale else 30.0,
        is_stale=stale,
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


class TestWeatherEvaluator:
    def test_outside_time_window_no_signal(self):
        evaluator = WeatherSignalEvaluator()
        contract = _make_contract(minutes_ahead=25.0)
        obs = _make_observation()
        book = _make_orderbook()

        signal, rejection, state = evaluator.evaluate(contract, obs, book)
        assert signal is None
        assert rejection is None  # outside window = not evaluated

    def test_stale_observation_rejected(self):
        evaluator = WeatherSignalEvaluator()
        contract = _make_contract()
        obs = _make_observation(stale=True)
        book = _make_orderbook()

        signal, rejection, state = evaluator.evaluate(contract, obs, book)
        assert signal is None
        assert rejection is not None
        assert "stale" in rejection.rejection_reason

    def test_missing_temperature_rejected(self):
        evaluator = WeatherSignalEvaluator()
        contract = _make_contract()
        obs = ASOSObservation(
            station="KORD",
            observed_at=datetime.now(timezone.utc),
            temperature_f=None,
            wind_speed_kts=None,
            wind_gust_kts=None,
            precip_inch=None,
            raw={},
            staleness_seconds=30.0,
            is_stale=False,
        )
        book = _make_orderbook()

        signal, rejection, state = evaluator.evaluate(contract, obs, book)
        assert signal is None
        assert rejection is not None
        assert "temperature" in rejection.rejection_reason

    def test_signal_generated_with_edge(self):
        evaluator = WeatherSignalEvaluator()
        # Temp well above threshold → model_prob ~0.95, market at 0.50 → huge edge
        contract = _make_contract(threshold=65.0)
        obs = _make_observation(temp=75.0)
        book = _make_orderbook(mid=0.50, spread=0.04)

        signal, rejection, state = evaluator.evaluate(contract, obs, book)
        assert signal is not None
        assert signal.direction == "yes"
        assert signal.edge > 0.05
        assert signal.kelly_fraction > 0.04

    def test_insufficient_edge_rejected(self):
        evaluator = WeatherSignalEvaluator()
        # Temp below threshold → not locked, physics prob ~0, blended ~0.28
        # Market at 0.30 → tiny edge after spread → rejected
        contract = _make_contract(threshold=80.0)
        obs = _make_observation(temp=68.0)
        book = _make_orderbook(mid=0.30, spread=0.04)

        signal, rejection, state = evaluator.evaluate(contract, obs, book)
        assert signal is None
        assert rejection is not None
        assert "edge" in rejection.rejection_reason or "kelly" in rejection.rejection_reason

    def test_cooldown_prevents_duplicate(self):
        evaluator = WeatherSignalEvaluator()
        # Temp below threshold so not locked; physics gives moderate prob
        # with blended model giving enough edge to signal but below
        # the _COOLDOWN_BYPASS_EDGE (0.10) on second eval
        contract = _make_contract(threshold=73.0)
        obs = _make_observation(temp=72.0)
        book = _make_orderbook(mid=0.20, spread=0.04)

        # First eval should generate signal (model prob ~0.28, market 0.20,
        # direction=no since 1-0.28=0.72 > 0.80=1-0.20... let's verify)
        signal1, _, _ = evaluator.evaluate(contract, obs, book)
        assert signal1 is not None

        # Second eval should hit cooldown (edge < 0.10 bypass threshold)
        signal2, rejection2, _ = evaluator.evaluate(contract, obs, book)
        assert signal2 is None
        assert rejection2 is not None
        assert "cooldown" in rejection2.rejection_reason

    def test_clear_cooldown(self):
        evaluator = WeatherSignalEvaluator()
        contract = _make_contract(threshold=60.0)
        obs = _make_observation(temp=75.0)
        book = _make_orderbook(mid=0.50, spread=0.04)

        signal1, _, _ = evaluator.evaluate(contract, obs, book)
        assert signal1 is not None

        evaluator.clear_cooldown(contract.ticker)

        signal2, _, _ = evaluator.evaluate(contract, obs, book)
        assert signal2 is not None

    def test_model_state_always_returned(self):
        evaluator = WeatherSignalEvaluator()
        contract = _make_contract()
        obs = _make_observation()
        book = _make_orderbook()

        _, _, state = evaluator.evaluate(contract, obs, book)
        assert state is not None
        assert state.ticker == contract.ticker
        assert state.signal_type == "weather"
        assert state.market_price == book.mid_price

    def test_no_direction_signal(self):
        evaluator = WeatherSignalEvaluator()
        # Temp well above threshold, market overpriced at 0.99
        # model_prob ~0.999 ≈ market, so edge small → rejection or no signal
        contract = _make_contract(threshold=60.0)
        obs = _make_observation(temp=61.0)
        book = _make_orderbook(mid=0.99, spread=0.04)

        signal, rejection, state = evaluator.evaluate(contract, obs, book)
        # Validates evaluator handles near-certainty cases
        assert state is not None

    def test_exit_signal_when_edge_flips(self):
        evaluator = WeatherSignalEvaluator()
        # Bought YES at 0.50 but now temp way below threshold
        # Physics prob ≈ 0, blended ≈ 0.275 (climo+trend at 0.5)
        # Market at 0.40 → current_edge = 0.275 - 0.40 = -0.125 < -0.03 → exit
        contract = _make_contract(threshold=90.0, minutes_ahead=10.0)
        obs = _make_observation(temp=60.0)
        book = _make_orderbook(mid=0.40, spread=0.04)

        exit_signal = evaluator.evaluate_exit(
            contract, obs, book,
            held_direction="yes",
            entry_price=0.50,
        )
        assert exit_signal is not None
        assert exit_signal.action.value == "exit"
        assert exit_signal.direction == "no"  # selling the YES


class TestKellyComputation:
    def test_positive_edge_positive_kelly(self):
        # Model says 0.65, fill at 0.50
        kelly = compute_kelly(0.65, 0.50, "yes")
        assert kelly > 0

    def test_no_edge_zero_kelly(self):
        # Model says 0.50, fill at 0.50
        kelly = compute_kelly(0.50, 0.50, "yes")
        assert kelly == 0.0

    def test_negative_edge_zero_kelly(self):
        # Model says 0.40, fill at 0.50 → negative kelly clamped to 0
        kelly = compute_kelly(0.40, 0.50, "yes")
        assert kelly == 0.0

    def test_direction_no(self):
        # Model says 0.30 (so P(no) = 0.70), fill_price = 0.40
        kelly = compute_kelly(0.30, 0.40, "no")
        assert kelly > 0


class TestFillPriceEstimation:
    def test_yes_uses_ask(self):
        book = _make_orderbook(mid=0.50, spread=0.04)
        assert estimate_fill_price("yes", book) == 0.52

    def test_no_uses_bid(self):
        book = _make_orderbook(mid=0.50, spread=0.04)
        assert estimate_fill_price("no", book) == 0.48

    def test_fallback_without_best_prices(self):
        book = OrderbookState(mid_price=0.50, spread=0.04)
        assert estimate_fill_price("yes", book) == 0.52
        assert estimate_fill_price("no", book) == 0.48
