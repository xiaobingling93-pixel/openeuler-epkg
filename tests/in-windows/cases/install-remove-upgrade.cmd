@echo off
REM Test install/remove/upgrade functionality
REM Native Windows version - simplified version of the in-vm test

setlocal EnableDelayedExpansion

set "TEST_CHANNEL=msys2"
set "TEST_PKG1=jq"
set "TEST_PKG2=tree"
set "TEST_PKG3=curl"

echo Starting install/remove/upgrade test
echo Using channel: %TEST_CHANNEL%

set "ENV_NAME=test-iur-win"

echo Creating environment: %ENV_NAME%
"%EPKG_BINARY%" env remove "%ENV_NAME%" 2>nul

"%EPKG_BINARY%" env create "%ENV_NAME%" -c %TEST_CHANNEL%
if errorlevel 1 (
    echo ERROR: Failed to create environment
    exit /b 1
)

REM Test help commands
echo Testing epkg --help
"%EPKG_BINARY%" --help >nul
if errorlevel 1 (
    echo ERROR: epkg --help failed
    goto :cleanup
)

"%EPKG_BINARY%" install --help >nul
if errorlevel 1 (
    echo ERROR: epkg install --help failed
    goto :cleanup
)

"%EPKG_BINARY%" remove --help >nul
if errorlevel 1 (
    echo ERROR: epkg remove --help failed
    goto :cleanup
)

echo Help commands test passed

REM Test install
echo Test 1: Install %TEST_PKG1%
"%EPKG_BINARY%" -e "%ENV_NAME%" --assume-yes install %TEST_PKG1%
if errorlevel 1 (
    echo ERROR: Failed to install %TEST_PKG1%
    goto :cleanup
)

REM Verify package is installed
echo Verifying %TEST_PKG1% is installed
"%EPKG_BINARY%" -e "%ENV_NAME%" run %TEST_PKG1% --version
if errorlevel 1 (
    echo ERROR: %TEST_PKG1% not found after install
    goto :cleanup
)

REM Test install multiple packages
echo Test 2: Install multiple packages %TEST_PKG2% %TEST_PKG3%
"%EPKG_BINARY%" -e "%ENV_NAME%" --assume-yes install %TEST_PKG2% %TEST_PKG3%
if errorlevel 1 (
    echo ERROR: Failed to install %TEST_PKG2% and %TEST_PKG3%
    goto :cleanup
)

REM Verify packages are installed
"%EPKG_BINARY%" -e "%ENV_NAME%" run %TEST_PKG2% --version
if errorlevel 1 (
    echo ERROR: %TEST_PKG2% not found after install
    goto :cleanup
)

"%EPKG_BINARY%" -e "%ENV_NAME%" run %TEST_PKG3% --version
if errorlevel 1 (
    echo ERROR: %TEST_PKG3% not found after install
    goto :cleanup
)

REM Test list
echo Test 3: List installed packages
"%EPKG_BINARY%" -e "%ENV_NAME%" list --installed > "%TEMP%\installed.txt"
findstr /C:"%TEST_PKG1%" "%TEMP%\installed.txt" >nul
if errorlevel 1 (
    echo ERROR: %TEST_PKG1% not in installed list
    goto :cleanup
)

echo List test passed

REM Test remove
echo Test 4: Remove %TEST_PKG3%
"%EPKG_BINARY%" -e "%ENV_NAME%" --assume-yes remove %TEST_PKG3%
if errorlevel 1 (
    echo ERROR: Failed to remove %TEST_PKG3%
    goto :cleanup
)

REM Verify package is removed
"%EPKG_BINARY%" -e "%ENV_NAME%" run %TEST_PKG3% --version >nul 2>&1
if not errorlevel 1 (
    echo ERROR: %TEST_PKG3% still found after remove
    goto :cleanup
)

echo Remove test passed

REM Test reinstall (install again)
echo Test 5: Reinstall %TEST_PKG3%
"%EPKG_BINARY%" -e "%ENV_NAME%" --assume-yes install %TEST_PKG3%
if errorlevel 1 (
    echo ERROR: Failed to reinstall %TEST_PKG3%
    goto :cleanup
)

"%EPKG_BINARY%" -e "%ENV_NAME%" run %TEST_PKG3% --version
if errorlevel 1 (
    echo ERROR: %TEST_PKG3% not found after reinstall
    goto :cleanup
)

echo Reinstall test passed

REM Test upgrade (check for upgradable, but don't fail if none)
echo Test 6: Check for upgradable packages
"%EPKG_BINARY%" -e "%ENV_NAME%" list --upgradable > "%TEMP%\upgradable.txt"
echo Upgrade check completed (packages may or may not be available)

REM Test dry-run
echo Test 7: Dry run remove %TEST_PKG1%
"%EPKG_BINARY%" -e "%ENV_NAME%" --dry-run remove %TEST_PKG1%
if errorlevel 1 (
    echo ERROR: Dry run failed
    goto :cleanup
)

REM Verify dry-run didn't actually remove
echo Verifying dry-run didn't remove package
"%EPKG_BINARY%" -e "%ENV_NAME%" run %TEST_PKG1% --version
if errorlevel 1 (
    echo ERROR: %TEST_PKG1% was removed by dry-run (should not happen)
    goto :cleanup
)

echo Dry-run test passed

echo.
echo ========================================
echo All install/remove/upgrade tests passed!
echo ========================================

:cleanup
echo Cleaning up test environment
"%EPKG_BINARY%" env remove "%ENV_NAME%" 2>nul
del "%TEMP%\installed.txt" 2>nul
del "%TEMP%\upgradable.txt" 2>nul
exit /b 0
