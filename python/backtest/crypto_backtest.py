"""Backtest crypto range strategies against historical price data."""

from .runner import BacktestRunner, BacktestResult


def run_crypto_backtest(symbol: str, start: str, end: str) -> BacktestResult:
    """Run a full crypto backtest for a given symbol and date range."""
    runner = BacktestRunner()
    # TODO: load historical price data + historical Kalshi settlements
    # TODO: generate signals, run through runner
    return runner.run([], [])
