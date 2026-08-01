# Dev loop: rebuild and restart Beautiful on source changes.
# Install once: cargo install cargo-watch
# Usage: .\scripts\dev-watch.ps1

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$cargo = "$env:USERPROFILE\.cargo\bin\cargo.exe"
if (-not (Test-Path $cargo)) {
    $cargo = "cargo"
}

Write-Host "Watching crates/ — Ctrl+C to stop" -ForegroundColor Cyan
& $cargo watch -q -c -x "run -p beautiful-app"
