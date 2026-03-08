"""Tests for Phase 5.1: Strategy analytics aggregation."""

from __future__ import annotations

from datetime import date
from unittest.mock import AsyncMock

import pytest

from analytics.aggregator import (
    _compute_strategy_metrics,
    _upsert_performance,
    aggregate_daily_performance,
)


class _MockConn:
    def __init__(self, sig_row, order_row, brier_row):
        self._results = [sig_row, order_row, brier_row]
        self._idx = 0
        self.execute = AsyncMock()

    async def fetchrow(self, query, *args):
        idx = self._idx
        self._idx += 1
        if idx < len(self._results):
            return self._results[idx]
        return None


class _MockCtx:
    def __init__(self, conn):
        self._conn = conn

    async def __aenter__(self):
        # Reset counter each time a new context is entered
        self._conn._idx = 0
        return self._conn

    async def __aexit__(self, *args):
        pass


class _MockPool:
    def __init__(self, sig_row, order_row, brier_row):
        self._conn = _MockConn(sig_row, order_row, brier_row)

    def acquire(self):
        return _MockCtx(self._conn)


def _make_mock_pool(sig_row=None, order_row=None, brier_row=None):
    """Create a mock pool that returns specified query results."""
    pool = _MockPool(sig_row, order_row, brier_row)
    return pool, pool._conn


class TestComputeStrategyMetrics:
    @pytest.mark.asyncio
    async def test_empty_day_returns_zeros(self):
        sig = {"total": 0, "executed": 0, "avg_edge": None, "avg_kelly": None}
        order = {"wins": 0, "losses": 0, "realized_pnl": 0}
        brier = {"brier_score": None}

        pool, _ = _make_mock_pool(sig, order, brier)
        metrics = await _compute_strategy_metrics(pool, "weather", date(2026, 3, 7))

        assert metrics["signals_generated"] == 0
        assert metrics["signals_executed"] == 0
        assert metrics["win_count"] == 0
        assert metrics["loss_count"] == 0
        assert metrics["realized_pnl_cents"] == 0
        assert metrics["brier_score"] is None

    @pytest.mark.asyncio
    async def test_populated_day(self):
        sig = {"total": 50, "executed": 12, "avg_edge": 0.08, "avg_kelly": 0.15}
        order = {"wins": 8, "losses": 4, "realized_pnl": 350}
        brier = {"brier_score": 0.18}

        pool, _ = _make_mock_pool(sig, order, brier)
        metrics = await _compute_strategy_metrics(pool, "crypto", date(2026, 3, 7))

        assert metrics["signals_generated"] == 50
        assert metrics["signals_executed"] == 12
        assert metrics["win_count"] == 8
        assert metrics["loss_count"] == 4
        assert metrics["realized_pnl_cents"] == 350
        assert abs(metrics["brier_score"] - 0.18) < 0.001
        assert abs(metrics["avg_edge"] - 0.08) < 0.001
        assert abs(metrics["avg_kelly"] - 0.15) < 0.001

    @pytest.mark.asyncio
    async def test_none_rows_return_defaults(self):
        pool, _ = _make_mock_pool(None, None, None)
        metrics = await _compute_strategy_metrics(pool, "weather", date(2026, 3, 7))

        assert metrics["signals_generated"] == 0
        assert metrics["win_count"] == 0
        assert metrics["realized_pnl_cents"] == 0
        assert metrics["brier_score"] is None


class TestUpsertPerformance:
    @pytest.mark.asyncio
    async def test_upsert_calls_execute(self):
        pool, conn = _make_mock_pool()
        metrics = {
            "signals_generated": 10,
            "signals_executed": 5,
            "win_count": 3,
            "loss_count": 2,
            "realized_pnl_cents": 150,
            "avg_edge": 0.06,
            "avg_kelly": 0.12,
            "brier_score": 0.2,
        }
        await _upsert_performance(pool, "weather", date(2026, 3, 7), metrics)
        conn.execute.assert_called_once()


class TestAggregateDailyPerformance:
    @pytest.mark.asyncio
    async def test_aggregates_both_strategies(self):
        sig = {"total": 5, "executed": 2, "avg_edge": 0.05, "avg_kelly": 0.1}
        order = {"wins": 1, "losses": 1, "realized_pnl": 50}
        brier = {"brier_score": 0.25}

        pool, _ = _make_mock_pool(sig, order, brier)
        results = await aggregate_daily_performance(pool, date(2026, 3, 7))

        assert "weather" in results
        assert "crypto" in results
        assert results["weather"]["signals_generated"] == 5
        assert results["crypto"]["signals_generated"] == 5

    @pytest.mark.asyncio
    async def test_win_rate_calculation(self):
        sig = {"total": 20, "executed": 10, "avg_edge": 0.07, "avg_kelly": 0.14}
        order = {"wins": 7, "losses": 3, "realized_pnl": 200}
        brier = {"brier_score": 0.15}

        pool, _ = _make_mock_pool(sig, order, brier)
        results = await aggregate_daily_performance(pool, date(2026, 3, 7))

        weather = results["weather"]
        win_rate = weather["win_count"] / (weather["win_count"] + weather["loss_count"])
        assert win_rate == 0.7
