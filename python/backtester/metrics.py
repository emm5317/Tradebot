"""Advanced backtesting metrics.

Computes log-loss, Sharpe ratio, Sortino ratio, max drawdown,
profit factor, ECE, streak stats, and time-decay weighted scoring.
"""

from __future__ import annotations

import math
from collections import defaultdict
from dataclasses import dataclass, field
from datetime import date


@dataclass
class TradeRecord:
    """A single trade for metric computation."""

    settlement_date: date
    direction: str          # "yes" or "no"
    model_prob: float       # raw probability (YES direction)
    market_price: float     # entry price
    edge: float             # |model_prob - market_price|
    settled_yes: bool       # actual outcome
    pnl_cents: float        # P&L in cents (after fees)
    fee_cents: float = 0.0  # fee paid


@dataclass
class AdvancedMetrics:
    """Full metric suite from a backtest run."""

    # Core
    accuracy: float = 0.0
    brier_score: float = 0.0
    simulated_pnl_cents: float = 0.0
    # New
    log_loss: float = 0.0
    sharpe_ratio: float = 0.0
    sortino_ratio: float = 0.0
    max_drawdown_cents: float = 0.0
    max_drawdown_pct: float = 0.0
    profit_factor: float = 0.0
    expected_calibration_error: float = 0.0
    win_streak: int = 0
    loss_streak: int = 0
    total_fees_cents: float = 0.0
    # Counts
    win_count: int = 0
    loss_count: int = 0
    total_signals: int = 0


def compute_advanced_metrics(
    trades: list[TradeRecord],
    time_decay_lambda: float = 0.0,
) -> AdvancedMetrics:
    """Compute all metrics from a list of trade records.

    Args:
        trades: List of TradeRecord objects, assumed sorted by settlement_date.
        time_decay_lambda: Exponential decay rate per day. 0.0 = no decay.

    Returns:
        AdvancedMetrics with all fields populated.
    """
    if not trades:
        return AdvancedMetrics()

    m = AdvancedMetrics(total_signals=len(trades))

    # Reference date for time decay (most recent trade)
    max_date = max(t.settlement_date for t in trades)

    # Accumulators
    brier_sum = 0.0
    log_loss_sum = 0.0
    weight_sum = 0.0
    correct = 0
    gross_profit = 0.0
    gross_loss = 0.0
    total_fees = 0.0

    # For streaks
    current_streak = 0
    current_streak_type: str | None = None
    best_win_streak = 0
    best_loss_streak = 0

    # Calibration buckets: 10 buckets of width 0.1
    cal_buckets: dict[int, list[tuple[float, float]]] = defaultdict(list)

    # Daily P&L for Sharpe/Sortino
    daily_pnl: dict[date, float] = defaultdict(float)

    for trade in trades:
        # Time-decay weight
        days_ago = (max_date - trade.settlement_date).days
        w = math.exp(-time_decay_lambda * days_ago) if time_decay_lambda > 0 else 1.0
        weight_sum += w

        # Directional probability
        if trade.direction == "yes":
            p = trade.model_prob
            won = trade.settled_yes
        else:
            p = 1.0 - trade.model_prob
            won = not trade.settled_yes

        outcome = 1.0 if trade.settled_yes else 0.0

        # Brier (weighted)
        brier_sum += w * (p - outcome) ** 2

        # Log-loss (weighted), clamp to avoid log(0)
        eps = 1e-15
        p_clamped = max(eps, min(1.0 - eps, p))
        log_loss_sum += w * -(outcome * math.log(p_clamped) + (1.0 - outcome) * math.log(1.0 - p_clamped))

        # Win/loss
        if won:
            correct += 1
            m.win_count += 1
            gross_profit += trade.pnl_cents
        else:
            m.loss_count += 1
            gross_loss += abs(trade.pnl_cents)

        total_fees += trade.fee_cents

        # Streaks
        outcome_type = "win" if won else "loss"
        if outcome_type == current_streak_type:
            current_streak += 1
        else:
            current_streak = 1
            current_streak_type = outcome_type
        if outcome_type == "win":
            best_win_streak = max(best_win_streak, current_streak)
        else:
            best_loss_streak = max(best_loss_streak, current_streak)

        # Calibration bucket
        bucket_idx = min(int(p * 10), 9)
        cal_buckets[bucket_idx].append((p, 1.0 if won else 0.0))

        # Daily P&L
        daily_pnl[trade.settlement_date] += trade.pnl_cents

    # Finalize core metrics
    if weight_sum > 0:
        m.brier_score = brier_sum / weight_sum
        m.log_loss = log_loss_sum / weight_sum
    m.accuracy = correct / len(trades)
    m.simulated_pnl_cents = sum(t.pnl_cents for t in trades)
    m.total_fees_cents = total_fees
    m.win_streak = best_win_streak
    m.loss_streak = best_loss_streak
    m.profit_factor = gross_profit / gross_loss if gross_loss > 0 else float("inf")

    # ECE: expected calibration error
    ece_sum = 0.0
    ece_total = 0
    for bucket_entries in cal_buckets.values():
        if not bucket_entries:
            continue
        avg_pred = sum(p for p, _ in bucket_entries) / len(bucket_entries)
        avg_actual = sum(o for _, o in bucket_entries) / len(bucket_entries)
        ece_sum += len(bucket_entries) * abs(avg_pred - avg_actual)
        ece_total += len(bucket_entries)
    m.expected_calibration_error = ece_sum / ece_total if ece_total > 0 else 0.0

    # Sharpe and Sortino from daily P&L
    if len(daily_pnl) >= 2:
        daily_returns = list(daily_pnl.values())
        mean_ret = sum(daily_returns) / len(daily_returns)
        variance = sum((r - mean_ret) ** 2 for r in daily_returns) / (len(daily_returns) - 1)
        std_ret = math.sqrt(variance) if variance > 0 else 0.0

        m.sharpe_ratio = (mean_ret / std_ret * math.sqrt(252)) if std_ret > 0 else 0.0

        # Sortino: only downside deviation
        downside = [min(0.0, r - mean_ret) for r in daily_returns]
        downside_var = sum(d ** 2 for d in downside) / (len(daily_returns) - 1)
        downside_std = math.sqrt(downside_var) if downside_var > 0 else 0.0
        m.sortino_ratio = (mean_ret / downside_std * math.sqrt(252)) if downside_std > 0 else 0.0

    # Max drawdown from cumulative P&L
    cum_pnl = 0.0
    peak = 0.0
    max_dd = 0.0
    for d in sorted(daily_pnl.keys()):
        cum_pnl += daily_pnl[d]
        peak = max(peak, cum_pnl)
        dd = peak - cum_pnl
        max_dd = max(max_dd, dd)

    m.max_drawdown_cents = max_dd
    m.max_drawdown_pct = (max_dd / peak * 100) if peak > 0 else 0.0

    return m
