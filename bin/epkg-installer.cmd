@echo off
REM SPDX-License-Identifier: MulanPSL-2.0+
REM Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

setlocal EnableDelayedExpansion

REM Global variables
for /f "tokens=*" %%a in ('uname -m 2^>nul ^|^| echo x86_64') do set "ARCH=%%a"
set "EPKG_STATIC=epkg"
set "EPKG_CACHE=%USERPROFILE%\.cache\epkg\downloads\epkg"
set "GITEE_API_BASE=https://gitee.com/api/v5/repos"
set "GITEE_OWNER=wu_fengguang"
set "GITEE_REPO=epkg"

REM Default values
set "CHANNEL="
set "STORE_MODE=auto"
set "EPKG_PATH="

goto :main

REM Functions
:print_step
    echo ^>^> %~1
    goto :eof

:print_info
    echo %~1
    goto :eof

:print_error
    echo ERROR: %~1 >&2
    exit /b 1

:check_architecture
    if "%ARCH%"=="x86_64" goto :arch_ok
    if "%ARCH%"=="amd64" goto :arch_ok
    if "%ARCH%"=="aarch64" goto :arch_ok
    if "%ARCH%"=="arm64" goto :arch_ok
    call :print_error "Unsupported architecture: %ARCH%"
    :arch_ok
    goto :eof

:normalize_arch
    if "%ARCH%"=="amd64" set "ARCH=x86_64"
    if "%ARCH%"=="arm64" set "ARCH=aarch64"
    goto :eof

:detect_os_family
    echo windows
    goto :eof

:parse_args
    :parse_loop
    if "%~1"=="" goto :eof
    if "%~1"=="-c" (
        if "%~2"=="" call :print_error "Option %~1 requires an argument"
        set "CHANNEL=%~2"
        shift
        shift
        goto :parse_loop
    )
    if "%~1"=="--channel" (
        if "%~2"=="" call :print_error "Option %~1 requires an argument"
        set "CHANNEL=%~2"
        shift
        shift
        goto :parse_loop
    )
    if "%~1"=="--store" (
        if "%~2"=="" call :print_error "Option %~1 requires an argument"
        if "%~2"=="shared" goto :store_ok
        if "%~2"=="private" goto :store_ok
        if "%~2"=="auto" goto :store_ok
        call :print_error "Invalid store mode: %~2. Must be one of: shared, private, auto"
        :store_ok
        set "STORE_MODE=%~2"
        shift
        shift
        goto :parse_loop
    )
    if "%~1"=="-h" goto :show_help
    if "%~1"=="--help" goto :show_help
    call :print_error "Unknown option: %~1"
    goto :parse_loop

:show_help
    echo Usage: %~nx0 [OPTIONS]
    echo.
    echo Options:
    echo   -c, --channel CHANNEL   Set the channel for the main environment
    echo   --store MODE            Store mode: shared, private, or auto (default: auto)
    echo   -h, --help              Show this help message
    echo.
    echo Examples:
    echo   %~nx0 --channel conda
    echo   %~nx0 --store shared
    echo   %~nx0 --channel msys2 --store private
    exit /b 0

:setup_environment
    if not exist "%EPKG_CACHE%" mkdir "%EPKG_CACHE%" 2>nul
    goto :eof

:check_git_tree
    set "SCRIPT_DIR=%~dp0"
    REM Go up one level since script is in bin\
    for %%i in ("%SCRIPT_DIR%..") do set "PROJECT_ROOT=%%~fi"
    if exist "%PROJECT_ROOT%\.git" (
        if exist "%PROJECT_ROOT%\target\debug\epkg.exe" (
            set "EPKG_PATH=%PROJECT_ROOT%\target\debug\epkg.exe"
            exit /b 0
        )
    )
    exit /b 1

:fetch_latest_release
    set "api_url=%GITEE_API_BASE%/%GITEE_OWNER%/%GITEE_REPO%/releases/latest"
    set "temp_file=%TEMP%\epkg_release.json"

    curl -s --connect-timeout 15 --max-time 30 "%api_url%" -o "%temp_file%" 2>nul
    if errorlevel 1 (
        call :print_error "Failed to fetch release info from Gitee API: %api_url%"
    )

    REM Parse tag_name from JSON using findstr
    set "tag_name="
    for /f "tokens=*" %%a in ('type "%temp_file%" ^| findstr "tag_name"') do (
        set "line=%%a"
        REM Extract value between quotes after tag_name
        for /f "tokens=2 delims=:" %%b in ("%%a") do (
            set "val=%%b"
            set "val=!val:"=!"
            set "val=!val: =!"
            set "tag_name=!val!"
        )
    )
    del "%temp_file%" 2>nul

    if "!tag_name!"=="" (
        call :print_error "Failed to parse release tag from Gitee API response"
    )
    echo !tag_name!
    goto :eof

