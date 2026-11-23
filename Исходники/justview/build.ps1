# Сборка JustView и копирование готового приложения в ..\JustView
Set-Location $PSScriptRoot

# cargo пишет прогресс в stderr — в Windows PowerShell 5.1 это не ошибка, поэтому через cmd.
cmd /c "cargo build --release 2>&1"
if ($LASTEXITCODE -ne 0) { throw 'Сборка не удалась' }
$ErrorActionPreference = 'Stop'

$out = Join-Path $PSScriptRoot '..\JustView'
New-Item -ItemType Directory -Force $out | Out-Null
Copy-Item 'target\release\JustView.exe' $out -Force
Copy-Item 'assets\icon.ico' (Join-Path $out 'JustView.ico') -Force

$version = (Select-String -Path 'Cargo.toml' -Pattern '^version\s*=\s*"(.+)"').Matches[0].Groups[1].Value
Write-Host "JustView $version собран -> $((Resolve-Path $out).Path)"
