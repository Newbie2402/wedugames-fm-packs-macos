# Builds the shipped customer exe: minify the frontend, compile in release
# (full LTO + stripped, set in the workspace Cargo.toml), then copy the binary
# to releases\WeduGames-FM-Packs.exe. Run from the project root.
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

& powershell -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot "build_ui.ps1")
if ($LASTEXITCODE -ne 0) { throw "build_ui gagal" }

cargo build --release -p wedugames-fm-packs
if ($LASTEXITCODE -ne 0) { throw "cargo build gagal" }

$built = Join-Path $root "target\release\wedugames-fm-packs.exe"
$dest = Join-Path $root "releases\WeduGames-FM-Packs.exe"
try {
    Copy-Item $built $dest -Force
    Write-Host ("built releases\WeduGames-FM-Packs.exe " + (Get-Item $dest).LastWriteTime)
} catch {
    # The release exe is locked, usually because the app is open. Write a dated
    # copy instead so the build is never lost; rename it once the app is closed.
    $alt = Join-Path $root ("releases\WeduGames-FM-Packs-" + (Get-Date -Format "MMdd-HHmm") + ".exe")
    Copy-Item $built $alt -Force
    Write-Host "releases\WeduGames-FM-Packs.exe terkunci (app sedang jalan). Disimpan sebagai $alt"
}
