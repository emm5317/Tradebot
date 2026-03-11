# DEPRECATED: Crypto evaluation moved to Rust crypto_evaluator.rs (Phase 3/4.4)
"""BE-4.4: Crypto Signal Evaluator.

Evaluates BTC binary option contracts using Black-Scholes pricing
with realized volatility from the Binance feed.

Improvements over spec:
- DB-driven blackout events instead of static JSON
- Uses fill price for Kelly, not mid-price
- Signal cooldown / dedup
- Exit signal support for open positions
"""

from __future__ import annotations

from datetime import UTC, datetime

import structlog

from models.binary_option import compute_binary_probability
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

logger = structlog.get_logger()

# Entry window: tighter than weather (5-15 min)
_MIN_MINUTES = 5.0
_MAX_MINUTES = 15.0

# Higher edge threshold for crypto (more volatile)
_MIN_EDGE = 0.06

# Minimum Kelly fraction
_MIN_KELLY = 0.04

_WIDE_SPREAD_THRESHOLD = 0.10

_SIGNAL_COOLDOWN_SECONDS = 300

# Exit thresholds
_EXIT_EDGE_THRESHOLD = -0.03
_EXIT_MIN_MINUTES = 2.0

# Max staleness for BTC feed
_MAX_BTC_STALENESS_SECONDS = 30


class BlackoutWindow:
    """A time window during which crypto signals are suppressed."""

    def __init__(self, event: str, start: datetime, end: datetime) -> None:
        self.event = event
        self.start = start
        self.end = end

    def is_active(self, at: datetime | None = None) -> bool:
        at = at or datetime.now(UTC)
        return self.start <= at <= self.end


