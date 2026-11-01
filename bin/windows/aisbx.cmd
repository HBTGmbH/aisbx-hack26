@echo off
rem aisbx - Windows shim: put this bin\windows\ directory on your PATH and type `aisbx` in
rem cmd.exe or PowerShell. It runs aisbx.ps1 next to it with all arguments unchanged.
setlocal
set "AISBX_PS=powershell.exe"
where /q pwsh.exe && set "AISBX_PS=pwsh.exe"
"%AISBX_PS%" -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%~dp0aisbx.ps1" %*
exit /b %ERRORLEVEL%
