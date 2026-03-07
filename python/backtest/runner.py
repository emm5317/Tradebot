"""Generic backtest runner."""

from dataclasses import dataclass, field


@dataclass
class BacktestResult:
    trades: list[dict] = field(default_factory=list)
    total_pnl: float = 0.0
    win_rate: float = 0.0
    sharpe: float = 0.0


class BacktestRunner:
    """Runs a signal strategy over historical data."""

    def __init__(self, initial_bankroll: float = 10_000.0):
        self.bankroll = initial_bankroll

    def run(self, signals: list[dict], outcomes: list[dict]) -> BacktestResult:
        """Execute backtest over paired signals and outcomes."""
        # TODO: iterate, simulate fills, track P&L
        return BacktestResult()
