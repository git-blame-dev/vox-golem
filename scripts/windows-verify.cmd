@echo off
setlocal

set "VERIFY_STAGE=%~1"
if "%VERIFY_STAGE%"=="" set "VERIFY_STAGE=all"

if /I "%VERIFY_STAGE%"=="all" goto stage_valid
if /I "%VERIFY_STAGE%"=="fmt" goto stage_valid
if /I "%VERIFY_STAGE%"=="clippy" goto stage_valid
if /I "%VERIFY_STAGE%"=="test" goto stage_valid
if /I "%VERIFY_STAGE%"=="build" goto stage_valid
>&2 echo Usage: windows-verify.cmd [all^|fmt^|clippy^|test^|build]
exit /b 1

:stage_valid
set "RUN_FMT=0"
set "RUN_CLIPPY=0"
set "RUN_TEST=0"
set "RUN_BUILD=0"

if /I "%VERIFY_STAGE%"=="all" set "RUN_FMT=1"
if /I "%VERIFY_STAGE%"=="all" set "RUN_CLIPPY=1"
if /I "%VERIFY_STAGE%"=="all" set "RUN_TEST=1"
if /I "%VERIFY_STAGE%"=="all" set "RUN_BUILD=1"
if /I "%VERIFY_STAGE%"=="fmt" set "RUN_FMT=1"
if /I "%VERIFY_STAGE%"=="clippy" set "RUN_CLIPPY=1"
if /I "%VERIFY_STAGE%"=="test" set "RUN_TEST=1"
if /I "%VERIFY_STAGE%"=="build" set "RUN_BUILD=1"

set "SCRIPT_DIR=%~dp0"
for %%I in ("%SCRIPT_DIR%..") do set "REPO_ROOT=%%~fI"

set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
set "VCVARSBAT="
set "VSINSTALL="

if exist "%VSWHERE%" (
for /f "usebackq delims=" %%I in (`"%VSWHERE%" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do set "VSINSTALL=%%I"
)

if defined VSINSTALL (
set "VCVARSBAT=%VSINSTALL%\VC\Auxiliary\Build\vcvars64.bat"
)

if not defined VCVARSBAT (
set "VCVARSBAT=%ProgramFiles(x86)%\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
)

if not exist "%VCVARSBAT%" (
>&2 echo Windows Build Tools were not found. Install Visual Studio Build Tools with the C++ workload.
exit /b 1
)

set "CARGO_EXE=%USERPROFILE%\.cargo\bin\cargo.exe"
if not exist "%CARGO_EXE%" (
set "CARGO_EXE=cargo.exe"
)

call "%VCVARSBAT%" -arch=x64 -host_arch=x64 >nul
if errorlevel 1 (
>&2 echo Failed to initialize the MSVC developer shell.
exit /b 1
)

cd /d "%REPO_ROOT%"

if "%RUN_FMT%"=="1" call :run_fmt
if errorlevel 1 exit /b 1
if "%RUN_CLIPPY%"=="1" call :run_clippy
if errorlevel 1 exit /b 1
if "%RUN_TEST%"=="1" call :run_test
if errorlevel 1 exit /b 1
if "%RUN_BUILD%"=="1" call :run_build
if errorlevel 1 exit /b 1

if /I "%VERIFY_STAGE%"=="all" echo [windows-verify] All native Windows cargo checks passed.
if /I not "%VERIFY_STAGE%"=="all" echo [windows-verify] Stage '%VERIFY_STAGE%' passed.
exit /b 0

:run_fmt
echo [windows-verify] cargo fmt --check
"%CARGO_EXE%" fmt --check
exit /b %errorlevel%

:run_clippy
echo [windows-verify] cargo clippy --all-targets --all-features -- -D warnings
"%CARGO_EXE%" clippy --all-targets --all-features -- -D warnings
exit /b %errorlevel%

:run_test
echo [windows-verify] cargo test
"%CARGO_EXE%" test
exit /b %errorlevel%

:run_build
echo [windows-verify] cargo build
"%CARGO_EXE%" build
exit /b %errorlevel%
