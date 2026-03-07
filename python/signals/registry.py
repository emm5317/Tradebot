"""Evaluator plugin registry for extensible market support.

Evaluators register by signal_type and are auto-discovered by the
orchestration loop. Adding a new market type only requires implementing
the BaseEvaluator protocol and calling registry.register().
"""

from __future__ import annotations

from typing import Any, Protocol, runtime_checkable

import structlog

from signals.types import ModelState, RejectedSignal, SignalSchema

logger = structlog.get_logger()


@runtime_checkable
class BaseEvaluator(Protocol):
    """Protocol that all signal evaluators must implement."""

    def evaluate(
        self,
        contract: Any,
        orderbook: Any,
        **kwargs: Any,
    ) -> tuple[SignalSchema | None, RejectedSignal | None, ModelState]: ...

    def evaluate_exit(
        self,
        contract: Any,
        orderbook: Any,
        held_direction: str,
        entry_price: float,
        **kwargs: Any,
    ) -> SignalSchema | None: ...


class EvaluatorRegistry:
    """Registry of signal evaluators keyed by signal_type.

    Usage:
        registry = EvaluatorRegistry()
        registry.register("weather", weather_evaluator)
        registry.register("crypto", crypto_evaluator)

        # Orchestration loop iterates all evaluators
        for signal_type, evaluator in registry.all().items():
            ...
    """

    def __init__(self) -> None:
        self._evaluators: dict[str, BaseEvaluator] = {}

    def register(self, signal_type: str, evaluator: BaseEvaluator) -> None:
        """Register an evaluator for a signal type."""
        self._evaluators[signal_type] = evaluator
        logger.info("evaluator_registered", signal_type=signal_type)

    def get(self, signal_type: str) -> BaseEvaluator | None:
        """Look up an evaluator by signal type."""
        return self._evaluators.get(signal_type)

    def all(self) -> dict[str, BaseEvaluator]:
        """Return all registered evaluators."""
        return dict(self._evaluators)

    def types(self) -> list[str]:
        """Return all registered signal types."""
        return list(self._evaluators.keys())
