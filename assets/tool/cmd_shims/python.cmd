@echo off
setlocal
rem Invokes the extensionless script: same basename as this file without .cmd.
set "EPKG_WRAPPER_SCRIPT=%~dp0%~n0"
where py >nul 2>&1 && (
  py -3 "%EPKG_WRAPPER_SCRIPT%" %*
  exit /b %ERRORLEVEL%
)
where python3 >nul 2>&1 && (
  python3 "%EPKG_WRAPPER_SCRIPT%" %*
  exit /b %ERRORLEVEL%
)
where python >nul 2>&1 && (
  python "%EPKG_WRAPPER_SCRIPT%" %*
  exit /b %ERRORLEVEL%
)
echo epkg tool wrapper: no Python launcher found in PATH (py / python3 / python^) >&2
exit /b 9009
