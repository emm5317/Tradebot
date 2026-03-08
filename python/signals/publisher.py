"""BE-4.5: Signal Publisher — NATS + DB dual write.

Publishes validated signals to NATS (latency-sensitive, triggers execution)
then persists to DB asynchronously (audit trail). Also writes model state
to Redis for UI consumption and records rejected signals.

Improvements over spec:
- NATS publish fires first (latency-critical path)
- DB write is fire-and-forget via asyncio.create_task
- Separate NATS subjects for actionable vs. all evaluations (UI)
- Model state written to Redis for real-time dashboard
- Rejected signals persisted for UI visibility
"""

from __future__ import annotations

import asyncio
import json
from typing import TYPE_CHECKING

import structlog

from signals.types import ModelState, RejectedSignal, SignalSchema

if TYPE_CHECKING:
    import asyncpg
    import nats

logger = structlog.get_logger()

# NATS subjects
_SUBJECT_SIGNALS = "tradebot.signals"  # actionable signals only
_SUBJECT_SIGNALS_LIVE = "tradebot.signals.live"  # all evaluations (for UI)

# Redis key prefix for model state
_REDIS_MODEL_STATE_PREFIX = "model_state:"
_REDIS_MODEL_STATE_TTL = 120  # seconds


class SignalPublisher:
    """Publishes signals to NATS and persists to database."""

    def __init__(
        self,
        nats_client: nats.NATS | None = None,
        db_pool: asyncpg.Pool | None = None,
        redis_client=None,
    ) -> None:
        self.nats = nats_client
        self.db_pool = db_pool
        self.redis = redis_client

    async def publish(self, signal: SignalSchema) -> None:
        """Publish a validated signal.

        Order of operations (latency-optimized):
        1. NATS publish (triggers Rust execution consumer)
        2. DB persist (async, non-blocking)
        3. Redis model state update (async, non-blocking)
        """
        payload = signal.model_dump_json().encode()

        # 1. NATS — latency-critical, awaited
        if self.nats is not None:
            try:
                await self.nats.publish(_SUBJECT_SIGNALS, payload)
                await self.nats.publish(_SUBJECT_SIGNALS_LIVE, payload)
            except Exception:
                logger.exception("nats_publish_failed", ticker=signal.ticker)

        # 2. DB persist — fire and forget
        if self.db_pool is not None:
            asyncio.create_task(self._persist_signal(signal))

        logger.info(
            "signal_published",
            ticker=signal.ticker,
            direction=signal.direction,
            action=signal.action.value,
            edge=f"{signal.edge:.3f}",
        )

    async def publish_rejection(self, rejection: RejectedSignal) -> None:
        """Record a rejected signal for UI visibility.

        Published to the live NATS subject (UI-only) and persisted to DB.
        """
        payload = rejection.model_dump_json().encode()

        # NATS live stream (UI only)
        if self.nats is not None:
            try:
                await self.nats.publish(_SUBJECT_SIGNALS_LIVE, payload)
            except Exception:
                logger.exception("nats_rejection_publish_failed", ticker=rejection.ticker)

        # DB persist — fire and forget
        if self.db_pool is not None:
            asyncio.create_task(self._persist_rejection(rejection))

    async def publish_model_state(self, state: ModelState) -> None:
        """Write model state to Redis for real-time UI display."""
        if self.redis is None:
            return

        key = f"{_REDIS_MODEL_STATE_PREFIX}{state.ticker}"
        try:
            await self.redis.set(
                key,
                state.model_dump_json(),
                ex=_REDIS_MODEL_STATE_TTL,
            )
        except Exception:
            logger.exception("redis_model_state_failed", ticker=state.ticker)

    async def publish_model_evaluation(
        self,
        ticker: str,
        signal_type: str,
        model_prob: float | None,
        market_price: float | None,
        edge: float | None,
        direction: str | None,
        inputs: dict | None = None,
        components: dict | None = None,
        confidence: float | None = None,
        acted_on: bool = False,
    ) -> None:
        """Persist full model evaluation snapshot for replay and attribution.

        Called every evaluation cycle for every contract, not just signals.
        """
        if self.db_pool is None:
            return

        asyncio.create_task(
            self._persist_evaluation(
                ticker, signal_type, model_prob, market_price,
                edge, direction, inputs, components, confidence, acted_on,
            )
        )

    async def _persist_evaluation(
        self,
        ticker: str,
        signal_type: str,
        model_prob: float | None,
        market_price: float | None,
        edge: float | None,
        direction: str | None,
        inputs: dict | None,
        components: dict | None,
        confidence: float | None,
        acted_on: bool,
    ) -> None:
        """Insert into model_evaluations table."""
        try:
            async with self.db_pool.acquire() as conn:
                await conn.execute(
                    """
                    INSERT INTO model_evaluations (
                        ticker, signal_type, model_prob, market_price,
                        edge, direction, inputs, components,
                        confidence, acted_on
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                    """,
                    ticker,
                    signal_type,
                    model_prob,
                    market_price,
                    edge,
                    direction,
                    json.dumps(inputs) if inputs else None,
                    json.dumps(components) if components else None,
                    confidence,
                    acted_on,
                )
        except Exception:
            logger.debug("evaluation_persist_skipped", ticker=ticker)

    async def _persist_signal(self, signal: SignalSchema) -> None:
        """Insert signal into signals table."""
        try:
            async with self.db_pool.acquire() as conn:
                await conn.execute(
                    """
                    INSERT INTO signals (
                        ticker, signal_type, direction, model_prob, market_price,
                        edge, kelly_fraction, minutes_remaining,
                        observation_data, acted_on
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                    """,
                    signal.ticker,
                    signal.signal_type,
                    signal.direction,
                    signal.model_prob,
                    signal.market_price,
                    signal.edge,
                    signal.kelly_fraction,
                    signal.minutes_remaining,
                    json.dumps({
                        "spread": signal.spread,
                        "order_imbalance": signal.order_imbalance,
                        "action": signal.action.value,
                    }),
                    True,  # acted_on
                )
        except Exception:
            logger.exception("signal_persist_failed", ticker=signal.ticker)

    async def _persist_rejection(self, rejection: RejectedSignal) -> None:
        """Insert rejected signal into signals table with acted_on=false."""
        try:
            async with self.db_pool.acquire() as conn:
                await conn.execute(
                    """
                    INSERT INTO signals (
                        ticker, signal_type, direction, model_prob, market_price,
                        edge, kelly_fraction, minutes_remaining,
                        acted_on, rejection_reason
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                    """,
                    rejection.ticker,
                    rejection.signal_type,
                    "yes",  # placeholder direction for rejections
                    rejection.model_prob or 0.0,
                    rejection.market_price or 0.0,
                    rejection.edge or 0.0,
                    0.0,  # kelly_fraction
                    rejection.minutes_remaining or 0.0,
                    False,  # acted_on
                    rejection.rejection_reason,
                )
        except Exception:
            logger.exception("rejection_persist_failed", ticker=rejection.ticker)
