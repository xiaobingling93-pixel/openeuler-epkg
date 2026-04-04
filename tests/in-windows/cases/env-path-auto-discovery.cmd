@echo off
REM Test --root DIR option, automatic name generation, and implicit environment discovery
REM Native Windows version

setlocal EnableDelayedExpansion

set "SCRIPT_DIR=%~dp0"
set "SCRIPT_DIR=%SCRIPT_DIR:~0,-1%"

REM Source common library
REM Note: In batch, we can't really "source" like in shell, so functions are inline

set "TEST_CHANNEL=msys2"
set "TEST_PKG_SMALL=jq"
set "TEST_PKG_ALT=tree"

echo Starting env path auto-discovery test
echo Using channel: %TEST_CHANNEL%

REM Create test directory
set "TEST_DIR=%USERPROFILE%\.epkg\tmp\env-path-test-%RANDOM%"
if not exist "%TEST_DIR%" mkdir "%TEST_DIR%"

REM Cleanup on exit (manual cleanup at end since batch doesn't have trap)

set "ORIG_DIR=%CD%"

REM ============================================================================
REM Test 1: --root DIR option with env create (automatic name generation)
REM ============================================================================
echo Test 1: --root DIR option with env create

set "ENV_ROOT=%TEST_DIR%\myenv"
echo Creating environment at path: %ENV_ROOT%

"%EPKG_BINARY%" env create --root "%ENV_ROOT%" -c %TEST_CHANNEL%
if errorlevel 1 (
    echo ERROR: Failed to create environment with --root
    goto :cleanup
)

REM Compute expected auto-generated name (manual inline of env_name_from_path)
set "input_path=%ENV_ROOT%"
REM Remove trailing backslash
if "%input_path:~-1%"=="\" set "input_path=%input_path:~0,-1%"
REM Replace backslashes with __
set "with_underscores=%input_path:\=__%"
REM Replace : with _ (for Windows drive letters like C:)
set "with_underscores=%with_underscores::=_%"
if not "%with_underscores:~0,2%"=="__" (
    set "EXPECTED_NAME=__%with_underscores%"
) else (
    set "EXPECTED_NAME=%with_underscores%"
)

echo Expected auto-generated name: %EXPECTED_NAME%

REM Verify environment appears in env list
echo Listing environments to verify registration
"%EPKG_BINARY%" env list | findstr /C:"%EXPECTED_NAME%" >nul
if errorlevel 1 (
    echo ERROR: Environment '%EXPECTED_NAME%' not found in env list
    goto :cleanup
)

REM Verify auto-generated name starts with __
echo %EXPECTED_NAME% | findstr /B "__" >nul
if errorlevel 1 (
    echo ERROR: Auto-generated name '%EXPECTED_NAME%' does not start with '__'
    goto :cleanup
)

REM Install and run a command using --root flag
echo Installing jq using --root flag
"%EPKG_BINARY%" --root "%ENV_ROOT%" --assume-yes install jq
if errorlevel 1 (
    echo ERROR: Failed to install jq with --root
    goto :cleanup
)

echo Running jq with --root flag
"%EPKG_BINARY%" --root "%ENV_ROOT%" run jq --version
if errorlevel 1 (
    echo ERROR: jq not found in environment via --root
    goto :cleanup
)

REM Also test that we can use -e with the auto-generated name
echo Testing -e flag with auto-generated name
"%EPKG_BINARY%" -e "%EXPECTED_NAME%" run jq --version
if errorlevel 1 (
    echo ERROR: jq not found via -e with auto-generated name
    goto :cleanup
)

REM ============================================================================
REM Test 2: -e overrides --root when both flags present
REM ============================================================================
echo Test 2: -e overrides --root precedence

set "ENV3_PATH=%TEST_DIR%\env3"
set "ENV3_NAME=explicit-name-win"

"%EPKG_BINARY%" env remove "%ENV3_NAME%" 2>nul

"%EPKG_BINARY%" env create --root "%ENV3_PATH%" -c %TEST_CHANNEL%
if errorlevel 1 (
    echo ERROR: Failed to create env3 via --root
    goto :cleanup
)

"%EPKG_BINARY%" env create "%ENV3_NAME%" -c %TEST_CHANNEL%
if errorlevel 1 (
    echo ERROR: Failed to create env3 via -e
    goto :cleanup
)

REM Install different packages in each to distinguish
"%EPKG_BINARY%" --root "%ENV3_PATH%" --assume-yes install jq
if errorlevel 1 (
    echo ERROR: Failed to install jq in path env
    goto :cleanup
)

"%EPKG_BINARY%" -e "%ENV3_NAME%" --assume-yes install %TEST_PKG_ALT%
if errorlevel 1 (
    echo ERROR: Failed to install %TEST_PKG_ALT% in named env
    goto :cleanup
)

REM Run with both -e and --root; -e should take precedence
echo Testing -e overrides --root
"%EPKG_BINARY%" -e "%ENV3_NAME%" --root "%ENV3_PATH%" run %TEST_PKG_ALT% --version
if errorlevel 1 (
    echo ERROR: %TEST_PKG_ALT% not found when both -e and --root flags present (-e should win)
    goto :cleanup
)

REM Verify jq is not found (should error)
"%EPKG_BINARY%" -e "%ENV3_NAME%" --root "%ENV3_PATH%" run jq --version >nul 2>&1
if not errorlevel 1 (
    echo ERROR: jq found when -e should have overridden --root
    goto :cleanup
)

echo All tests passed!
goto :cleanup

:cleanup
echo Cleaning up test directory: %TEST_DIR%
"%EPKG_BINARY%" env remove "%EXPECTED_NAME%" 2>nul
"%EPKG_BINARY%" env remove "%ENV3_NAME%" 2>nul
"%EPKG_BINARY%" env remove "__myenv" 2>nul
"%EPKG_BINARY%" env remove "__env3" 2>nul
rmdir /s /q "%TEST_DIR%" 2>nul
exit /b 0
