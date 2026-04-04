@echo off
REM Main entrance script for Windows native tests
REM Usage: run-tests.cmd [test-name]

setlocal EnableDelayedExpansion

REM Get script directory
set "SCRIPT_DIR=%~dp0"
set "SCRIPT_DIR=%SCRIPT_DIR:~0,-1%"

REM Default epkg binary path (can be overridden via EPKG_BIN environment variable)
if "%~1"=="" (
    set "EPKG_BINARY=%EPKG_BIN%"
) else (
    set "EPKG_BINARY=%~1"
)

if "!EPKG_BINARY!"=="" (
    REM Try to find epkg.exe in common locations
    if exist "C:\Users\%USERNAME%\.epkg\envs\self\usr\bin\epkg.exe" (
        set "EPKG_BINARY=C:\Users\%USERNAME%\.epkg\envs\self\usr\bin\epkg.exe"
    ) else (
        echo ERROR: EPKG_BIN not set and epkg.exe not found in default location
        echo Usage: run-tests.cmd [path\to\epkg.exe]
        exit /b 1
    )
)

echo ========================================
echo Running Windows Native Tests
echo EPKG_BINARY: !EPKG_BINARY!
echo ========================================
echo.

REM Run each test case
set "TEST_DIR=%SCRIPT_DIR%\cases"
set "ALL_PASSED=1"

echo [1/4] Running env-path-auto-discovery test...
call "%TEST_DIR%\env-path-auto-discovery.cmd" > "%TEMP%\env-path-auto-discovery.log" 2>&1
if errorlevel 1 (
    echo FAILED: env-path-auto-discovery test
    type "%TEMP%\env-path-auto-discovery.log"
    set "ALL_PASSED=0"
) else (
    echo PASSED: env-path-auto-discovery test
)
echo.

echo [2/4] Running export-import test...
call "%TEST_DIR%\export-import.cmd" > "%TEMP%\export-import.log" 2>&1
if errorlevel 1 (
    echo FAILED: export-import test
    type "%TEMP%\export-import.log"
    set "ALL_PASSED=0"
) else (
    echo PASSED: export-import test
)
echo.

echo [3/4] Running history-restore test...
call "%TEST_DIR%\history-restore.cmd" > "%TEMP%\history-restore.log" 2>&1
if errorlevel 1 (
    echo FAILED: history-restore test
    type "%TEMP%\history-restore.log"
    set "ALL_PASSED=0"
) else (
    echo PASSED: history-restore test
)
echo.

echo [4/4] Running install-remove-upgrade test...
call "%TEST_DIR%\install-remove-upgrade.cmd" > "%TEMP%\install-remove-upgrade.log" 2>&1
if errorlevel 1 (
    echo FAILED: install-remove-upgrade test
    type "%TEMP%\install-remove-upgrade.log"
    set "ALL_PASSED=0"
) else (
    echo PASSED: install-remove-upgrade test
)
echo.

echo ========================================
if "!ALL_PASSED!"=="1" (
    echo All tests PASSED
    exit /b 0
) else (
    echo Some tests FAILED
    exit /b 1
)
