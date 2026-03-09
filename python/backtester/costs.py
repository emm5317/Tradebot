"""Transaction cost modeling for backtesting.

Kalshi uses three fee models per series:
- quadratic: fee = fee_multiplier × price × (1 - price) × count
- quadratic_with_maker_fees: separate maker/taker quadratic fees
- flat: fixed fee per contract

Settlement is free on Kalshi; only entry has fees.
"""

from __future__ import annotations

from dataclasses import dataclass


@dataclass
class FeeModel:
    """Kalshi fee model for backtesting."""

    fee_type: str = "quadratic"
    taker_fee_multiplier: float = 0.07   # 7% quadratic multiplier (Kalshi default)
    maker_fee_multiplier: float = 0.035  # 3.5% maker discount
    flat_fee_cents: int = 2
    assume_taker: bool = True            # conservative: assume we take liquidity

    def compute_fee(self, price: float, count: int = 1) -> float:
        """Compute fee in cents for a trade.

        Args:
            price: Contract price in [0, 1].
            count: Number of contracts.

        Returns:
            Fee in cents.
        """
        if self.fee_type == "flat":
            return self.flat_fee_cents * count

        multiplier = (
            self.taker_fee_multiplier if self.assume_taker
            else self.maker_fee_multiplier
        )
        # Quadratic: multiplier × price × (1 - price) × count × 100 (cents)
        return multiplier * price * (1.0 - price) * count * 100

    def round_trip_cost(self, entry_price: float, count: int = 1) -> float:
        """Total fees for entry + settlement.

        Settlement is free on Kalshi, so only entry has fees.
        """
        return self.compute_fee(entry_price, count)
