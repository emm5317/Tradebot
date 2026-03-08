"""BE-4.3: Weather Signal Evaluator.

Evaluates weather contracts for tradeable edge using the ensemble model.
Improvements over spec:
- Uses estimated fill price for Kelly, not mid-price
- Tracks rejections for UI visibility
- Signal cooldown prevents duplicate signals
- Supports exit signals for open positions
- Exposes ModelState for real-time UI display
"""

from __future__ import annotations

from datetime import datetime, timezone
from typing import TYPE_CHECKING

import structlog

from models.physics import compute_ensemble_probability
from signals.types import (
    Contract,
    ModelState,
    OrderbookState,
    RejectedSignal,
    SignalAction,
    SignalSchema,
)
from signals.utils import (
    compute_effective_edge,
    compute_kelly,
    determine_direction,
    estimate_fill_price,
)

if TYPE_CHECKING:
    from data.mesonet import ASOSObservation

logger = structlog.get_logger()

# Entry window: only evaluate contracts 8-18 minutes from settlement
_MIN_MINUTES = 8.0
_MAX_MINUTES = 18.0

# Minimum edge after spread adjustment
_MIN_EDGE = 0.05

# Minimum Kelly fraction
_MIN_KELLY = 0.04

# Wide spread penalty threshold
_WIDE_SPREAD_THRESHOLD = 0.10

# Cooldown: ignore ticker if signaled within this many seconds
_SIGNAL_COOLDOWN_SECONDS = 300  # 5 minutes

# Edge threshold for exiting a position (negative = edge flipped)
_EXIT_EDGE_THRESHOLD = -0.03
_EXIT_MIN_MINUTES = 3.0


