"""Tests for crypto threshold sweep and multi-signal mode (Phase 8.1/8.4/8.5)."""

from __future__ import annotations

from datetime import date, datetime, timedelta, timezone

import pytest

from backtester.costs import FeeModel
from backtester.metrics import TradeRecord, compute_advanced_metrics
from backtester.sweep import (
    CRYPTO_THRESHOLD_GRID,
    ParameterSweep,
    SweepResult,
    _generate_combinations,
    _generate_walk_forward_splits,
)


# ── 8.1: Crypto threshold grid ──────────────────────────────────

class TestCryptoThresholdGrid:
    """Verify the crypto threshold parameter grid is correct."""

    def test_grid_has_required_keys(self):
        assert "min_edge" in CRYPTO_THRESHOLD_GRID
        assert "min_confidence" in CRYPTO_THRESHOLD_GRID
        assert "min_kelly" in CRYPTO_THRESHOLD_GRID
        assert "kelly_multiplier" in CRYPTO_THRESHOLD_GRID

    def test_grid_produces_combinations(self):
        combos = _generate_combinations(CRYPTO_THRESHOLD_GRID, 500)
        # 5 × 4 × 4 × 4 = 320
        assert len(combos) == 320

    def test_max_combos_cap(self):
        combos = _generate_combinations(CRYPTO_THRESHOLD_GRID, 50)
        assert len(combos) == 50


class TestEvaluateCryptoThresholds:
    """Test crypto threshold evaluation logic (no DB needed)."""

    def _make_signal(
        self,
        ticker: str = "BTC-Y",
        direction: str = "yes",
        model_prob: float = 0.65,
        market_price: float = 0.50,
        edge: float = 0.15,
        kelly_fraction: float = 0.05,
        settled_yes: bool = True,
        confidence: float = 0.7,
        day_offset: int = 0,
    ) -> dict:
        base = datetime(2026, 1, 1, tzinfo=timezone.utc)
        return {
            "ticker": ticker,
            "signal_type": "crypto",
            "direction": direction,
            "model_prob": model_prob,
            "market_price": market_price,
            "edge": edge,
            "kelly_fraction": kelly_fraction,
            "minutes_remaining": 30.0,
            "created_at": base + timedelta(days=day_offset),
            "settled_yes": settled_yes,
            "settlement_time": base + timedelta(days=day_offset, hours=1),
            "confidence": confidence,
        }

    def _make_sweep(self, multi_signal: bool = False) -> ParameterSweep:
        """Create a ParameterSweep with no DB pool (for threshold eval only)."""
        sweep = object.__new__(ParameterSweep)
        sweep.pool = None
        sweep.fee_model = FeeModel(fee_type="flat", flat_fee_cents=0)
        sweep.time_decay_lambda = 0.0
        sweep.multi_signal = multi_signal
        return sweep

    def test_basic_filtering_by_min_edge(self):
        """Signals below min_edge should be filtered out."""
        sweep = self._make_sweep()
        signals = [
            self._make_signal(edge=0.02),  # below 0.05 threshold
            self._make_signal(ticker="BTC-Y2", edge=0.10, day_offset=1),  # above
        ]
        params = {"min_edge": 0.05, "min_confidence": 0.0, "min_kelly": 0.0, "kelly_multiplier": 1.0}
        result = sweep._evaluate_crypto_thresholds(signals, params)
        assert result.total_signals == 1

    def test_filtering_by_min_confidence(self):
        sweep = self._make_sweep()
        signals = [
            self._make_signal(confidence=0.2),  # below 0.5 threshold
            self._make_signal(ticker="BTC-Y2", confidence=0.8, day_offset=1),
        ]
        params = {"min_edge": 0.0, "min_confidence": 0.5, "min_kelly": 0.0, "kelly_multiplier": 1.0}
        result = sweep._evaluate_crypto_thresholds(signals, params)
        assert result.total_signals == 1

    def test_filtering_by_min_kelly(self):
        sweep = self._make_sweep()
        signals = [
            self._make_signal(kelly_fraction=0.005),  # below
            self._make_signal(ticker="BTC-Y2", kelly_fraction=0.05, day_offset=1),
        ]
        params = {"min_edge": 0.0, "min_confidence": 0.0, "min_kelly": 0.01, "kelly_multiplier": 1.0}
        result = sweep._evaluate_crypto_thresholds(signals, params)
        assert result.total_signals == 1

    def test_kelly_multiplier_scales_pnl(self):
        sweep = self._make_sweep()
        signals = [self._make_signal(kelly_fraction=0.10, settled_yes=True)]

        result_full = sweep._evaluate_crypto_thresholds(
            signals, {"min_edge": 0.0, "min_confidence": 0.0, "min_kelly": 0.0, "kelly_multiplier": 1.0}
        )
        result_half = sweep._evaluate_crypto_thresholds(
            signals, {"min_edge": 0.0, "min_confidence": 0.0, "min_kelly": 0.0, "kelly_multiplier": 0.5}
        )
        # Half Kelly should produce roughly half the P&L
        assert abs(result_half.simulated_pnl_cents) < abs(result_full.simulated_pnl_cents) + 1

    def test_win_loss_tracking(self):
        sweep = self._make_sweep()
        signals = [
            self._make_signal(ticker="A", settled_yes=True, day_offset=0),
            self._make_signal(ticker="B", settled_yes=False, day_offset=1),
            self._make_signal(ticker="C", settled_yes=True, day_offset=2),
        ]
        params = {"min_edge": 0.0, "min_confidence": 0.0, "min_kelly": 0.0, "kelly_multiplier": 1.0}
        result = sweep._evaluate_crypto_thresholds(signals, params)
        assert result.win_count == 2
        assert result.loss_count == 1

    def test_fees_applied(self):
        sweep = self._make_sweep()
        sweep.fee_model = FeeModel()  # quadratic fees
        signals = [self._make_signal()]
        params = {"min_edge": 0.0, "min_confidence": 0.0, "min_kelly": 0.0, "kelly_multiplier": 1.0}
        result = sweep._evaluate_crypto_thresholds(signals, params)
        assert result.fee_total_cents > 0

    def test_brier_score_computed(self):
        sweep = self._make_sweep()
        signals = [self._make_signal(model_prob=0.7, settled_yes=True)]
        params = {"min_edge": 0.0, "min_confidence": 0.0, "min_kelly": 0.0, "kelly_multiplier": 1.0}
        result = sweep._evaluate_crypto_thresholds(signals, params)
        assert result.brier_score > 0
        assert result.brier_score < 1.0

    def test_no_signals_pass_filter(self):
        sweep = self._make_sweep()
        signals = [self._make_signal(edge=0.01)]
        params = {"min_edge": 0.99, "min_confidence": 0.0, "min_kelly": 0.0, "kelly_multiplier": 1.0}
        result = sweep._evaluate_crypto_thresholds(signals, params)
        assert result.total_signals == 0
        assert result.brier_score == 0.0


