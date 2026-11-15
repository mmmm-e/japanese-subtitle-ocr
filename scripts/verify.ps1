$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)
cargo fmt --check
if ($LASTEXITCODE) { throw 'Formatting failed' }
cargo clippy --all-targets -- -D warnings
if ($LASTEXITCODE) { throw 'Clippy failed' }
cargo test
if ($LASTEXITCODE) { throw 'Tests failed' }
cargo build --release
if ($LASTEXITCODE) { throw 'Release build failed' }
