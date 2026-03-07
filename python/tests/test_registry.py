"""Tests for the evaluator plugin registry."""

from signals.registry import EvaluatorRegistry
from signals.weather import WeatherSignalEvaluator
from signals.crypto import CryptoSignalEvaluator


def test_register_and_get():
    reg = EvaluatorRegistry()
    weather = WeatherSignalEvaluator()
    reg.register("weather", weather)
    assert reg.get("weather") is weather


def test_get_missing_returns_none():
    reg = EvaluatorRegistry()
    assert reg.get("sports") is None


def test_all_returns_registered():
    reg = EvaluatorRegistry()
    reg.register("weather", WeatherSignalEvaluator())
    reg.register("crypto", CryptoSignalEvaluator())
    all_evals = reg.all()
    assert set(all_evals.keys()) == {"weather", "crypto"}


def test_types():
    reg = EvaluatorRegistry()
    reg.register("weather", WeatherSignalEvaluator())
    reg.register("crypto", CryptoSignalEvaluator())
    assert sorted(reg.types()) == ["crypto", "weather"]