# ── 8.4: Multi-signal mode ──────────────────────────────────────

class TestMultiSignalMode:
    """Verify multi-signal vs single-signal behavior."""

    def _make_sweep(self, multi_signal: bool = False) -> ParameterSweep:
        sweep = object.__new__(ParameterSweep)
        sweep.pool = None
        sweep.fee_model = FeeModel(fee_type="flat", flat_fee_cents=0)
        sweep.time_decay_lambda = 0.0
        sweep.multi_signal = multi_signal
        return sweep

    def _make_signal(self, ticker: str, day_offset: int = 0, **kwargs) -> dict:
        base = datetime(2026, 1, 1, tzinfo=timezone.utc)
        defaults = {
            "ticker": ticker,
            "signal_type": "crypto",
            "direction": "yes",
            "model_prob": 0.65,
            "market_price": 0.50,
            "edge": 0.15,
            "kelly_fraction": 0.05,
            "minutes_remaining": 30.0,
            "created_at": base + timedelta(days=day_offset),
            "settled_yes": True,
            "settlement_time": base + timedelta(days=day_offset, hours=1),
            "confidence": 0.7,
        }
        defaults.update(kwargs)
        return defaults

    def test_single_signal_deduplicates_tickers(self):
        """In single-signal mode, only first signal per ticker is used."""
        sweep = self._make_sweep(multi_signal=False)
        signals = [
            self._make_signal("BTC-A", day_offset=0),
            self._make_signal("BTC-A", day_offset=0),  # same ticker
            self._make_signal("BTC-B", day_offset=1),
        ]
        params = {"min_edge": 0.0, "min_confidence": 0.0, "min_kelly": 0.0, "kelly_multiplier": 1.0}
        result = sweep._evaluate_crypto_thresholds(signals, params)
        assert result.total_signals == 2  # one per ticker

    def test_multi_signal_allows_duplicates(self):
        """In multi-signal mode, all signals count."""
        sweep = self._make_sweep(multi_signal=True)
        signals = [
            self._make_signal("BTC-A", day_offset=0),
            self._make_signal("BTC-A", day_offset=0),  # same ticker OK
            self._make_signal("BTC-B", day_offset=1),
        ]
        params = {"min_edge": 0.0, "min_confidence": 0.0, "min_kelly": 0.0, "kelly_multiplier": 1.0}
        result = sweep._evaluate_crypto_thresholds(signals, params)
        assert result.total_signals == 3

    def test_multi_signal_more_signals_than_single(self):
        """Multi-signal should produce >= signals compared to single mode."""
        signals = [
            self._make_signal("T1", day_offset=0),
            self._make_signal("T1", day_offset=0),
            self._make_signal("T2", day_offset=1),
            self._make_signal("T2", day_offset=1),
        ]
        params = {"min_edge": 0.0, "min_confidence": 0.0, "min_kelly": 0.0, "kelly_multiplier": 1.0}

        single = self._make_sweep(multi_signal=False)._evaluate_crypto_thresholds(signals, params)
        multi = self._make_sweep(multi_signal=True)._evaluate_crypto_thresholds(signals, params)
        assert multi.total_signals >= single.total_signals


# ── 8.5: Parallel execution helpers ─────────────────────────────

class TestWalkForwardSplits:
    """Walk-forward split generation."""

    def test_splits_cover_range(self):
        splits = _generate_walk_forward_splits(
            date(2026, 1, 1), date(2026, 3, 1), window_days=14
        )
        assert len(splits) >= 2

    def test_splits_validation_does_not_overlap(self):
        """Validation windows should not overlap with each other."""
        splits = _generate_walk_forward_splits(
            date(2026, 1, 1), date(2026, 4, 1), window_days=14
        )
        for i in range(len(splits) - 1):
            # Each val window starts after the previous val starts
            assert splits[i + 1].val_start > splits[i].val_start
            # Train of next split starts at val_start of current (rolling)
            assert splits[i + 1].train_start == splits[i].val_start

    def test_too_short_range_no_splits(self):
        splits = _generate_walk_forward_splits(
            date(2026, 1, 1), date(2026, 1, 10), window_days=14
        )
        assert len(splits) == 0