class WeatherSignalEvaluator:
    """Evaluates weather contracts for entry and exit signals."""

    def __init__(
        self,
        sigma_table: dict[tuple[str, int, int], float] | None = None,
        climo_table: dict[tuple[str, int, int], tuple[float, float]] | None = None,
        ensemble_weights: tuple[float, float, float] | None = None,
        station_calibration: dict | None = None,
    ) -> None:
        self.sigma_table = sigma_table
        self.climo_table = climo_table
        self.ensemble_weights = ensemble_weights
        self.station_calibration = station_calibration or {}
        self._recent_signals: dict[str, datetime] = {}

    def evaluate(
        self,
        contract: Contract,
        observation: ASOSObservation,
        orderbook: OrderbookState,
        recent_temps: list[float] | None = None,
    ) -> tuple[SignalSchema | None, RejectedSignal | None, ModelState]:
        """Evaluate a weather contract for entry signal.

        Returns:
            Tuple of (signal_or_none, rejection_or_none, model_state).
            Exactly one of signal/rejection will be non-None (or both None
            if outside time window).
        """
        now = datetime.now(timezone.utc)
        minutes = (contract.settlement_time - now).total_seconds() / 60.0

        # Build model state (always returned for UI)
        model_state = ModelState(
            ticker=contract.ticker,
            signal_type="weather",
            market_price=orderbook.mid_price,
            spread=orderbook.spread,
            minutes_remaining=minutes,
        )

        # 1. Check time window
        if not (_MIN_MINUTES <= minutes <= _MAX_MINUTES):
            return None, None, model_state

        # 2. Check observation freshness
        if observation.is_stale:
            rejection = RejectedSignal(
                ticker=contract.ticker,
                signal_type="weather",
                rejection_reason="stale_observation",
                minutes_remaining=minutes,
                market_price=orderbook.mid_price,
            )
            model_state.rejection_reason = "stale_observation"
            return None, rejection, model_state

        # 3. Check required fields
        if observation.temperature_f is None:
            rejection = RejectedSignal(
                ticker=contract.ticker,
                signal_type="weather",
                rejection_reason="missing_temperature",
                minutes_remaining=minutes,
            )
            model_state.rejection_reason = "missing_temperature"
            return None, rejection, model_state

        if contract.threshold is None:
            rejection = RejectedSignal(
                ticker=contract.ticker,
                signal_type="weather",
                rejection_reason="missing_threshold",
                minutes_remaining=minutes,
            )
            model_state.rejection_reason = "missing_threshold"
            return None, rejection, model_state

        # 4. Check signal cooldown
        last_signal = self._recent_signals.get(contract.ticker)
        if last_signal is not None:
            elapsed = (now - last_signal).total_seconds()
            if elapsed < _SIGNAL_COOLDOWN_SECONDS:
                rejection = RejectedSignal(
                    ticker=contract.ticker,
                    signal_type="weather",
                    rejection_reason=f"cooldown ({int(elapsed)}s/{_SIGNAL_COOLDOWN_SECONDS}s)",
                    minutes_remaining=minutes,
                )
                model_state.rejection_reason = "cooldown"
                return None, rejection, model_state

        # 5. Compute ensemble probability
        hour = now.hour
        month = now.month
        station = contract.station or "KORD"

        p_ensemble, p_physics, p_climo, p_trend = compute_ensemble_probability(
            current_temp_f=observation.temperature_f,
            threshold_f=contract.threshold,
            minutes_remaining=minutes,
            station=station,
            hour=hour,
            month=month,
            recent_temps=recent_temps,
            sigma_table=self.sigma_table,
            climo_table=self.climo_table,
            weights=self.ensemble_weights,
        )

        # Update model state with probabilities
        model_state.model_prob = p_ensemble
        model_state.physics_prob = p_physics
        model_state.climo_prob = p_climo
        model_state.trend_prob = p_trend

        # 6. Determine direction and raw edge
        market_price = orderbook.mid_price
        direction, raw_edge = determine_direction(p_ensemble, market_price)

        model_state.direction = direction
        model_state.edge = raw_edge

        # 7. Spread-adjusted edge
        effective_edge = compute_effective_edge(
            raw_edge, orderbook.spread, _WIDE_SPREAD_THRESHOLD
        )

        if effective_edge < _MIN_EDGE:
            rejection = RejectedSignal(
                ticker=contract.ticker,
                signal_type="weather",
                rejection_reason=f"insufficient_edge ({effective_edge:.3f} < {_MIN_EDGE})",
                model_prob=p_ensemble,
                market_price=market_price,
                edge=effective_edge,
                minutes_remaining=minutes,
            )
            model_state.rejection_reason = "insufficient_edge"
            return None, rejection, model_state

        # 8. Kelly sizing using estimated fill price, not mid-price
        fill_price = estimate_fill_price(direction, orderbook)
        kelly = compute_kelly(p_ensemble, fill_price, direction)

        if kelly < _MIN_KELLY:
            rejection = RejectedSignal(
                ticker=contract.ticker,
                signal_type="weather",
                rejection_reason=f"kelly_too_low ({kelly:.3f} < {_MIN_KELLY})",
                model_prob=p_ensemble,
                market_price=market_price,
                edge=effective_edge,
                minutes_remaining=minutes,
            )
            model_state.rejection_reason = "kelly_too_low"
            return None, rejection, model_state

        # 9. Signal passes all filters — record cooldown and emit
        self._recent_signals[contract.ticker] = now

        signal = SignalSchema(
            ticker=contract.ticker,
            signal_type="weather",
            action=SignalAction.ENTRY,
            direction=direction,
            model_prob=p_ensemble,
            market_price=market_price,
            edge=effective_edge,
            kelly_fraction=kelly,
            minutes_remaining=minutes,
            spread=orderbook.spread,
            order_imbalance=orderbook.imbalance,
        )

        logger.info(
            "weather_signal",
            ticker=contract.ticker,
            direction=direction,
            edge=f"{effective_edge:.3f}",
            kelly=f"{kelly:.3f}",
            model_prob=f"{p_ensemble:.3f}",
            market=f"{market_price:.3f}",
        )

        return signal, None, model_state

    def evaluate_exit(
        self,
        contract: Contract,
        observation: ASOSObservation,
        orderbook: OrderbookState,
        held_direction: str,
        entry_price: float,
        recent_temps: list[float] | None = None,
    ) -> SignalSchema | None:
        """Re-evaluate an open position for exit.

        Returns an EXIT signal if edge has flipped against the held position.
        """
        now = datetime.now(timezone.utc)
        minutes = (contract.settlement_time - now).total_seconds() / 60.0

        if minutes < _EXIT_MIN_MINUTES:
            return None  # too close to settlement, let it ride

        if observation.is_stale or observation.temperature_f is None:
            return None

        if contract.threshold is None:
            return None

        station = contract.station or "KORD"
        p_ensemble, _, _, _ = compute_ensemble_probability(
            current_temp_f=observation.temperature_f,
            threshold_f=contract.threshold,
            minutes_remaining=minutes,
            station=station,
            hour=now.hour,
            month=now.month,
            recent_temps=recent_temps,
            sigma_table=self.sigma_table,
            climo_table=self.climo_table,
            weights=self.ensemble_weights,
        )

        market_price = orderbook.mid_price

        # Check if edge has flipped against our position
        if held_direction == "yes":
            current_edge = p_ensemble - market_price
        else:
            current_edge = market_price - p_ensemble

        if current_edge < _EXIT_EDGE_THRESHOLD:
            exit_direction = "no" if held_direction == "yes" else "yes"

            logger.info(
                "weather_exit_signal",
                ticker=contract.ticker,
                held=held_direction,
                edge=f"{current_edge:.3f}",
                model_prob=f"{p_ensemble:.3f}",
            )

            return SignalSchema(
                ticker=contract.ticker,
                signal_type="weather",
                action=SignalAction.EXIT,
                direction=exit_direction,
                model_prob=p_ensemble,
                market_price=market_price,
                edge=abs(current_edge),
                kelly_fraction=0.0,  # full exit
                minutes_remaining=minutes,
                spread=orderbook.spread,
                order_imbalance=orderbook.imbalance,
            )

        return None

    def clear_cooldown(self, ticker: str) -> None:
        """Clear cooldown for a ticker (e.g., after position closed)."""
        self._recent_signals.pop(ticker, None)


