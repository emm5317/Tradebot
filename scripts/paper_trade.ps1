# Start paper trading mode
$ErrorActionPreference = "Stop"

$env:TRADING_MODE = "paper"

Write-Host "Starting Tradebot in PAPER trading mode ..."
Write-Host "Press Ctrl+C to stop."

# Start Python signal generator in background
$pythonJob = Start-Job -ScriptBlock {
    Set-Location $using:PSScriptRoot\..
    python -m python.signals.scanner
}

# Start Rust execution engine
Set-Location $PSScriptRoot\..\rust
cargo run --release

# Cleanup
Stop-Job $pythonJob -ErrorAction SilentlyContinue
