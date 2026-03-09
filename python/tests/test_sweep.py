"""Tests for parameter sweep framework and settlement summary."""

from __future__ import annotations

import json
from datetime import date, datetime, timedelta, timezone
from unittest.mock import AsyncMock

import pytest

from backtester.sweep import (
    ParameterSweep,
    SweepResult,
    WalkForwardSplit,
    _generate_combinations,
    _generate_walk_forward_splits,
)


# ── Combination generation ───────────────────────────────────────

class TestGenerateCombinations:
    def test_single_param(self):
        grid = {"a": [1, 2, 3]}
        combos = _generate_combinations(grid, 100)
        assert len(combos) == 3
        assert combos[0] == {"a": 1}
        assert combos[2] == {"a": 3}

    def test_multi_param_cartesian(self):
        grid = {"a": [1, 2], "b": [10, 20]}
        combos = _generate_combinations(grid, 100)
        assert len(combos) == 4
        assert {"a": 1, "b": 10} in combos
        assert {"a": 2, "b": 20} in combos

    def test_max_combos_cap(self):
        grid = {"a": list(range(50)), "b": list(range(50))}
        combos = _generate_combinations(grid, 100)
        assert len(combos) == 100

    def test_empty_grid(self):
        combos = _generate_combinations({}, 100)
        assert len(combos) == 1  # single empty combo from product of nothing
        assert combos[0] == {}


# ── Walk-forward splits ─────────────────────────────────────────

class TestWalkForwardSplits:
    def test_basic_splits(self):
        start = date(2026, 1, 1)
        end = date(2026, 2, 28)
        splits = _generate_walk_forward_splits(start, end, window_days=14)

        assert len(splits) >= 2
        for s in splits:
            assert s.train_end < s.val_start
            assert (s.val_end - s.val_start).days == 13  # 14 days inclusive - 1

    def test_no_overlap(self):
        start = date(2026, 1, 1)
        end = date(2026, 3, 31)
        splits = _generate_walk_forward_splits(start, end, window_days=14)

        for i in range(len(splits) - 1):
            # Next split's train starts at current split's val_start
            assert splits[i + 1].train_start == splits[i].val_start

    def test_too_short_for_splits(self):
        start = date(2026, 1, 1)
        end = date(2026, 1, 10)
        splits = _generate_walk_forward_splits(start, end, window_days=14)
        assert len(splits) == 0

    def test_single_split(self):
        start = date(2026, 1, 1)
        end = date(2026, 1, 29)  # exactly 2 windows of 14 days
        splits = _generate_walk_forward_splits(start, end, window_days=14)
        assert len(splits) == 1
        assert splits[0].train_start == date(2026, 1, 1)
        assert splits[0].val_start == date(2026, 1, 15)


# ── SweepResult ──────────────────────────────────────────────────

class TestSweepResult:
    def test_defaults(self):
        r = SweepResult(run_id="abc", params={"sigma_scale": 1.0})
        assert r.total_contracts == 0
        assert r.brier_score == 0.0
        assert r.calibration == {}

    def test_with_values(self):
        r = SweepResult(
            run_id="def",
            params={"sigma_scale": 0.9, "min_edge": 0.05},
            total_contracts=50,
            total_signals=30,
            accuracy=0.65,
            brier_score=0.18,
            simulated_pnl_cents=250,
            win_count=20,
            loss_count=10,
        )
        assert r.accuracy == 0.65
        assert r.win_count + r.loss_count == 30


# ── Settlement summary ──────────────────────────────────────────

class TestSettlementSummary:
    @pytest.mark.asyncio
    async def test_aggregate_with_mock_pool(self):
        from analytics.settlement_summary import aggregate_settlement_summary

        # Build a mock pool that returns realistic query results
        asos_rows = [
            {"station": "KORD", "max_f": 75.0, "min_f": 55.0,
             "obs_count": 24, "first_obs": datetime(2026, 3, 7, 6, 0, tzinfo=timezone.utc),
             "last_obs": datetime(2026, 3, 7, 23, 0, tzinfo=timezone.utc)},
        ]
        metar_rows = [
            {"station": "KORD", "metar_max_c": 24, "metar_min_c": 13},
        ]
        contract_rows = [
            {"station": "KORD", "settled": 3},
        ]

        call_count = 0

        class MockConn:
            async def fetch(self, query, *args):
                nonlocal call_count
                call_count += 1
                if call_count == 1:
                    return asos_rows
                elif call_count == 2:
                    return metar_rows
                elif call_count == 3:
                    return contract_rows
                return []

            async def execute(self, query, *args):
                pass

        class MockCtx:
            async def __aenter__(self):
                return MockConn()
            async def __aexit__(self, *a):
                pass

        class MockPool:
            def acquire(self):
                return MockCtx()

        pool = MockPool()
        results = await aggregate_settlement_summary(pool, date(2026, 3, 7))

        assert "KORD" in results
        assert results["KORD"]["obs_count"] == 24
        # METAR max: 24C = 75.2F, ASOS max: 75.0F -> final should be max(75.0, 75.2) = 75.2
        assert results["KORD"]["metar_max_f"] == pytest.approx(75.2, abs=0.1)

    @pytest.mark.asyncio
    async def test_asos_only_no_metar(self):
        from analytics.settlement_summary import aggregate_settlement_summary

        asos_rows = [
            {"station": "KJFK", "max_f": 68.0, "min_f": 52.0,
             "obs_count": 20, "first_obs": datetime(2026, 3, 7, 6, 0, tzinfo=timezone.utc),
             "last_obs": datetime(2026, 3, 7, 22, 0, tzinfo=timezone.utc)},
        ]

        call_count = 0

        class MockConn:
            async def fetch(self, query, *args):
                nonlocal call_count
                call_count += 1
                if call_count == 1:
                    return asos_rows
                return []

            async def execute(self, query, *args):
                pass

        class MockCtx:
            async def __aenter__(self):
                return MockConn()
            async def __aexit__(self, *a):
                pass

        class MockPool:
            def acquire(self):
                return MockCtx()

        pool = MockPool()
        results = await aggregate_settlement_summary(pool, date(2026, 3, 7))

        assert "KJFK" in results
        # No METAR -> final max/min should be ASOS values
        assert results["KJFK"]["asos_max_f"] == 68.0
        assert results["KJFK"].get("metar_max_f") is None
