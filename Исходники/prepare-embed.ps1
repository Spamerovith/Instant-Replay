# Packs JustView and FFmpeg into embed\ - build.rs builds them into Instant Replay.exe.
# JustView is built from justview\ (a copy of the JustView project, keep it in sync with it).
# FFmpeg (LGPL shared build by BtbN) is downloaded once into the cache folder on D:.
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

$embed = Join-Path $PSScriptRoot 'embed'
New-Item -ItemType Directory -Force $embed | Out-Null
$cache = 'D:/cargo-target/ffmpeg-cache'
$ffUrl = 'https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-lgpl-shared.zip'

function Pack($src, $name) {
    $dst = Join-Path $embed ($name + '.gz')
    if ((Test-Path $dst) -and ((Get-Item $dst).LastWriteTime -gt (Get-Item $src).LastWriteTime)) { return }
    Write-Host "pack $name"
    $in = [IO.File]::OpenRead($src)
    $out = [IO.File]::Create($dst)
    $gz = New-Object IO.Compression.GZipStream($out, [IO.Compression.CompressionLevel]::Optimal)
    $in.CopyTo($gz)
    $gz.Dispose(); $out.Dispose(); $in.Dispose()
}

# JustView
$env:CARGO_TARGET_DIR = 'D:/cargo-target/justview-embedded'
cmd /c "cargo build --release --manifest-path justview\Cargo.toml 2>&1"
if ($LASTEXITCODE -ne 0) { throw 'JustView build failed' }
Pack "$env:CARGO_TARGET_DIR/release/JustView.exe" 'JustView.exe'
Remove-Item Env:CARGO_TARGET_DIR

# FFmpeg
$ff = Join-Path $cache 'ffmpeg-master-latest-win64-lgpl-shared'
if (-not (Test-Path (Join-Path $ff 'bin\ffmpeg.exe'))) {
    New-Item -ItemType Directory -Force $cache | Out-Null
    $zip = Join-Path $cache 'ffmpeg-lgpl-shared.zip'
    if (-not (Test-Path $zip)) { Invoke-WebRequest $ffUrl -OutFile $zip }
    Expand-Archive $zip -DestinationPath $cache -Force
}
foreach ($f in Get-ChildItem (Join-Path $ff 'bin') | Where-Object { $_.Name -notlike 'ffplay*' }) {
    Pack $f.FullName ('ffmpeg__' + $f.Name)
}
Pack (Join-Path $ff 'LICENSE.txt') 'ffmpeg__LICENSE.txt'
Write-Host 'embed ready'
