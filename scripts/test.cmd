@echo off
REM ASCII-only wrapper. Batch files are read with the OEM code page, so any
REM non-ASCII text here would break the command parser. All logic lives in
REM test.ps1, which is UTF-8 safe.
setlocal
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0test.ps1"
exit /b %ERRORLEVEL%
