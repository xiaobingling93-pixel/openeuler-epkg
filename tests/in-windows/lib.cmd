@echo off
REM Common functions for Windows native tests

REM Log a message with timestamp
:log
    echo [%DATE% %TIME%] %~1
    goto :eof

REM Log error and exit
:error
    echo [ERROR] %~1 >&2
    exit /b 1

REM Run epkg command
:epkg
    echo epkg %*
    "!EPKG_BINARY!" %*
    set "EXIT_CODE=!ERRORLEVEL!"
    if !EXIT_CODE! neq 0 (
        echo ERROR: epkg command failed with exit code !EXIT_CODE! >&2
    )
    exit /b !EXIT_CODE!

REM Get env name from path (mimic env_name_from_path logic)
:env_name_from_path
    set "input_path=%~1"
    REM Remove trailing backslashes
    set "trimmed=!input_path:\\=\!"
    if "!trimmed:~-1!"=="\" set "trimmed=!trimmed:~0,-1!"

    REM Replace backslashes with __
    set "with_underscores=!trimmed:\=__!"

    REM Replace : with _ (for Windows drive letters)
    set "with_underscores=!with_underscores::=_!"

    REM Ensure name starts with __
    if not "!with_underscores:~0,2!"=="__" (
        set "with_underscores=__!with_underscores!"
    )

    set "ENV_NAME_RESULT=!with_underscores!"
    goto :eof
