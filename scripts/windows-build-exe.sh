#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)

usage() {
  cat <<'EOF'
Windows build helper for VoxGolem.

Usage:
  windows-build-exe.sh         Build the native VoxGolem.exe.
EOF
}

case "${1-}" in
  "" ) ;;
  -h|--help|/? )
    usage
    exit 0
    ;;
  * )
    printf 'Unknown argument: %s\n\n' "$1" >&2
    usage >&2
    exit 1
    ;;
esac

if [ -d /mnt/c/WINDOWS/system32 ]; then
  PATH="$PATH:/mnt/c/WINDOWS/system32"
fi

if [ -d /mnt/c/Windows/System32 ]; then
  PATH="$PATH:/mnt/c/Windows/System32"
fi

export PATH

if ! command -v cmd.exe >/dev/null 2>&1; then
  printf 'Unable to find cmd.exe in the current WSL environment.\n' >&2
  exit 1
fi

if ! command -v wslpath >/dev/null 2>&1; then
  printf 'Unable to find wslpath in the current WSL environment.\n' >&2
  exit 1
fi

cd "$repo_root"

printf '[windows-build-exe] bun install\n'
bun install

printf '[windows-build-exe] bun run build\n'
bun run build

mkdir -p "$repo_root/target"

tauri_build_config="$repo_root/target/voxgolem-tauri-build-config.json"
temp_cmd="$repo_root/target/windows-build-exe-temp.cmd"

cleanup() {
  rm -f "$temp_cmd" "$tauri_build_config"
}

trap cleanup EXIT HUP INT TERM

printf '%s\n' '{"build":{"beforeBuildCommand":"cmd /c exit 0"}}' > "$tauri_build_config"

repo_root_win=$(wslpath -w "$repo_root")
tauri_dir_win=$(wslpath -w "$repo_root/apps/windows-tauri/src-tauri")
tauri_build_config_win=$(wslpath -w "$tauri_build_config")
temp_cmd_win=$(wslpath -w "$temp_cmd")

cat > "$temp_cmd" <<EOF
@echo off
setlocal

set "REPO_ROOT=$repo_root_win"
set "TAURI_DIR=$tauri_dir_win"
set "OUTPUT_EXE=$repo_root_win\target\release\vox-golem.exe"
set "TAURI_BUILD_CONFIG=$tauri_build_config_win"

set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
set "VCVARSBAT="
set "VSINSTALL="

if exist "%VSWHERE%" (
  for /f "usebackq delims=" %%I in (
    \`"%VSWHERE%" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath\`
  ) do set "VSINSTALL=%%I"
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

set "CARGO_TAURI_EXE=%USERPROFILE%\.cargo\bin\cargo-tauri.exe"
if not exist "%CARGO_TAURI_EXE%" (
  where cargo-tauri.exe >nul 2>nul
  if errorlevel 1 (
    >&2 echo Tauri CLI was not found. Install it with: cargo install tauri-cli --version "^2"
    exit /b 1
  )
  set "CARGO_TAURI_EXE=cargo-tauri.exe"
)

call "%VCVARSBAT%" -arch=x64 -host_arch=x64 >nul
if errorlevel 1 (
  >&2 echo Failed to initialize the MSVC developer shell.
  exit /b 1
)

cd /d "%TAURI_DIR%"

echo [windows-build-exe] stopping running VoxGolem executables
powershell.exe -NoProfile -NonInteractive -Command ^
  "\$stopped = \$false; \$names = @('vox-golem', 'vox-golem-windows'); foreach (\$name in \$names) { \$procs = Get-Process -Name \$name -ErrorAction SilentlyContinue; if (\$null -ne \$procs) { \$procs | Stop-Process -Force -ErrorAction SilentlyContinue; \$stopped = \$true } }; if (\$stopped) { Start-Sleep -Seconds 1 }" >nul

echo [windows-build-exe] cargo tauri build --no-bundle
"%CARGO_TAURI_EXE%" build --config "%TAURI_BUILD_CONFIG%" --no-bundle
if errorlevel 1 exit /b 1

echo [windows-build-exe] Built native app executable:
echo %OUTPUT_EXE%
EOF

set +e
cmd.exe /c "$temp_cmd_win"
status=$?
set -e

if [ "$status" -ne 0 ]; then
  printf 'Windows build failed. If you saw "Access is denied", close the running VoxGolem exe and rerun this script.\n' >&2
fi

exit "$status"
