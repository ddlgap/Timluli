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

$venv = ".build-venv"
$python = Join-Path $venv "Scripts\python.exe"

if (-not (Test-Path $python)) {
    Write-Host "Creating build venv ($venv) with Python 3.12..."
    py -3.12 -m venv $venv
}

Write-Host "Installing build dependencies..."
& $python -m pip install --quiet --upgrade pip
& $python -m pip install --quiet pymupdf python-bidi requests pyinstaller

Write-Host "Building timluli-pdf.exe (this can take a minute)..."
& $python -m PyInstaller --noconfirm --onefile `
    --name timluli-pdf `
    --collect-all pymupdf `
    --hidden-import fitz `
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
