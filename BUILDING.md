# Building Timluli on Windows

## Prerequisites

The core app builds with standard Tauri prerequisites. The **local whisper engine**
(`whisper-rs`) requires additional native build tools because it compiles `whisper.cpp`
from C++ source.

### 1. Rust toolchain (stable, MSVC target)

```powershell
winget install Rustlang.Rustup
rustup install stable
rustup default stable
rustup component add clippy rustfmt
```

Verify:
```powershell
rustc --version   # rustc 1.xx.x (stable-x86_64-pc-windows-msvc)
cargo --version
```

### 2. Visual Studio 2019/2022 Build Tools with C++ workload

Required for MSVC linker + Windows SDK headers.

Download: https://visualstudio.microsoft.com/visual-cpp-build-tools/

Install with these workloads:
- **Desktop development with C++**
  - MSVC v143 (or v142) C++ build tools
  - Windows 10/11 SDK
  - C++ CMake tools for Windows ← includes CMake

### 3. CMake

whisper-rs compiles whisper.cpp via CMake.

**Option A — via Visual Studio** (already installed by the C++ workload above):
```
C:\Program Files (x86)\Microsoft Visual Studio\2019\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe
```
Add that path to your `PATH`, or use **Option B**.

**Option B — standalone installer**:
```
winget install Kitware.CMake
```

Verify:
```powershell
cmake --version   # cmake version 3.xx.x
```

### 4. LLVM/Clang (for bindgen)

whisper-rs uses `bindgen` to generate Rust bindings from `whisper.h`, which requires
libclang.

Download: https://releases.llvm.org/ → "LLVM-xx.x.x-win64.exe"

After installing, set the environment variable:
```powershell
[System.Environment]::SetEnvironmentVariable(
    "LIBCLANG_PATH",
    "C:\Program Files\LLVM\bin",
    "User"
)
```

**Restart your shell** after setting this variable.

Verify:
```powershell
clang --version   # clang version xx.x.x
```

### 5. Node.js 18+

```
winget install OpenJS.NodeJS
node --version    # v18.x or higher
npm --version
```

### 6. Python 3.12 (for the PDF translation sidecar)

Hebrew PDF→PDF translation (layout-preserving) runs through a standalone
`timluli-pdf.exe` sidecar compiled from `src-tauri/sidecar/timluli_pdf.py` with
PyInstaller (PyMuPDF + python-bidi). The exe is **not** committed — build it once
before `tauri:dev`/`tauri:build`, since Tauri copies `src-tauri/resources/` at
compile time.

Requires the `py -3.12` launcher in PATH. All Python deps install into a throwaway
`.build-venv` — nothing touches your system Python.

```powershell
# From repo root:
src-tauri\sidecar\build_sidecar.ps1
# Produces src-tauri/resources/timluli-pdf.exe
```

If the exe is missing at runtime, Hebrew PDF translation returns a clear error
("מנוע ה-PDF ... לא נמצא"); other formats are unaffected.

### 7. ONNX Runtime DLL (for Hebrew auto-punctuation)

The optional auto-punctuation engine runs an ONNX model in-process via the `ort`
crate in **load-dynamic** mode — it is **not** statically linked, so it needs
`onnxruntime.dll` at runtime, bundled app-locally next to the exe (via
`bundle.resources` `"onnxruntime/*.dll" → root`, exactly like `vcruntime/`).

The DLL **must be 1.22.x** to match `ort` 2.0.0-rc.10. A working copy is committed at
`src-tauri/onnxruntime/onnxruntime.dll` (local-build fallback); CI overwrites it from
the official `microsoft/onnxruntime` v1.22.1 release so the shipped DLL is a clean
build (see release.yml step "Bundle ONNX Runtime"). For local `tauri:dev`, the DLL is
picked up from next to the dev exe — copy it into `src-tauri/target/debug/` if a fresh
checkout lacks it.

> Note: `tokenizers` is built with `default-features = false, features = ["onig"]` on
> purpose — the default `esaxx_fast` feature pulls a C++ dep compiled with the static
> CRT (/MT) that clashes with whisper-rs-sys's dynamic CRT (/MD) → `LNK2038`.

The punctuation **model** itself (~280 MB ONNX + tokenizer) is downloaded on demand at
runtime from the GitHub release; it is never bundled in the installer.

---

## Build Steps

```powershell
# Install npm dependencies
npm install

# Build the PDF translation sidecar (once, and after editing timluli_pdf.py)
src-tauri\sidecar\build_sidecar.ps1

# Development server (live reload)
npm run tauri:dev

# Production build
npm run tauri:build
```

---

## Common Build Errors

### `cmake` is not installed

```
error: failed to execute command: program not found
is `cmake` not installed?
```

Install CMake (see §3 above) and ensure it is in PATH.

### `LIBCLANG_PATH` / bindgen errors

```
error: failed to run custom build command for `whisper-rs-sys`
...
libclang: path search unsuccessful
```

Set `LIBCLANG_PATH` to the directory containing `libclang.dll` (see §4 above).
Restart your shell after setting the variable.

### Out-of-memory during compilation

Add to `src-tauri/Cargo.toml`:
```toml
[profile.dev]
debug = 0        # already set — reduces peak RAM
codegen-units = 4
```

---

## SHA-256 Verification for Model Catalog

Before shipping a release, download each model from HuggingFace and compute its hash:

```powershell
# PowerShell
Get-FileHash .\ggml-model-q5_0.bin -Algorithm SHA256
Get-FileHash .\ggml-model.bin      -Algorithm SHA256
```

Replace the `TBD_FILL_AFTER_VERIFICATION` placeholders in
`src-tauri/resources/models.toml` with the computed hashes.

Verify HF filenames against the live repo before every release:
https://huggingface.co/ivrit-ai/whisper-large-v3-turbo-ggml/tree/main

---

## WebView2

Bundled automatically in Windows 11. On older Windows 10, the NSIS/MSI installer
bootstraps WebView2 at install time via `embedBootstrapper` (configured in
`tauri.conf.json`).
