@echo off
setlocal

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

echo [windows-verify] cargo fmt --check
"%CARGO_EXE%" fmt --check
if errorlevel 1 exit /b 1

echo [windows-verify] cargo clippy --all-targets --all-features -- -D warnings
"%CARGO_EXE%" clippy --all-targets --all-features -- -D warnings
if errorlevel 1 exit /b 1

echo [windows-verify] cargo test
"%CARGO_EXE%" test
if errorlevel 1 exit /b 1

echo [windows-verify] cargo build
"%CARGO_EXE%" build
if errorlevel 1 exit /b 1

echo [windows-verify] All native Windows cargo checks passed.
