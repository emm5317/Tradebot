# Setup database – run migrations against local PostgreSQL
$ErrorActionPreference = "Stop"

$DB_URL = if ($env:DATABASE_URL) { $env:DATABASE_URL } else { "postgresql://tradebot:tradebot@localhost:5432/tradebot" }

Write-Host "Running migrations against $DB_URL ..."

Get-ChildItem -Path ".\migrations\*.sql" | Sort-Object Name | ForEach-Object {
    Write-Host "  Applying $($_.Name) ..."
    psql $DB_URL -f $_.FullName
}

Write-Host "Migrations complete."
