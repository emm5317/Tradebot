# Run backtests
$ErrorActionPreference = "Stop"

$strategy = if ($args[0]) { $args[0] } else { "weather" }

Write-Host "Running $strategy backtest ..."

Set-Location $PSScriptRoot\..
python -m python.backtest."${strategy}_backtest"
