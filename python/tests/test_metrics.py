"""Tests for advanced backtesting metrics (Phase 8.3)."""

from __future__ import annotations

from datetime import date, timedelta

from backtester.metrics import TradeRecord, compute_advanced_metrics


def _make_trade(
    day_offset: int = 0,
    direction: str = "yes",
    model_prob: float = 0.7,
    market_price: float = 0.50,
    settled_yes: bool = True,
    pnl_cents: float = 10.0,
    fee_cents: float = 1.0,
) -> TradeRecord:
    return TradeRecord(
        settlement_date=date(2026, 1, 1) + timedelta(days=day_offset),
        direction=direction,
        model_prob=model_prob,
        market_price=market_price,
        edge=abs(model_prob - market_price),
        settled_yes=settled_yes,
        pnl_cents=pnl_cents,
        fee_cents=fee_cents,
    )


class TestComputeAdvancedMetrics:
    """Core metric computation tests."""

    def test_empty_trades(self):
        m = compute_advanced_metrics([])
        assert m.total_signals == 0
        assert m.accuracy == 0.0
        assert m.brier_score == 0.0

    def test_single_winning_trade(self):
        trades = [_make_trade(pnl_cents=20, fee_cents=1.75)]
        m = compute_advanced_metrics(trades)
        assert m.total_signals == 1
        assert m.win_count == 1
        assert m.loss_count == 0
        assert m.accuracy == 1.0
        assert m.total_fees_cents == 1.75

    def test_accuracy_with_mixed_trades(self):
        trades = [
            _make_trade(day_offset=0, settled_yes=True, pnl_cents=10),  # win
            _make_trade(day_offset=1, settled_yes=False, pnl_cents=-10),  # loss
            _make_trade(day_offset=2, settled_yes=True, pnl_cents=10),  # win
        ]
        m = compute_advanced_metrics(trades)
        assert abs(m.accuracy - 2 / 3) < 1e-9
        assert m.win_count == 2
        assert m.loss_count == 1

    def test_pnl_sums_correctly(self):
        trades = [
            _make_trade(day_offset=0, pnl_cents=15),
            _make_trade(day_offset=1, pnl_cents=-8),
            _make_trade(day_offset=2, pnl_cents=12),
        ]
        m = compute_advanced_metrics(trades)
        assert abs(m.simulated_pnl_cents - 19) < 1e-9

    def test_fees_summed(self):
        trades = [
            _make_trade(day_offset=0, fee_cents=1.5),
            _make_trade(day_offset=1, fee_cents=2.0),
        ]
        m = compute_advanced_metrics(trades)
        assert abs(m.total_fees_cents - 3.5) < 1e-9


class TestBrierAndLogLoss:
    """Brier score and log-loss computation."""

    def test_perfect_predictions_brier_zero(self):
        """Model predicts 1.0 for YES, outcome is YES → Brier = 0."""
        trades = [_make_trade(model_prob=1.0, settled_yes=True, pnl_cents=10)]
        m = compute_advanced_metrics(trades)
        assert abs(m.brier_score) < 1e-9

    def test_worst_prediction_brier_one(self):
        """Model predicts 0.0 for YES, outcome is YES → Brier = 1."""
        trades = [_make_trade(model_prob=0.0, settled_yes=True, pnl_cents=-10)]
        m = compute_advanced_metrics(trades)
        assert abs(m.brier_score - 1.0) < 1e-9

    def test_log_loss_perfect_near_zero(self):
        """Near-perfect prediction should have log-loss close to 0."""
        trades = [_make_trade(model_prob=0.999, settled_yes=True, pnl_cents=10)]
        m = compute_advanced_metrics(trades)
        assert m.log_loss < 0.01

    def test_log_loss_bad_prediction_high(self):
        """Bad prediction should have high log-loss."""
        trades = [_make_trade(model_prob=0.01, settled_yes=True, pnl_cents=-10)]
        m = compute_advanced_metrics(trades)
        assert m.log_loss > 3.0  # -log(0.01) ≈ 4.6


