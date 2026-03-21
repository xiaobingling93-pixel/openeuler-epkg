@echo off
setlocal
set "EPKG_WRAPPER_SCRIPT=%~dp0%~n0"
where bash >nul 2>&1 && (
  bash "%EPKG_WRAPPER_SCRIPT%" %*
  exit /b %ERRORLEVEL%
)
where sh >nul 2>&1 && (
  sh "%EPKG_WRAPPER_SCRIPT%" %*
  exit /b %ERRORLEVEL%
)
echo epkg tool wrapper: bash/sh not found in PATH (install Git Bash or use MSYS2 shell^) >&2
exit /b 9009
