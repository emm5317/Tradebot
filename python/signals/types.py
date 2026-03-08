"""Shared types for the signal engine."""

from __future__ import annotations

from datetime import datetime, timezone
from enum import Enum
from typing import Any, Literal

from pydantic import BaseModel, Field


class SignalDirection(str, Enum):
    YES = "yes"
    NO = "no"


class SignalAction(str, Enum):
    ENTRY = "entry"
    EXIT = "exit"


class SignalSchema(BaseModel):
    """Validated signal ready for publishing."""

    ticker: str
    signal_type: Literal["weather", "crypto"]
    action: SignalAction = SignalAction.ENTRY
    direction: Literal["yes", "no"]
    model_prob: float = Field(ge=0.0, le=1.0)
    market_price: float = Field(ge=0.0, le=1.0)
    edge: float = Field(ge=0.0)
    kelly_fraction: float = Field(ge=0.0, le=1.0)
    minutes_remaining: float = Field(ge=0.0)
    spread: float = Field(ge=0.0, default=0.0)
    order_imbalance: float = Field(ge=0.0, le=1.0, default=0.5)
    model_components: dict | None = None
    published_at: datetime = Field(default_factory=lambda: datetime.now(timezone.utc))


class RejectedSignal(BaseModel):
    """Lightweight record of why a contract was not traded.

    Written to signals table with acted_on=false for UI visibility.
    """

    ticker: str
    signal_type: Literal["weather", "crypto"]
    rejection_reason: str
    model_prob: float | None = None
    market_price: float | None = None
    edge: float | None = None
    minutes_remaining: float | None = None
    evaluated_at: datetime = Field(default_factory=lambda: datetime.now(timezone.utc))


class ModelState(BaseModel):
    """Snapshot of model internals for UI display.

    Written to Redis keyed by ticker, refreshed every evaluation cycle.
    """

    ticker: str
    signal_type: Literal["weather", "crypto"]
    model_prob: float | None = None
    physics_prob: float | None = None
    climo_prob: float | None = None
    trend_prob: float | None = None
    market_price: float | None = None
    edge: float | None = None
    spread: float | None = None
    direction: str | None = None
    rejection_reason: str | None = None
    minutes_remaining: float | None = None
    updated_at: datetime = Field(default_factory=lambda: datetime.now(timezone.utc))


class OrderbookState(BaseModel):
    """Orderbook snapshot for signal evaluation."""

    mid_price: float
    spread: float
    best_bid: float | None = None
    best_ask: float | None = None
    bid_depth: int = 0
    ask_depth: int = 0
    best_bid_size: int | None = None
    best_ask_size: int | None = None
    last_trade_price: float | None = None
    last_trade_count: int | None = None
    trade_aggr_30s: float = 0.0
    recent_volume_60s: int = 0
    market_status: str | None = None
    volume: int | None = None
    open_interest: int | None = None

    @property
    def imbalance(self) -> float:
        """Bid volume / total volume. >0.5 = buying pressure."""
        total = self.bid_depth + self.ask_depth
        if total == 0:
            return 0.5
        return self.bid_depth / total


class Contract(BaseModel):
    """Contract info needed for signal evaluation."""

    ticker: str
    category: str
    city: str | None = None
    station: str | None = None
    threshold: float | None = None
    settlement_time: datetime
    status: str = "active"

    # Resolved settlement rules (populated by ContractRulesResolver)
    rules: Any | None = None  # rules.resolver.ContractRules

    model_config = {"arbitrary_types_allowed": True}
