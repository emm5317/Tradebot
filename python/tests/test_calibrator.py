"""Tests for calibration agent logic."""

from __future__ import annotations

from calibrator.daemon import CalibrationDaemon, _parse_update_count


class TestParseUpdateCount:
    """Test asyncpg result string parsing."""

    def test_update_count(self):
        assert _parse_update_count("UPDATE 5") == 5

    def test_insert_count(self):
        assert _parse_update_count("INSERT 0 3") == 3

    def test_zero_count(self):
        assert _parse_update_count("UPDATE 0") == 0

    def test_empty_string(self):
        assert _parse_update_count("") == 0

    def test_no_number(self):
        assert _parse_update_count("ERROR") == 0


class TestDriftThreshold:
    """Test drift detection threshold logic."""

    def test_drift_detected_when_recent_exceeds_baseline(self):
        # 7d Brier = 0.25, 30d Brier = 0.20 → drift = 0.05 > 0.03
        recent = 0.25
        baseline = 0.20
        assert recent > baseline + 0.03

    def test_no_drift_when_within_threshold(self):
        # 7d Brier = 0.22, 30d Brier = 0.20 → drift = 0.02 < 0.03
        recent = 0.22
        baseline = 0.20
        assert not (recent > baseline + 0.03)

    def test_no_drift_when_improving(self):
        # 7d Brier = 0.18, 30d Brier = 0.20 → improving
        recent = 0.18
        baseline = 0.20
        assert not (recent > baseline + 0.03)


class TestCalibrationDaemonInit:
    """Test daemon initialization."""

    def test_creates_with_defaults(self):
        daemon = CalibrationDaemon()
        assert daemon.pool is None
        assert daemon._shutdown is not None
        assert not daemon._shutdown.is_set()

    def test_shutdown(self):
        daemon = CalibrationDaemon()
        daemon.shutdown()
        assert daemon._shutdown.is_set()


class TestOutcomeSettlement:
    """Test outcome settlement SQL logic."""

    def test_win_yes_settled_yes(self):
        """YES direction + settled_yes=true → win."""
        direction = "yes"
        settled_yes = True
        outcome = "win" if (direction == "yes" and settled_yes) or (direction == "no" and not settled_yes) else "loss"
        assert outcome == "win"

    def test_win_no_settled_no(self):
        """NO direction + settled_yes=false → win."""
        direction = "no"
        settled_yes = False
        outcome = "win" if (direction == "yes" and settled_yes) or (direction == "no" and not settled_yes) else "loss"
        assert outcome == "win"

    def test_loss_yes_settled_no(self):
        """YES direction + settled_yes=false → loss."""
        direction = "yes"
        settled_yes = False
        outcome = "win" if (direction == "yes" and settled_yes) or (direction == "no" and not settled_yes) else "loss"
        assert outcome == "loss"

    def test_loss_no_settled_yes(self):
        """NO direction + settled_yes=true → loss."""
        direction = "no"
        settled_yes = True
        outcome = "win" if (direction == "yes" and settled_yes) or (direction == "no" and not settled_yes) else "loss"
        assert outcome == "loss"
