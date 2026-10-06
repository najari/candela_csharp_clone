@echo off
REM ASCII-only wrapper. Batch files are read with the OEM code page, so any
REM non-ASCII text here would break the command parser. All logic lives in
REM run-gui.ps1, which is UTF-8 safe.
REM
REM Usage:
REM   scripts\run-gui.cmd                 launch the WPF shell
REM   scripts\run-gui.cmd -SmokeTest      run the in-app smoke harness and exit
setlocal
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0run-gui.ps1" %*
exit /b %ERRORLEVEL%
