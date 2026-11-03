@echo off
rem ============================================================
rem  [optional] x86_64-pc-windows-gnu local debug build
rem  - all paths derive from this script's own location (%~dp0)
rem  - output dir: D:\tools\oread-target-gnu  (override: OREAD_TARGET_GNU)
rem  - needs: rustup target x86_64-pc-windows-gnu + MinGW-w64 gcc on PATH
rem  - for everyday msvc-host development use scripts\build-dev.ps1 instead
rem  - messages are ASCII on purpose: .bat cannot rely on UTF-8 codepage
rem ============================================================
setlocal EnableExtensions

pushd "%~dp0"
if errorlevel 1 (
    echo [ERROR] cannot enter script dir %~dp0
    exit /b 1
)

rem ---- target dir convention ----
set "CARGO_TARGET_DIR=D:\tools\oread-target-gnu"
if defined OREAD_TARGET_GNU set "CARGO_TARGET_DIR=%OREAD_TARGET_GNU%"

rem ---- MinGW discovery (gnu target only) ----
if not defined OREAD_MINGW if exist "D:\tools\MinGW\mingw64\bin\gcc.exe" set "OREAD_MINGW=D:\tools\MinGW\mingw64"
if defined OREAD_MINGW set "PATH=%OREAD_MINGW%\bin;%PATH%"

rem ---- tool checks ----
where /q cargo
if errorlevel 1 (
    echo [ERROR] cargo not found on PATH
    echo    - install rustup: https://rustup.rs/
    echo    - reopen the terminal after installing ^(PATH only affects new processes^)
    goto :fail
)

where /q rustup
if errorlevel 1 (
    echo [ERROR] rustup not found on PATH
    echo    - cannot add the gnu target; make sure CARGO_HOME\bin is on PATH
    goto :fail
)

where /q x86_64-w64-mingw32-gcc
if errorlevel 1 (
    where /q gcc
    if errorlevel 1 (
        echo [ERROR] MinGW-w64 compiler not found ^(x86_64-w64-mingw32-gcc.exe or gcc.exe^)
        echo    - install WinLibs GCC, e.g. extract to D:\tools\MinGW\mingw64
        echo    - add D:\tools\MinGW\mingw64\bin to PATH, or set OREAD_MINGW to that dir
        echo    - for local debugging only, prefer scripts\build-dev.ps1 ^(msvc host, no MinGW^)
        goto :fail
    )
)

rem ---- target check (idempotent) ----
rustup target list --installed | findstr /x /c:"x86_64-pc-windows-gnu" >nul
if errorlevel 1 (
    echo [INFO] target x86_64-pc-windows-gnu missing, adding it now...
    rustup target add x86_64-pc-windows-gnu
    if errorlevel 1 (
        echo [ERROR] rustup target add failed
        echo    - check network/mirror, try RUSTUP_DIST_SERVER=https://mirrors.ustc.edu.cn/rust-static
        goto :fail
    )
)

rem ---- build ----
echo === build start %DATE% %TIME% ===
echo [build_gnu] CARGO_TARGET_DIR=%CARGO_TARGET_DIR%
cargo build --target x86_64-pc-windows-gnu
if errorlevel 1 (
    echo [ERROR] cargo build --target x86_64-pc-windows-gnu failed
    echo    - see docs\BUILD.md "common errors" section
    goto :fail
)

set "ARTIFACT=%CARGO_TARGET_DIR%\x86_64-pc-windows-gnu\debug\boranlib.exe"
if exist "%ARTIFACT%" (
    echo [build_gnu] artifact: %ARTIFACT%
) else (
    echo [WARN] cargo returned 0 but artifact not found: %ARTIFACT%
    echo        check [[bin]] name in Cargo.toml
)

popd
exit /b 0

:fail
popd
exit /b 1