:download_epkg_asset
    set "asset_name=%~1"
    set "latest_version=%~2"

    set "binary_url=https://gitee.com/%GITEE_OWNER%/%GITEE_REPO%/releases/download/%latest_version%/%asset_name%"
    set "sha_url=%binary_url%.sha256"
    set "sha_file=%asset_name%.sha256"

    echo.
    echo Downloading %sha_file% ...
    if exist "./%sha_file%" del "./%sha_file%" 2>nul
    if exist "./%asset_name%" del "./%asset_name%" 2>nul

    curl -L -# -o "./%sha_file%" "%sha_url%" --connect-timeout 15 --max-time 30 2>nul
    if errorlevel 1 exit /b 1

    REM Check if file is empty
    for %%F in ("./%sha_file%") do set "size=%%~zF"
    if "%size%"=="0" exit /b 1

    REM Check if file contains HTML error page
    findstr /i "<html <!DOCTYPE <body" "./%sha_file%" >nul 2>&1
    if not errorlevel 1 exit /b 1

    REM Check if file contains JSON error
    findstr "{" "./%sha_file%" >nul 2>&1
    if not errorlevel 1 exit /b 1

    echo Downloading %asset_name% ...
    curl -L -# -o "./%asset_name%" "%binary_url%" --retry 5 --connect-timeout 15 --max-time 300 2>nul
    if errorlevel 1 exit /b 1

    REM Verify checksum if certutil is available
    certutil -? >nul 2>&1
    if errorlevel 1 goto :no_certutil

    REM Calculate SHA256 and verify
    for /f "skip=1 tokens=*" %%a in ('certutil -hashfile "./%asset_name%" SHA256 2^>nul') do (
        if not defined computed_hash set "computed_hash=%%a"
    )
    set "computed_hash=!computed_hash: =!"

    REM Read expected hash from sha file
    for /f "tokens=1" %%a in ('type "./%sha_file%"') do set "expected_hash=%%a"

    if /i not "!computed_hash!"=="!expected_hash!" (
        echo Checksum verification failed
        exit /b 1
    )
    :no_certutil

    exit /b 0

:download_files
    REM Skip download if running from git tree
    call :check_git_tree
    if not errorlevel 1 (
        echo.
        call :print_info "Using local binary from git tree: %EPKG_PATH%"
        goto :eof
    )

    call :print_info "Fetching latest release from Gitee..."
    for /f "tokens=*" %%a in ('call :fetch_latest_release') do set "latest_version=%%a"
    if errorlevel 1 exit /b 1

    set "ASSET_NAME=%EPKG_STATIC%-windows-%ARCH%.exe"

    cd /d "%EPKG_CACHE%" 2>nul || exit /b 1

    echo.
    call :print_info "Latest release: %latest_version%"
    call :print_info "Destination: %EPKG_CACHE%"

    call :download_epkg_asset "%ASSET_NAME%" "%latest_version%"
    if errorlevel 1 (
        call :print_error "Failed to download epkg binary for windows/%ARCH% (%ASSET_NAME%)"
    )
    set "EPKG_PATH=.\%ASSET_NAME%"
    goto :eof

:initialize_epkg
    REM Build the install command with options
    set "install_cmd=%EPKG_PATH% self install --store=%STORE_MODE%"

    REM Add channel option if specified
    if not "%CHANNEL%"=="" (
        set "install_cmd=%install_cmd% --channel=%CHANNEL%"
    )

    echo.
    REM Check if running as admin
    net session >nul 2>&1
    if not errorlevel 1 (
        call :print_info "Installation mode: shared (system-wide)"
    ) else (
        call :print_info "Installation mode: private (user-local)"
    )

    REM Show what we're doing
    if not "%CHANNEL%"=="" (
        call :print_info "Installing epkg with channel: %CHANNEL%"
    )
    call :print_info "Store mode: %STORE_MODE%"

    REM Install epkg
    %install_cmd% || exit /b 1
    goto :eof

:print_completion
    echo.
    echo =================================================
    echo               Installation Complete
    echo =================================================
    call :print_info "Usage:"
    call :print_info "  epkg search [pattern]  - Search for packages"
    call :print_info "  epkg install [pkg]     - Install packages"
    call :print_info "  epkg remove [pkg]      - Remove packages"
    call :print_info "  epkg list              - List packages"
    call :print_info "  epkg update            - Update repo data"
    call :print_info "  epkg upgrade           - Upgrade packages"
    call :print_info "  epkg --help            - Show detailed help"
    goto :eof

:check_duplicate_install
    if exist "%USERPROFILE%\.epkg\envs\main" (
        echo epkg was already initialized for current user
        echo TO upgrade epkg: epkg self upgrade
        echo TO uninstall epkg: epkg self remove
        exit /b 1
    )
    goto :eof

:main
    call :check_duplicate_install
    call :parse_args %*
    call :normalize_arch
    call :check_architecture
    call :setup_environment
    call :download_files
    call :initialize_epkg
    call :print_completion

endlocal
