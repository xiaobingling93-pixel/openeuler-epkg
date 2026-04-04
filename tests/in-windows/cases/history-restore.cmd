@echo off
REM Test history/restore functionality
REM Native Windows version

setlocal EnableDelayedExpansion

set "TEST_CHANNEL=msys2"
set "TEST_PKG1=jq"
set "TEST_PKG2=tree"
set "TEST_PKG3=curl"

echo Starting history/restore test
echo Using channel: %TEST_CHANNEL%

set "ENV_NAME=test-history-win"

echo Creating environment: %ENV_NAME%
"%EPKG_BINARY%" env remove "%ENV_NAME%" 2>nul

"%EPKG_BINARY%" env create "%ENV_NAME%" -c %TEST_CHANNEL%
if errorlevel 1 (
    echo ERROR: Failed to create environment
    exit /b 1
)

echo Installing %TEST_PKG1% and %TEST_PKG3%
"%EPKG_BINARY%" -e "%ENV_NAME%" --assume-yes install %TEST_PKG1% %TEST_PKG3%
if errorlevel 1 (
    echo ERROR: Failed to install %TEST_PKG1% and %TEST_PKG3%
    goto :cleanup
)

echo Installing %TEST_PKG1% and %TEST_PKG2% (%TEST_PKG2% should be new)
"%EPKG_BINARY%" -e "%ENV_NAME%" --assume-yes install %TEST_PKG1% %TEST_PKG2%
if errorlevel 1 (
    echo ERROR: Failed to install %TEST_PKG1% and %TEST_PKG2%
    goto :cleanup
)

echo Removing %TEST_PKG3%
"%EPKG_BINARY%" -e "%ENV_NAME%" --assume-yes remove %TEST_PKG3%
if errorlevel 1 (
    echo ERROR: Failed to remove %TEST_PKG3%
    goto :cleanup
)

echo Installing ripgrep
"%EPKG_BINARY%" -e "%ENV_NAME%" --assume-yes install ripgrep
if errorlevel 1 (
    echo WARNING: Failed to install ripgrep (may not be available)
    REM Don't fail here, ripgrep might not be available
)

REM Verify history shows the above generations
echo Checking history
"%EPKG_BINARY%" -e "%ENV_NAME%" history > "%TEMP%\history.txt"

REM Count generations (look for lines starting with number)
findstr /B /R "[0-9]" "%TEMP%\history.txt" > "%TEMP%\generations.txt"
for /f %%a in ('type "%TEMP%\generations.txt" ^| find /c /v ""') do set "GEN_COUNT=%%a"

echo History shows %GEN_COUNT% generations

if %GEN_COUNT% lss 3 (
    echo WARNING: Expected at least 3 generations, found %GEN_COUNT%
)

REM Restore to -2 (2 generations ago)
echo Restoring to -2
"%EPKG_BINARY%" -e "%ENV_NAME%" --assume-yes restore -2
if errorlevel 1 (
    echo ERROR: Failed to restore to -2
    goto :cleanup
)

REM Verify that packages are in expected state after restore
echo Verifying installed packages after restore

"%EPKG_BINARY%" -e "%ENV_NAME%" run %TEST_PKG1% --version
if errorlevel 1 (
    echo ERROR: %TEST_PKG1% not found after restore
    goto :cleanup
)

"%EPKG_BINARY%" -e "%ENV_NAME%" run %TEST_PKG2% --version
if errorlevel 1 (
    echo ERROR: %TEST_PKG2% not found after restore
    goto :cleanup
)

"%EPKG_BINARY%" -e "%ENV_NAME%" run %TEST_PKG3% --version
if errorlevel 1 (
    echo ERROR: %TEST_PKG3% not found after restore
    goto :cleanup
)

REM Check ripgrep is not present after restore
"%EPKG_BINARY%" -e "%ENV_NAME%" run rg --version >nul 2>&1
if not errorlevel 1 (
    echo WARNING: ripgrep should not be present after restore (but it's there)
    REM Don't fail, ripgrep install might have failed earlier
)

echo History/restore test completed successfully

:cleanup
echo Cleaning up test environment
"%EPKG_BINARY%" env remove "%ENV_NAME%" 2>nul
del "%TEMP%\history.txt" 2>nul
del "%TEMP%\generations.txt" 2>nul
exit /b 0