class CryptoSignalEvaluator:
    """Evaluates crypto binary option contracts."""

    def __init__(self, blackout_windows: list[BlackoutWindow] | None = None) -> None:
        self.blackout_windows = blackout_windows or []
        self._recent_signals: dict[str, datetime] = {}

    def set_blackout_windows(self, windows: list[BlackoutWindow]) -> None:
        """Update blackout windows (called after loading from DB)."""
        self.blackout_windows = windows

    def evaluate(
        self,
        contract: Contract,
        spot_price: float,
        realized_vol: float | None,
        btc_last_updated: datetime,
        orderbook: OrderbookState,
        strike: float | None = None,
    ) -> tuple[SignalSchema | None, RejectedSignal | None, ModelState]:
        """Evaluate a crypto contract for entry signal.

        Returns:
            Tuple of (signal_or_none, rejection_or_none, model_state).
        """
        now = datetime.now(UTC)
        minutes = (contract.settlement_time - now).total_seconds() / 60.0

        # Use contract threshold as strike, or explicit override
        contract_strike = strike or contract.threshold

        model_state = ModelState(
            ticker=contract.ticker,
            signal_type="crypto",
            market_price=orderbook.mid_price,
            spread=orderbook.spread,
            minutes_remaining=minutes,
        )

        # 1. Check time window
        if not (_MIN_MINUTES <= minutes <= _MAX_MINUTES):
            return None, None, model_state

        # 2. Check blackout windows
        active_blackout = self._check_blackout(now)
        if active_blackout:
            rejection = RejectedSignal(
                ticker=contract.ticker,
                signal_type="crypto",
                rejection_reason=f"blackout ({active_blackout.event})",
                minutes_remaining=minutes,
                market_price=orderbook.mid_price,
            )
            model_state.rejection_reason = f"blackout ({active_blackout.event})"
            return None, rejection, model_state

        # 3. Check BTC feed freshness
        btc_staleness = (now - btc_last_updated).total_seconds()
        if btc_staleness > _MAX_BTC_STALENESS_SECONDS:
            rejection = RejectedSignal(
                ticker=contract.ticker,
                signal_type="crypto",
                rejection_reason=f"btc_feed_stale ({btc_staleness:.0f}s)",
                minutes_remaining=minutes,
            )
            model_state.rejection_reason = "btc_feed_stale"
            return None, rejection, model_state

        # 4. Check we have volatility data
        if realized_vol is None or realized_vol <= 0:
            rejection = RejectedSignal(
                ticker=contract.ticker,
                signal_type="crypto",
                rejection_reason="missing_volatility",
                minutes_remaining=minutes,
            )
            model_state.rejection_reason = "missing_volatility"
            return None, rejection, model_state

        # 5. Check strike
        if contract_strike is None or contract_strike <= 0:
            rejection = RejectedSignal(
                ticker=contract.ticker,
                signal_type="crypto",
                rejection_reason="missing_strike",
                minutes_remaining=minutes,
            )
            model_state.rejection_reason = "missing_strike"
            return None, rejection, model_state

        # 6. Check signal cooldown
        last_signal = self._recent_signals.get(contract.ticker)
        if last_signal is not None:
            elapsed = (now - last_signal).total_seconds()
            if elapsed < _SIGNAL_COOLDOWN_SECONDS:
                rejection = RejectedSignal(
                    ticker=contract.ticker,
                    signal_type="crypto",
                    rejection_reason=f"cooldown ({int(elapsed)}s/{_SIGNAL_COOLDOWN_SECONDS}s)",
                    minutes_remaining=minutes,
                )
                model_state.rejection_reason = "cooldown"
                return None, rejection, model_state

        # 7. Compute model probability
        model_prob = compute_binary_probability(
            spot=spot_price,
            strike=contract_strike,
            minutes_remaining=minutes,
            sigma_annual=realized_vol,
        )

        model_state.model_prob = model_prob
        model_state.physics_prob = model_prob  # single model, no ensemble

        # 8. Determine direction and raw edge
        market_price = orderbook.mid_price
        direction, raw_edge = determine_direction(model_prob, market_price)

        model_state.direction = direction
        model_state.edge = raw_edge

        # 9. Spread-adjusted edge
        effective_edge = compute_effective_edge(raw_edge, orderbook.spread, _WIDE_SPREAD_THRESHOLD)

        if effective_edge < _MIN_EDGE:
            rejection = RejectedSignal(
                ticker=contract.ticker,
                signal_type="crypto",
                rejection_reason=f"insufficient_edge ({effective_edge:.3f} < {_MIN_EDGE})",
                model_prob=model_prob,
                market_price=market_price,
                edge=effective_edge,
                minutes_remaining=minutes,
            )
            model_state.rejection_reason = "insufficient_edge"
            return None, rejection, model_state

        # 10. Kelly using fill price
        fill_price = estimate_fill_price(direction, orderbook)
        kelly = compute_kelly(model_prob, fill_price, direction)

        if kelly < _MIN_KELLY:
            rejection = RejectedSignal(
                ticker=contract.ticker,
                signal_type="crypto",
                rejection_reason=f"kelly_too_low ({kelly:.3f} < {_MIN_KELLY})",
                model_prob=model_prob,
                market_price=market_price,
                edge=effective_edge,
                minutes_remaining=minutes,
            )
            model_state.rejection_reason = "kelly_too_low"
            return None, rejection, model_state

        # 11. Emit signal
        self._recent_signals[contract.ticker] = now

        signal = SignalSchema(
            ticker=contract.ticker,
            signal_type="crypto",
            action=SignalAction.ENTRY,
            direction=direction,
            model_prob=model_prob,
            market_price=market_price,
            edge=effective_edge,
            kelly_fraction=kelly,
            minutes_remaining=minutes,
            spread=orderbook.spread,
            order_imbalance=orderbook.imbalance,
        )

        logger.info(
            "crypto_signal",
            ticker=contract.ticker,
            direction=direction,
            edge=f"{effective_edge:.3f}",
            kelly=f"{kelly:.3f}",
            model_prob=f"{model_prob:.3f}",
            spot=f"{spot_price:.0f}",
            strike=f"{contract_strike:.0f}",
            vol=f"{realized_vol:.3f}",
        )

        return signal, None, model_state

    def evaluate_exit(
        self,
        contract: Contract,
        spot_price: float,
        realized_vol: float | None,
        btc_last_updated: datetime,
        orderbook: OrderbookState,
        held_direction: str,
        entry_price: float,
        strike: float | None = None,
    ) -> SignalSchema | None:
        """Re-evaluate open crypto position for exit."""
        now = datetime.now(UTC)
        minutes = (contract.settlement_time - now).total_seconds() / 60.0

        if minutes < _EXIT_MIN_MINUTES:
            return None

        if realized_vol is None or realized_vol <= 0:
            return None

        btc_staleness = (now - btc_last_updated).total_seconds()
        if btc_staleness > _MAX_BTC_STALENESS_SECONDS:
            return None

        contract_strike = strike or contract.threshold
        if contract_strike is None or contract_strike <= 0:
            return None

        model_prob = compute_binary_probability(
            spot=spot_price,
            strike=contract_strike,
            minutes_remaining=minutes,
            sigma_annual=realized_vol,
        )

        market_price = orderbook.mid_price

        if held_direction == "yes":
            current_edge = model_prob - market_price
        else:
            current_edge = market_price - model_prob

        if current_edge < _EXIT_EDGE_THRESHOLD:
            exit_direction = "no" if held_direction == "yes" else "yes"

            logger.info(
                "crypto_exit_signal",
                ticker=contract.ticker,
                held=held_direction,
                edge=f"{current_edge:.3f}",
            )

            return SignalSchema(
                ticker=contract.ticker,
                signal_type="crypto",
                action=SignalAction.EXIT,
                direction=exit_direction,
                model_prob=model_prob,
                market_price=market_price,
                edge=abs(current_edge),
                kelly_fraction=0.0,
                minutes_remaining=minutes,
                spread=orderbook.spread,
                order_imbalance=orderbook.imbalance,
            )

        return None

    def clear_cooldown(self, ticker: str) -> None:
        self._recent_signals.pop(ticker, None)

    def _check_blackout(self, at: datetime) -> BlackoutWindow | None:
        for w in self.blackout_windows:
            if w.is_active(at):
                return w
        return None


async def load_blackout_windows(pool) -> list[BlackoutWindow]:
    """Load blackout windows from database.

    Expects a table: blackout_events(event TEXT, start_time TIMESTAMPTZ, end_time TIMESTAMPTZ).
    Falls back to empty list if table doesn't exist.
    """
    try:
        async with pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT event, start_time, end_time
                FROM blackout_events
                WHERE end_time > now()
                ORDER BY start_time
                """
            )
        windows = [
            BlackoutWindow(
                event=row["event"],
                start=row["start_time"],
                end=row["end_time"],
            )
            for row in rows
        ]
        logger.info("blackout_windows_loaded", count=len(windows))
        return windows
    except Exception:
        logger.warning("blackout_table_missing", msg="no blackout_events table, using empty list")
        return []
