@echo off
chcp 65001 >nul
cd /d "%~dp0"

echo.
echo  Dibo — מפעיל...
echo.

:: ── הוספת נתיבים ──────────────────────────────────────────────────────────
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
set "PATH=C:\Program Files\nodejs;%PATH%"

:: ── בדיקת node ───────────────────────────────────────────────────────────
where node >nul 2>&1
if errorlevel 1 (
    echo  שגיאה: node.js לא נמצא.
    echo  הורד והתקן מ: https://nodejs.org
    pause
    exit /b 1
)

:: ── בדיקת cargo ──────────────────────────────────────────────────────────
where cargo >nul 2>&1
if errorlevel 1 (
    echo  שגיאה: Rust/Cargo לא נמצא.
    echo  הורד והתקן מ: https://rustup.rs
    pause
    exit /b 1
)

:: ── npm install אם צריך ──────────────────────────────────────────────────
if not exist "node_modules" (
    echo  מתקין תלויות npm...
    npm install
    if errorlevel 1 (
        echo  שגיאה בהתקנת תלויות. בדוק חיבור אינטרנט.
        pause
        exit /b 1
    )
)

:: ── הפעלה ────────────────────────────────────────────────────────────────
echo  מפעיל Dibo...
npm run tauri:dev

if errorlevel 1 (
    echo.
    echo  הפעלה נכשלה — ראה שגיאות למעלה.
    pause
)
