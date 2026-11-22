# Builds the optimized release and copies it into ..\Instant Replay\
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

cargo test --release
if ($LASTEXITCODE -ne 0) { throw "tests failed" }
cargo build --release
if ($LASTEXITCODE -ne 0) { throw "build failed" }

$meta = cargo metadata --format-version 1 --no-deps | ConvertFrom-Json
$target = $meta.target_directory
$version = ($meta.packages | Where-Object name -eq 'instant-replay').version

$dest = Join-Path (Split-Path $PSScriptRoot -Parent) 'Instant Replay'
New-Item -ItemType Directory -Force $dest | Out-Null
Copy-Item (Join-Path $target 'release\instant-replay.exe') (Join-Path $dest 'Instant Replay.exe') -Force
Write-Host "Instant Replay $version -> $dest\Instant Replay.exe"
