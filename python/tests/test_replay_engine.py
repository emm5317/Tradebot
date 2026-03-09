"""Tests for replay engine and source ablation (Phase 8.6)."""

from __future__ import annotations

import json
import math

import pytest

from backtester.replay import (
    ReplayConfig,
    ReplayEngine,
    ReplayResult,
    SourceAttribution,
    ablate_and_reblend,
)


# ── 8.6a: Source ablation logic ──────────────────────────────────

class TestAblateAndReblend:
    """Test the ablate_and_reblend function directly (no DB needed)."""

    def _make_components(
        self,
        physics: float = 0.65,
        hrrr: float = 0.70,
        trend: float = 0.60,
        climo: float = 0.55,
        weights: list[float] | None = None,
    ) -> dict:
        return {
            "physics": physics,
            "hrrr": hrrr,
            "trend": trend,
            "climo": climo,
            "weights": weights or [0.45, 0.25, 0.20, 0.10],
        }

    def test_no_ablation_returns_original_blend(self):
        """With no sources ablated, result equals weighted average."""
        comp = self._make_components()
        result = ablate_and_reblend(comp, [])
        expected = (0.45*0.65 + 0.25*0.70 + 0.20*0.60 + 0.10*0.55) / 1.0
        assert abs(result - expected) < 1e-9

    def test_ablate_hrrr(self):
        """Ablating HRRR redistributes its weight among remaining sources."""
        comp = self._make_components()
        result = ablate_and_reblend(comp, ["hrrr"])
        # Remaining: physics(0.45), trend(0.20), climo(0.10) → total 0.75
        expected = (0.45*0.65 + 0.20*0.60 + 0.10*0.55) / 0.75
        assert abs(result - expected) < 1e-9

    def test_ablate_multiple_sources(self):
        """Ablating multiple sources works correctly."""
        comp = self._make_components()
        result = ablate_and_reblend(comp, ["hrrr", "trend"])
        # Remaining: physics(0.45), climo(0.10) → total 0.55
        expected = (0.45*0.65 + 0.10*0.55) / 0.55
        assert abs(result - expected) < 1e-9

    def test_ablate_all_returns_default(self):
        """Ablating all sources returns 0.5 default."""
        comp = self._make_components()
        result = ablate_and_reblend(comp, ["physics", "hrrr", "trend", "climo"])
        assert result == 0.5

    def test_none_components_returns_default(self):
        assert ablate_and_reblend(None, ["hrrr"]) == 0.5

    def test_string_components_parsed(self):
        """JSON string components are parsed correctly."""
        comp = self._make_components()
        result = ablate_and_reblend(json.dumps(comp), [])
        expected = (0.45*0.65 + 0.25*0.70 + 0.20*0.60 + 0.10*0.55) / 1.0
        assert abs(result - expected) < 1e-9

    def test_no_weights_key_returns_stored_prob(self):
        """Components without 'weights' key fall back to stored probability."""
        comp = {"probability": 0.72}
        result = ablate_and_reblend(comp, ["hrrr"])
        assert result == 0.72

    def test_result_clamped_to_01(self):
        """Result is clamped to [0.0, 1.0]."""
        comp = {
            "physics": 1.5,  # out of range
            "weights": [1.0],
        }
        result = ablate_and_reblend(comp, [])
        assert result == 1.0

    def test_crypto_sources(self):
        """Crypto source names are detected and used."""
        comp = {
            "n_d2": 0.60,
            "levy": 0.55,
            "basis": 0.70,
            "funding": 0.50,
            "weights": [0.40, 0.30, 0.20, 0.10],
        }
        result = ablate_and_reblend(comp, ["basis"])
        # Remaining: n_d2(0.40), levy(0.30), funding(0.10) → total 0.80
        expected = (0.40*0.60 + 0.30*0.55 + 0.10*0.50) / 0.80
        assert abs(result - expected) < 1e-9

    def test_ablate_physics_only(self):
        """Ablating only physics redistributes to hrrr+trend+climo."""
        comp = self._make_components(physics=0.80, hrrr=0.60, trend=0.50, climo=0.40)
        result = ablate_and_reblend(comp, ["physics"])
        remaining_w = 0.25 + 0.20 + 0.10  # 0.55
        expected = (0.25*0.60 + 0.20*0.50 + 0.10*0.40) / remaining_w
        assert abs(result - expected) < 1e-9


