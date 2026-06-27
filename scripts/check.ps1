#!/usr/bin/env pwsh
# Quality gate (rust skill): formatting, lints, tests. Stops on first failure.
$ErrorActionPreference = "Stop"

Write-Host "==> cargo fmt --all --check" -ForegroundColor Cyan
cargo fmt --all --check

Write-Host "==> cargo clippy --all-targets --all-features -- -D warnings" -ForegroundColor Cyan
cargo clippy --all-targets --all-features -- -D warnings

Write-Host "==> cargo test --workspace" -ForegroundColor Cyan
cargo test --workspace

Write-Host "All checks passed." -ForegroundColor Green
