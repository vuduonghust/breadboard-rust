@echo off
rem BSE-AutoRouter AIO launcher - Windows
setlocal EnableExtensions DisableDelayedExpansion
chcp 65001 >nul 2>&1
cd /d "%~dp0"

rem Pause by default for Explorer double-click; skip pause in common integrated terminals.
set "BSE_FROM_EXPLORER=1"
if defined WT_SESSION set "BSE_FROM_EXPLORER=0"
if defined TERM set "BSE_FROM_EXPLORER=0"
if defined TERM_PROGRAM set "BSE_FROM_EXPLORER=0"
if defined ConEmuPID set "BSE_FROM_EXPLORER=0"
if defined BSE_NO_PAUSE set "BSE_FROM_EXPLORER=0"

rem Prefer the Python launcher, then fall back to python.exe.
py -3 --version >nul 2>&1
if not errorlevel 1 goto use_py
python --version >nul 2>&1
if not errorlevel 1 goto use_python

echo.
echo [ERROR] Khong tim thay Python 3.
echo Cai Python 3 tu https://www.python.org/downloads/ va bat tuy chon Add Python to PATH.
set "BSE_EXIT=9009"
goto finish

:use_py
py -3 "%~dp0bse.py" %*
set "BSE_EXIT=%ERRORLEVEL%"
goto finish

:use_python
python "%~dp0bse.py" %*
set "BSE_EXIT=%ERRORLEVEL%"

:finish
if "%BSE_FROM_EXPLORER%"=="1" (
    echo.
    if not "%BSE_EXIT%"=="0" echo [BSE] Chuong trinh ket thuc voi ma loi %BSE_EXIT%.
    pause
)
endlocal & exit /b %BSE_EXIT%