# ── Attribution computation ──────────────────────────────────────

class TestComputeAttribution:
    """Test attribution comparison logic."""

    def _make_result(self, brier: float, pnl: int, n_evals: int = 100) -> ReplayResult:
        config = ReplayConfig(
            start_time=None,  # type: ignore
            end_time=None,  # type: ignore
        )
        return ReplayResult(
            config=config,
            n_evaluations=n_evals,
            brier_score=brier,
            pnl_cents=pnl,
        )

    def _engine(self) -> ReplayEngine:
        """Create ReplayEngine with no DB pool (for compute_attribution only)."""
        engine = object.__new__(ReplayEngine)
        engine.pool = None
        engine.fee_model = None
        return engine

    def test_source_improves_model(self):
        """Source that lowers Brier → positive brier_delta."""
        engine = self._engine()
        baseline = self._make_result(brier=0.20, pnl=100)  # with source
        ablated = self._make_result(brier=0.30, pnl=50)    # without source
        attr = engine.compute_attribution("hrrr", baseline, ablated)
        assert attr.brier_delta > 0  # positive = source helps
        assert attr.pnl_delta > 0

    def test_source_hurts_model(self):
        """Source that raises Brier → negative brier_delta."""
        engine = self._engine()
        baseline = self._make_result(brier=0.30, pnl=50)
        ablated = self._make_result(brier=0.20, pnl=100)
        attr = engine.compute_attribution("bad_source", baseline, ablated)
        assert attr.brier_delta < 0  # negative = source hurts
        assert attr.pnl_delta < 0

    def test_source_neutral(self):
        """Source with no effect → zero deltas."""
        engine = self._engine()
        baseline = self._make_result(brier=0.25, pnl=80)
        ablated = self._make_result(brier=0.25, pnl=80)
        attr = engine.compute_attribution("neutral", baseline, ablated)
        assert attr.brier_delta == 0.0
        assert attr.pnl_delta == 0

    def test_n_affected_matches_baseline(self):
        engine = self._engine()
        baseline = self._make_result(brier=0.20, pnl=100, n_evals=42)
        ablated = self._make_result(brier=0.25, pnl=80)
        attr = engine.compute_attribution("x", baseline, ablated)
        assert attr.n_affected == 42


# ── Ablation effect on Brier ─────────────────────────────────────

class TestAblationBrierEffect:
    """Verify that ablation produces meaningful Brier score changes."""

    def test_removing_good_source_worsens_brier(self):
        """If a source contributes a high-quality probability, removing it
        should produce a worse (higher) blended probability error."""
        # Good source: physics predicts 0.90, actual outcome = YES
        comp = {
            "physics": 0.90,
            "hrrr": 0.50,
            "trend": 0.50,
            "climo": 0.50,
            "weights": [0.50, 0.20, 0.20, 0.10],
        }
        baseline_prob = ablate_and_reblend(comp, [])
        ablated_prob = ablate_and_reblend(comp, ["physics"])

        # Brier for outcome=1.0
        baseline_brier = (baseline_prob - 1.0) ** 2
        ablated_brier = (ablated_prob - 1.0) ** 2
        assert ablated_brier > baseline_brier

    def test_removing_bad_source_improves_brier(self):
        """If a source contributes a misleading probability, removing it
        should improve the blended probability."""
        # Bad source: physics predicts 0.10, but actual outcome = YES
        comp = {
            "physics": 0.10,
            "hrrr": 0.80,
            "trend": 0.80,
            "climo": 0.80,
            "weights": [0.50, 0.20, 0.20, 0.10],
        }
        baseline_prob = ablate_and_reblend(comp, [])
        ablated_prob = ablate_and_reblend(comp, ["physics"])

        baseline_brier = (baseline_prob - 1.0) ** 2
        ablated_brier = (ablated_prob - 1.0) ** 2
        assert ablated_brier < baseline_brier
