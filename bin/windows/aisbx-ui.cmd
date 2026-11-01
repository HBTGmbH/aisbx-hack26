@echo off
rem aisbx-ui - Windows shim (see aisbx.cmd): runs aisbx-ui.ps1 next to it with every argument.
setlocal
set "AISBX_PS=powershell.exe"
where /q pwsh.exe && set "AISBX_PS=pwsh.exe"
"%AISBX_PS%" -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%~dp0aisbx-ui.ps1" %*
exit /b %ERRORLEVEL%
