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

set "LLVM_BIN="
if defined LIBCLANG_PATH set "LLVM_BIN=%LIBCLANG_PATH%"
if not defined LLVM_BIN if exist "%ProgramFiles%\LLVM\bin\libclang.dll" set "LLVM_BIN=%ProgramFiles%\LLVM\bin"
if not defined LLVM_BIN if exist "%ProgramW6432%\LLVM\bin\libclang.dll" set "LLVM_BIN=%ProgramW6432%\LLVM\bin"
if defined LLVM_BIN (
  set "LIBCLANG_PATH=%LLVM_BIN%"
  set "PATH=%LLVM_BIN%;%PATH%"
)

set "CMAKE_BIN="
if exist "%ProgramFiles%\CMake\bin\cmake.exe" set "CMAKE_BIN=%ProgramFiles%\CMake\bin"
if not defined CMAKE_BIN if exist "%ProgramFiles(x86)%\CMake\bin\cmake.exe" set "CMAKE_BIN=%ProgramFiles(x86)%\CMake\bin"
if defined CMAKE_BIN set "PATH=%CMAKE_BIN%;%PATH%"

where cmake >nul 2>&1
if errorlevel 1 (
  >&2 echo CMake was not found in PATH. Install Kitware.CMake or add CMake\bin to PATH.
  exit /b 1
)

where libclang.dll >nul 2>&1
if errorlevel 1 if not defined LIBCLANG_PATH (
  >&2 echo libclang.dll was not found. Install LLVM.LLVM or set LIBCLANG_PATH to LLVM\bin.
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
if not defined VOXGOLEM_CUDA_RUNTIME_DIR (
>&2 echo VOXGOLEM_CUDA_RUNTIME_DIR must be set before running the Windows build stage.
exit /b 1
)

echo [windows-verify] bun install
cd /d "%REPO_ROOT%"
call bun install
if errorlevel 1 exit /b 1

echo [windows-verify] bun run --cwd frontend/app build
cd /d "%REPO_ROOT%"
call bun run --cwd frontend/app build
if errorlevel 1 exit /b 1

set "TAURI_VERIFY_CONFIG=%TEMP%\voxgolem-tauri-verify-config-%RANDOM%%RANDOM%.json"

set "CARGO_TAURI_EXE=%USERPROFILE%\.cargo\bin\cargo-tauri.exe"
if not exist "%CARGO_TAURI_EXE%" (
where cargo-tauri.exe >nul 2>nul
if errorlevel 1 (
>&2 echo Tauri CLI was not found. Install it with: cargo install tauri-cli --version "^2"
exit /b 1
)
set "CARGO_TAURI_EXE=cargo-tauri.exe"
)
set "TAURI_DIR=%REPO_ROOT%\apps\windows-tauri\src-tauri"
set "TAURI_ICON_ICO=%TAURI_DIR%\icons\icon.ico"
if not exist "%TAURI_ICON_ICO%" (
>&2 echo Tauri icon was not found: %TAURI_ICON_ICO%
exit /b 1
)
set "TAURI_ICON_JSON=%TAURI_ICON_ICO:\=/%"
> "%TAURI_VERIFY_CONFIG%" echo {"build":{"beforeBuildCommand":"cmd /c exit 0"},"bundle":{"icon":["%TAURI_ICON_JSON%"]}}

set "VOXGOLEM_TARGET_EXE=%REPO_ROOT%\target\release\vox-golem.exe"
powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "$ErrorActionPreference = 'SilentlyContinue'; $target = [System.IO.Path]::GetFullPath($env:VOXGOLEM_TARGET_EXE); Get-Process -Name 'vox-golem' -ErrorAction SilentlyContinue | Where-Object { try { $_.Path -and ([System.IO.Path]::GetFullPath($_.Path) -eq $target) } catch { $false } } | ForEach-Object { Write-Host ('[windows-verify] Stopping locked release app process {0}' -f $_.Id); Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue }; exit 0"
if errorlevel 1 exit /b 1
cd /d "%TAURI_DIR%"
echo [windows-verify] cargo tauri build --no-bundle
"%CARGO_TAURI_EXE%" build --config "%TAURI_VERIFY_CONFIG%" --no-bundle
set "TAURI_BUILD_STATUS=%errorlevel%"
del "%TAURI_VERIFY_CONFIG%" >nul 2>nul
if not "%TAURI_BUILD_STATUS%"=="0" exit /b %TAURI_BUILD_STATUS%
powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "%SCRIPT_DIR%windows-package-runtime.ps1" -Mode StageRelease -RepoRoot "%REPO_ROOT%"
exit /b %errorlevel%
