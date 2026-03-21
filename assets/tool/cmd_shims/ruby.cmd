@echo off
setlocal
set "EPKG_WRAPPER_SCRIPT=%~dp0%~n0"
where ruby >nul 2>&1 && (
  ruby "%EPKG_WRAPPER_SCRIPT%" %*
  exit /b %ERRORLEVEL%
)
echo epkg tool wrapper: ruby not found in PATH >&2
exit /b 9009
