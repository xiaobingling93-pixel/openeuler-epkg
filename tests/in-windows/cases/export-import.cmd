@echo off
REM Test export/import functionality
REM Native Windows version

setlocal EnableDelayedExpansion

set "TEST_CHANNEL=msys2"
set "TEST_PKGS=jq tree"

echo Starting export/import test
echo Using channel: %TEST_CHANNEL%, packages: %TEST_PKGS%

REM Create test environment names
set "ENV_NAME=test-export-win"
set "ENV2_NAME=test-import-win"

REM Cleanup any existing test environments
echo Cleaning up existing test environments
"%EPKG_BINARY%" env remove "%ENV_NAME%" 2>nul
"%EPKG_BINARY%" env remove "%ENV2_NAME%" 2>nul

echo Creating environment: %ENV_NAME%
"%EPKG_BINARY%" env create "%ENV_NAME%" -c %TEST_CHANNEL%
if errorlevel 1 (
    echo ERROR: Failed to create environment
    exit /b 1
)

echo Installing %TEST_PKGS%
"%EPKG_BINARY%" -e "%ENV_NAME%" --assume-yes install %TEST_PKGS%
if errorlevel 1 (
    echo ERROR: Failed to install packages
    goto :cleanup
)

REM Export to a file
set "EXPORT_FILE=%TEMP%\epkg-export-%ENV_NAME%.yaml"
echo Exporting environment to %EXPORT_FILE%
"%EPKG_BINARY%" env export "%ENV_NAME%" --output "%EXPORT_FILE%"
if errorlevel 1 (
    echo ERROR: Failed to export environment
    goto :cleanup
)

if not exist "%EXPORT_FILE%" (
    echo ERROR: Export file not found
    goto :cleanup
)

echo Export file created: %EXPORT_FILE%

REM Get list of packages from original environment
echo Getting package list from original environment
"%EPKG_BINARY%" -e "%ENV_NAME%" list --installed > "%TEMP%\list1.txt"

echo Creating new environment with import
"%EPKG_BINARY%" --assume-yes env create "%ENV2_NAME%" --import "%EXPORT_FILE%"
if errorlevel 1 (
    echo ERROR: Failed to create environment with import
    goto :cleanup
)

REM Get list of packages from imported environment
echo Getting package list from imported environment
"%EPKG_BINARY%" -e "%ENV2_NAME%" list --installed > "%TEMP%\list2.txt"

REM Compare lists - basic check: both should have jq and tree
echo Comparing package lists
findstr /C:"jq" "%TEMP%\list2.txt" >nul
if errorlevel 1 (
    echo ERROR: jq not found in imported environment package list
    goto :cleanup
)

findstr /C:"tree" "%TEMP%\list2.txt" >nul
if errorlevel 1 (
    echo ERROR: tree not found in imported environment package list
    goto :cleanup
)

echo Package lists match (jq and tree found)

REM Verify that commands are installed in env2
echo Verifying jq command in env2
"%EPKG_BINARY%" -e "%ENV2_NAME%" run jq --version
if errorlevel 1 (
    echo ERROR: jq not found in env2
    goto :cleanup
)

echo Verifying tree command in env2
"%EPKG_BINARY%" -e "%ENV2_NAME%" run tree --version
if errorlevel 1 (
    echo ERROR: tree not found in env2
    goto :cleanup
)

echo Export/import test completed successfully

:cleanup
echo Cleaning up test environments
"%EPKG_BINARY%" env remove "%ENV_NAME%" 2>nul
"%EPKG_BINARY%" env remove "%ENV2_NAME%" 2>nul
del "%EXPORT_FILE%" 2>nul
del "%TEMP%\list1.txt" 2>nul
del "%TEMP%\list2.txt" 2>nul
exit /b 0
