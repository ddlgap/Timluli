# Builds the standalone PDF-translation sidecar (timluli-pdf.exe) with PyInstaller
# and copies it into src-tauri/resources/ so Tauri bundles it as a resource.
#
# Run this BEFORE `npm run tauri:dev` or `npm run tauri:build` — Tauri copies the
# resources/ tree at compile time, so the exe must already be there.
#
# Prerequisites: Python 3.12 in PATH (the `py -3.12` launcher). All Python deps
# are installed into a throwaway .build-venv here; nothing touches the system env.
#
# Design notes:
# - Console subsystem (NOT --noconsole): the Rust host pipes stdout/stderr and
#   parses them, so std handles must always be valid. The window is suppressed at
#   launch by CREATE_NO_WINDOW (0x08000000) in Rust — same as the Chrome/LibreOffice
#   sidecars — so no console flashes. A --windowed exe risks None stdio.
# - --collect-all pymupdf + --hidden-import fitz: ensure PyMuPDF's binaries and the
#   legacy `fitz` import shim are bundled.

$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

if (-not (Test-Path .\timluli_pdf.py)) {
    Write-Error "timluli_pdf.py not found next to this script."
    exit 1
}

# Pick a Python 3.12 interpreter to seed the venv: prefer the `py -3.12` launcher
# (local dev), fall back to `python` on PATH (e.g. actions/setup-python in CI,
# where the py launcher may not expose -3.12).
$baseExe = $null
$baseArgs = @()
if (Get-Command py -ErrorAction SilentlyContinue) {
    try {
        & py -3.12 --version *> $null
        if ($LASTEXITCODE -eq 0) { $baseExe = "py"; $baseArgs = @("-3.12") }
    } catch {}
}
if (-not $baseExe -and (Get-Command python -ErrorAction SilentlyContinue)) {
    $baseExe = "python"
}
if (-not $baseExe) {
    Write-Error "No Python 3.12 found (need the 'py -3.12' launcher or 'python' on PATH)."
    exit 1
}

$venv = ".build-venv"
$python = Join-Path $venv "Scripts\python.exe"

if (-not (Test-Path $python)) {
    Write-Host "Creating build venv ($venv) using '$baseExe $baseArgs'..."
    & $baseExe @baseArgs -m venv $venv
}

Write-Host "Installing build dependencies..."
& $python -m pip install --quiet --upgrade pip
& $python -m pip install --quiet pymupdf python-bidi requests pyinstaller

Write-Host "Building timluli-pdf.exe (this can take a minute)..."
# --add-data "glossaries;glossaries": bundle the curated telecom glossary so the
# frozen exe can load it via sys._MEIPASS (see _resource_path in timluli_pdf.py).
# On Windows the PyInstaller --add-data separator is ';'.
$addData = @("glossaries;glossaries")
# Bundle a Hebrew font only if one was dropped into fonts/ (else the renderer falls
# back to system Arial — see _resolve_hebrew_font). Avoids shipping an empty dir.
if (Test-Path ".\fonts\*.ttf") {
    $addData += "fonts;fonts"
    Write-Host "  bundling fonts/ (found .ttf)"
}
$dataArgs = $addData | ForEach-Object { @("--add-data", $_) }
& $python -m PyInstaller --noconfirm --onefile `
    --name timluli-pdf `
    --collect-all pymupdf `
    --hidden-import fitz `
    @dataArgs `
    timluli_pdf.py

$built = ".\dist\timluli-pdf.exe"
if (-not (Test-Path $built)) {
    Write-Error "Build failed: $built was not produced."
    exit 1
}

$resources = Join-Path $PSScriptRoot "..\resources"
if (-not (Test-Path $resources)) { New-Item -ItemType Directory -Force -Path $resources | Out-Null }
$dest = Join-Path $resources "timluli-pdf.exe"
Copy-Item $built $dest -Force
Write-Host "Built -> src-tauri/resources/timluli-pdf.exe"