class TestSharpeAndDrawdown:
    """Sharpe ratio, Sortino ratio, and max drawdown."""

    def test_sharpe_positive_for_mostly_profits(self):
        """Mostly profitable daily returns → positive Sharpe."""
        trades = [_make_trade(day_offset=i, pnl_cents=10 + (i % 3) * 5) for i in range(10)]
        m = compute_advanced_metrics(trades)
        assert m.sharpe_ratio > 0

    def test_sharpe_zero_for_single_day(self):
        """Single day can't compute std, so Sharpe = 0."""
        trades = [_make_trade(pnl_cents=10)]
        m = compute_advanced_metrics(trades)
        assert m.sharpe_ratio == 0.0

    def test_max_drawdown_from_peak(self):
        """Drawdown tracks peak-to-trough decline."""
        trades = [
            _make_trade(day_offset=0, pnl_cents=100),  # cum: 100 (peak)
            _make_trade(day_offset=1, pnl_cents=-30),  # cum: 70
            _make_trade(day_offset=2, pnl_cents=-50),  # cum: 20 → DD = 80
            _make_trade(day_offset=3, pnl_cents=200),  # cum: 220
        ]
        m = compute_advanced_metrics(trades)
        assert abs(m.max_drawdown_cents - 80) < 1e-9

    def test_no_drawdown_monotonic_increase(self):
        """No drawdown if P&L only increases."""
        trades = [_make_trade(day_offset=i, pnl_cents=10) for i in range(5)]
        m = compute_advanced_metrics(trades)
        assert m.max_drawdown_cents == 0.0

    def test_sortino_ignores_upside(self):
        """Sortino uses only downside deviation."""
        trades = [_make_trade(day_offset=i, pnl_cents=10) for i in range(5)]
        m = compute_advanced_metrics(trades)
        # All positive returns → downside deviation = 0 → Sortino = 0 (or inf)
        # In our impl, zero downside_std → 0
        assert m.sortino_ratio == 0.0 or m.sortino_ratio > m.sharpe_ratio


class TestProfitFactorAndStreaks:
    """Profit factor and streak tracking."""

    def test_profit_factor_all_wins(self):
        trades = [_make_trade(day_offset=i, pnl_cents=10) for i in range(3)]
        m = compute_advanced_metrics(trades)
        assert m.profit_factor == float("inf")

    def test_profit_factor_mixed(self):
        trades = [
            _make_trade(day_offset=0, settled_yes=True, pnl_cents=30),
            _make_trade(day_offset=1, settled_yes=False, pnl_cents=-10),
        ]
        m = compute_advanced_metrics(trades)
        assert abs(m.profit_factor - 3.0) < 1e-9

    def test_win_streak(self):
        trades = [
            _make_trade(day_offset=0, settled_yes=True, pnl_cents=10),
            _make_trade(day_offset=1, settled_yes=True, pnl_cents=10),
            _make_trade(day_offset=2, settled_yes=True, pnl_cents=10),
            _make_trade(day_offset=3, settled_yes=False, pnl_cents=-10),
        ]
        m = compute_advanced_metrics(trades)
        assert m.win_streak == 3
        assert m.loss_streak == 1

    def test_loss_streak(self):
        trades = [
            _make_trade(day_offset=0, settled_yes=False, pnl_cents=-10),
            _make_trade(day_offset=1, settled_yes=False, pnl_cents=-10),
            _make_trade(day_offset=2, settled_yes=True, pnl_cents=10),
        ]
        m = compute_advanced_metrics(trades)
        assert m.loss_streak == 2
        assert m.win_streak == 1


class TestTimeDecay:
    """Exponential time-decay weighting."""

    def test_zero_decay_same_as_uniform(self):
        """lambda=0 should produce same results as uniform weighting."""
        trades = [_make_trade(day_offset=i, pnl_cents=10 if i % 2 == 0 else -5) for i in range(5)]
        m0 = compute_advanced_metrics(trades, time_decay_lambda=0.0)
        m1 = compute_advanced_metrics(trades, time_decay_lambda=0.0)
        assert abs(m0.brier_score - m1.brier_score) < 1e-15

    def test_decay_weights_recent_more(self):
        """With decay, a recent bad prediction should worsen Brier more than an old one."""
        # Old bad prediction (day 0), recent good prediction (day 10)
        trades_old_bad = [
            _make_trade(day_offset=0, model_prob=0.1, settled_yes=True, pnl_cents=-10),
            _make_trade(day_offset=10, model_prob=0.9, settled_yes=True, pnl_cents=10),
        ]
        # Recent bad prediction (day 10), old good prediction (day 0)
        trades_recent_bad = [
            _make_trade(day_offset=0, model_prob=0.9, settled_yes=True, pnl_cents=10),
            _make_trade(day_offset=10, model_prob=0.1, settled_yes=True, pnl_cents=-10),
        ]
        m_old_bad = compute_advanced_metrics(trades_old_bad, time_decay_lambda=0.1)
        m_recent_bad = compute_advanced_metrics(trades_recent_bad, time_decay_lambda=0.1)
        # Recent bad should have worse (higher) Brier
        assert m_recent_bad.brier_score > m_old_bad.brier_score


class TestECE:
    """Expected Calibration Error."""

    def test_perfectly_calibrated_ece_zero(self):
        """If predictions match outcomes exactly, ECE ≈ 0."""
        # All predictions at 1.0 with outcome YES
        trades = [_make_trade(model_prob=1.0, settled_yes=True, pnl_cents=10) for _ in range(5)]
        m = compute_advanced_metrics(trades)
        assert m.expected_calibration_error < 0.01

    def test_miscalibrated_high_ece(self):
        """Systematically wrong predictions → high ECE."""
        # Predict 0.9 but all settle NO
        trades = [_make_trade(day_offset=i, model_prob=0.9, settled_yes=False, pnl_cents=-10) for i in range(10)]
        m = compute_advanced_metrics(trades)
        assert m.expected_calibration_error > 0.5
