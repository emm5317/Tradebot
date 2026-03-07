"""Backtest weather-based strategies against historical ASOS data."""

from .runner import BacktestRunner, BacktestResult


def run_weather_backtest(station: str, start: str, end: str) -> BacktestResult:
    """Run a full weather backtest for a given station and date range."""
    runner = BacktestRunner()
    # TODO: load historical weather data + historical Kalshi settlements
    # TODO: generate signals, run through runner
    return runner.run([], [])
