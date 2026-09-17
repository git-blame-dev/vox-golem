#!/usr/bin/env bash
set -eu -o pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
temp_dir="$(mktemp -d)"
trap 'rm -rf "$temp_dir"' EXIT

mkdir -p "$temp_dir/compiler"
# The generated stub retains these variables for its own runtime.
# shellcheck disable=SC2016
printf '%s\n' \
  '#!/usr/bin/env bash' \
  'set -eu' \
  'printf "%s\n" "$@" > "$VOXGOLEM_COMPILER_ARGS_LOG"' > "$temp_dir/compiler/clang-cl"
chmod 0755 "$temp_dir/compiler/clang-cl"
printf '%s\n' '#include <io.h>' '#include <fcntl.h>' > "$temp_dir/espeak-compat.h"
VOXGOLEM_REAL_CLANG_CL="$temp_dir/compiler/clang-cl" \
  VOXGOLEM_ESPEAK_COMPAT_HEADER="$temp_dir/espeak-compat.h" \
  VOXGOLEM_COMPILER_ARGS_LOG="$temp_dir/compiler-args" \
  "$root/scripts/windows-cross/clang-cl" -nologo -c -- "$temp_dir/unrelated.c"
if grep -Fq '/FI' "$temp_dir/compiler-args"; then
  printf '%s\n' 'Windows compiler wrapper modified an unrelated source.' >&2
  exit 1
fi
VOXGOLEM_REAL_CLANG_CL="$temp_dir/compiler/clang-cl" \
  VOXGOLEM_ESPEAK_COMPAT_HEADER="$temp_dir/espeak-compat.h" \
  VOXGOLEM_COMPILER_ARGS_LOG="$temp_dir/compiler-args" \
  "$root/scripts/windows-cross/clang-cl" -nologo -c -- "$temp_dir/espeak-ng/src/espeak-ng.c"
mapfile -t compiler_args < "$temp_dir/compiler-args"
test "${compiler_args[0]}" = '--driver-mode=cl'
test "${compiler_args[1]}" = "/FI$temp_dir/espeak-compat.h"

# The generated stub retains these variables for its own runtime.
# shellcheck disable=SC2016
printf '%s\n' \
  '#!/usr/bin/env bash' \
  'set -eu' \
  'test "$1" = "-VERSION"' \
  'printf "%s\n" "${FAKE_NSIS_VERSION:-v3.09-4}"' > "$temp_dir/makensis"
chmod 0755 "$temp_dir/makensis"
VOXGOLEM_MAKENSIS="$temp_dir/makensis" bash "$root/scripts/verify-nsis-version.sh"
if FAKE_NSIS_VERSION=v3.10 VOXGOLEM_MAKENSIS="$temp_dir/makensis" \
  bash "$root/scripts/verify-nsis-version.sh"; then
  printf '%s\n' 'NSIS version verification unexpectedly accepted an unpinned generator.' >&2
  exit 1
fi

nsis_source="$temp_dir/installer-source.nsi"
nsis_custom="$temp_dir/installer-custom.nsi"
# These are literal NSIS variables in the generated template fixture.
# shellcheck disable=SC2016
printf '%s\n' \
  '; 5. Choose install directory page' \
  '!define MUI_PAGE_CUSTOMFUNCTION_PRE SkipIfPassive' \
  '!insertmacro MUI_PAGE_DIRECTORY' \
  '; Use show readme button in the finish page as a button create a desktop shortcut' \
  '!define MUI_FINISHPAGE_SHOWREADME' \
  '!define MUI_FINISHPAGE_SHOWREADME_TEXT "$(createDesktop)"' \
  '!define MUI_FINISHPAGE_SHOWREADME_FUNCTION CreateOrUpdateDesktopShortcut' \
  '  ; Create desktop shortcut for silent and passive installers' \
  '  ; because finish page will be skipped' \
  '  ${If} $PassiveMode = 1' \
  '  ${OrIf} ${Silent}' \
  '    Call CreateOrUpdateDesktopShortcut' \
  '  ${EndIf}' \
  '  ${If} $INSTDIR == "${PLACEHOLDER_INSTALL_DIR}"' \
  '    ; Set default install location' \
  '    !if "${INSTALLMODE}" == "perMachine"' \
  '      ${If} ${RunningX64}' \
  '        !if "${ARCH}" == "x64"' \
  '          StrCpy $INSTDIR "$PROGRAMFILES64\${PRODUCTNAME}"' \
  '        !else if "${ARCH}" == "arm64"' \
  '          StrCpy $INSTDIR "$PROGRAMFILES64\${PRODUCTNAME}"' \
  '        !else' \
  '          StrCpy $INSTDIR "$PROGRAMFILES\${PRODUCTNAME}"' \
  '        !endif' \
  '      ${Else}' \
  '        StrCpy $INSTDIR "$PROGRAMFILES\${PRODUCTNAME}"' \
  '      ${EndIf}' \
  '    !else if "${INSTALLMODE}" == "currentUser"' \
  '      StrCpy $INSTDIR "$LOCALAPPDATA\${PRODUCTNAME}"' \
  '    !endif' \
  '' \
  '    Call RestorePreviousInstallLocation' \
  '  ${EndIf}' > "$nsis_source"
python3 "$root/scripts/customize-windows-nsis-template.py" "$nsis_source" "$nsis_custom"
# shellcheck disable=SC2016
grep -Fq 'StrCpy $INSTDIR "$LOCALAPPDATA\Programs\VoxGolem"' "$nsis_custom"
if grep -Fq 'MUI_PAGE_DIRECTORY' "$nsis_custom" || \
   grep -Fq 'RestorePreviousInstallLocation' "$nsis_custom" || \
   grep -Fq 'CreateOrUpdateDesktopShortcut' "$nsis_custom"; then
  printf '%s\n' 'Customized NSIS template retained a forbidden installation identity path.' >&2
  exit 1
fi

mkdir -p "$temp_dir/windows-package"
# The generated stub retains these variables for its own runtime.
# shellcheck disable=SC2016
printf '%s\n' \
  '#!/usr/bin/env bash' \
  'set -eu' \
  'printf "%s\n" "    DLL Name: KERNEL32.dll"' \
  'case "$2" in' \
  '  *vox-golem.exe) printf "%s\n" "    DLL Name: DirectML.dll" "    DLL Name: bcrypt.dll" ;;' \
  '  *onnxruntime_providers_cuda.dll) printf "%s\n" "    DLL Name: cudnn64_9.dll" ;;' \
  'esac' \
  'if [ "${FAKE_UNPLANNED_IMPORT:-}" = 1 ]; then printf "%s\n" "    DLL Name: nvinfer_10.dll"; fi' > \
  "$temp_dir/fake-llvm-objdump"
chmod 0755 "$temp_dir/fake-llvm-objdump"
# The generated stub retains these variables for its own runtime.
# shellcheck disable=SC2016
printf '%s\n' \
  '#!/usr/bin/env bash' \
  'set -eu' \
  'case "$1" in' \
  '  *"${FAKE_NON_PE_NAME:-__never__}") printf "%s: ASCII text\\n" "$1" ;;' \
  '  *"${FAKE_WRONG_ARCH_NAME:-__never__}") printf "%s: PE32 executable Intel 80386\\n" "$1" ;;' \
  '  *\$PLUGINSDIR/*.dll) printf "%s: PE32 executable (DLL) Intel 80386\\n" "$1" ;;' \
  '  *\$PLUGINSDIR/*.bmp) printf "%s: PC bitmap\\n" "$1" ;;' \
  '  *uninstall.exe) printf "%s: PE32 executable Intel 80386\\n" "$1" ;;' \
  '  *vox-golem.exe) printf "%s: PE32+ executable x86-64\\n" "$1" ;;' \
  '  *.exe) printf "%s: PE32 executable Intel 80386, Nullsoft Installer self-extracting archive\\n" "$1" ;;' \
  '  *) printf "%s: PE32+ executable x86-64\\n" "$1" ;;' \
  'esac' > "$temp_dir/fake-file"
chmod 0755 "$temp_dir/fake-file"
for file in \
  vox-golem.exe \
  DirectML.dll \
  onnxruntime_providers_cuda.dll \
  onnxruntime_providers_shared.dll \
  MSVCP140.dll \
  MSVCP140_1.dll \
  VCRUNTIME140.dll \
  VCRUNTIME140_1.dll; do
  printf '%s\n' "$file" > "$temp_dir/windows-package/$file"
done
VOXGOLEM_FILE="$temp_dir/fake-file" VOXGOLEM_OBJDUMP="$temp_dir/fake-llvm-objdump" \
  "$root/scripts/verify-windows-package.sh" "$temp_dir/windows-package"
if VOXGOLEM_FILE="$temp_dir/fake-file" VOXGOLEM_OBJDUMP="$temp_dir/fake-llvm-objdump" FAKE_UNPLANNED_IMPORT=1 \
  "$root/scripts/verify-windows-package.sh" "$temp_dir/windows-package"; then
  printf '%s\n' 'Windows package unexpectedly accepted an unplanned PE dependency.' >&2
  exit 1
fi
printf '%s\n' 'forbidden runtime' > "$temp_dir/windows-package/cudnn64_9.dll"
if VOXGOLEM_FILE="$temp_dir/fake-file" VOXGOLEM_OBJDUMP="$temp_dir/fake-llvm-objdump" \
  "$root/scripts/verify-windows-package.sh" "$temp_dir/windows-package"; then
  printf '%s\n' 'Windows package unexpectedly accepted a CUDA runtime.' >&2
  exit 1
fi
rm "$temp_dir/windows-package/cudnn64_9.dll"
rm "$temp_dir/windows-package/MSVCP140.dll"
if VOXGOLEM_FILE="$temp_dir/fake-file" VOXGOLEM_OBJDUMP="$temp_dir/fake-llvm-objdump" \
  "$root/scripts/verify-windows-package.sh" "$temp_dir/windows-package"; then
  printf '%s\n' 'Windows package unexpectedly accepted a missing VC runtime.' >&2
  exit 1
fi

printf '%s\n' 'MSVCP140.dll' > "$temp_dir/windows-package/MSVCP140.dll"
if VOXGOLEM_FILE="$temp_dir/fake-file" FAKE_NON_PE_NAME='MSVCP140.dll' \
  VOXGOLEM_OBJDUMP="$temp_dir/fake-llvm-objdump" \
  "$root/scripts/verify-windows-package.sh" "$temp_dir/windows-package"; then
  printf '%s\n' 'Windows package unexpectedly accepted a non-PE payload file.' >&2
  exit 1
fi
if VOXGOLEM_FILE="$temp_dir/fake-file" FAKE_WRONG_ARCH_NAME='MSVCP140.dll' \
  VOXGOLEM_OBJDUMP="$temp_dir/fake-llvm-objdump" \
  "$root/scripts/verify-windows-package.sh" "$temp_dir/windows-package"; then
  printf '%s\n' 'Windows package unexpectedly accepted a wrong-architecture payload file.' >&2
  exit 1
fi

truncate -s 25000001 "$temp_dir/windows-package/DirectML.dll"
if VOXGOLEM_FILE="$temp_dir/fake-file" VOXGOLEM_OBJDUMP="$temp_dir/fake-llvm-objdump" \
  "$root/scripts/verify-windows-package.sh" "$temp_dir/windows-package"; then
  printf '%s\n' 'Windows package unexpectedly accepted an oversized payload file.' >&2
  exit 1
fi
printf '%s\n' 'DirectML.dll' > "$temp_dir/windows-package/DirectML.dll"

installer_input="$temp_dir/installer-input"
plugin_dir="$installer_input/\$PLUGINSDIR"
mkdir -p "$plugin_dir"
cp "$temp_dir/windows-package/"* "$installer_input/"
for internal in System.dll nsDialogs.dll nsis_tauri_utils.dll StartMenu.dll NSISdl.dll; do
  printf '%s\n' "$internal" > "$plugin_dir/$internal"
done
printf '%s\n' 'bitmap' > "$plugin_dir/modern-wizard.bmp"
printf '%s\n' 'uninstaller' > "$installer_input/uninstall.exe"
create_test_installer() {
  local output="$1"
  rm -f "$output"
  (cd "$installer_input" && 7z a -t7z "$output" \
    "./\$PLUGINSDIR/System.dll" \
    "./\$PLUGINSDIR/modern-wizard.bmp" \
    "./\$PLUGINSDIR/nsDialogs.dll" \
    "./\$PLUGINSDIR/nsis_tauri_utils.dll" \
    "./\$PLUGINSDIR/StartMenu.dll" \
    "./\$PLUGINSDIR/NSISdl.dll" \
    ./*.dll ./*.exe >/dev/null)
  for internal in System.dll modern-wizard.bmp nsDialogs.dll nsis_tauri_utils.dll StartMenu.dll NSISdl.dll; do
    7z rn "$output" "$internal" "\$PLUGINSDIR/$internal" >/dev/null
  done
}
installer="$temp_dir/fake-installer.exe"
create_test_installer "$installer"
VOXGOLEM_FILE="$temp_dir/fake-file" VOXGOLEM_OBJDUMP="$temp_dir/fake-llvm-objdump" \
  "$root/scripts/verify-windows-installer.sh" "$installer" 250000000
if VOXGOLEM_FILE="$temp_dir/fake-file" FAKE_NON_PE_NAME='MSVCP140.dll' \
  VOXGOLEM_OBJDUMP="$temp_dir/fake-llvm-objdump" \
  "$root/scripts/verify-windows-installer.sh" "$installer" 250000000; then
  printf '%s\n' 'Windows installer unexpectedly accepted a non-PE payload file.' >&2
  exit 1
fi
if VOXGOLEM_FILE="$temp_dir/fake-file" FAKE_WRONG_ARCH_NAME='MSVCP140.dll' \
  VOXGOLEM_OBJDUMP="$temp_dir/fake-llvm-objdump" \
  "$root/scripts/verify-windows-installer.sh" "$installer" 250000000; then
  printf '%s\n' 'Windows installer unexpectedly accepted a wrong-architecture payload file.' >&2
  exit 1
fi
if VOXGOLEM_FILE="$temp_dir/fake-file" VOXGOLEM_OBJDUMP="$temp_dir/fake-llvm-objdump" \
  FAKE_UNPLANNED_IMPORT=1 \
  "$root/scripts/verify-windows-installer.sh" "$installer" 250000000; then
  printf '%s\n' 'Windows installer unexpectedly accepted an unplanned PE dependency.' >&2
  exit 1
fi
if VOXGOLEM_FILE="$temp_dir/fake-file" FAKE_NON_PE_NAME='uninstall.exe' \
  VOXGOLEM_OBJDUMP="$temp_dir/fake-llvm-objdump" \
  "$root/scripts/verify-windows-installer.sh" "$installer" 250000000; then
  printf '%s\n' 'Windows installer unexpectedly accepted a non-PE uninstaller.' >&2
  exit 1
fi

for forbidden in cudnn64_9.dll model.onnx unexpected-plugin.dll; do
  forbidden_installer="$temp_dir/forbidden-$forbidden.exe"
  cp "$installer" "$forbidden_installer"
  printf '%s\n' forbidden > "$plugin_dir/$forbidden"
  (cd "$installer_input" && 7z a "$forbidden_installer" "./\$PLUGINSDIR/$forbidden" >/dev/null)
  7z rn "$forbidden_installer" "$forbidden" "\$PLUGINSDIR/$forbidden" >/dev/null
  rm "$plugin_dir/$forbidden"
  if VOXGOLEM_FILE="$temp_dir/fake-file" VOXGOLEM_OBJDUMP="$temp_dir/fake-llvm-objdump" \
    "$root/scripts/verify-windows-installer.sh" "$forbidden_installer" 250000000; then
    printf 'Windows installer unexpectedly accepted forbidden internal %s.\n' "$forbidden" >&2
    exit 1
  fi
done

traversal_installer="$temp_dir/traversal-installer.exe"
cp "$installer" "$traversal_installer"
7z rn "$traversal_installer" "\$PLUGINSDIR/System.dll" '../System.dll' >/dev/null
if VOXGOLEM_FILE="$temp_dir/fake-file" VOXGOLEM_OBJDUMP="$temp_dir/fake-llvm-objdump" \
  "$root/scripts/verify-windows-installer.sh" "$traversal_installer" 250000000; then
  printf '%s\n' 'Windows installer unexpectedly accepted a traversal-shaped internal path.' >&2
  exit 1
fi

oversized_installer="$temp_dir/oversized-installer.exe"
truncate -s 25000001 "$installer_input/DirectML.dll"
create_test_installer "$oversized_installer"
if VOXGOLEM_FILE="$temp_dir/fake-file" VOXGOLEM_OBJDUMP="$temp_dir/fake-llvm-objdump" \
  "$root/scripts/verify-windows-installer.sh" "$oversized_installer" 250000000; then
  printf '%s\n' 'Windows installer unexpectedly accepted an oversized extracted payload.' >&2
  exit 1
fi

printf '%s\n' 'DirectML.dll' > "$installer_input/DirectML.dll"
oversized_uninstaller="$temp_dir/oversized-uninstaller.exe"
truncate -s 500001 "$installer_input/uninstall.exe"
create_test_installer "$oversized_uninstaller"
if VOXGOLEM_FILE="$temp_dir/fake-file" VOXGOLEM_OBJDUMP="$temp_dir/fake-llvm-objdump" \
  "$root/scripts/verify-windows-installer.sh" "$oversized_uninstaller" 250000000; then
  printf '%s\n' 'Windows installer unexpectedly accepted an oversized uninstaller.' >&2
  exit 1
fi
printf '%s\n' 'uninstaller' > "$installer_input/uninstall.exe"

header_named_installer="$temp_dir/header-name.exe"
cp "$installer" "$header_named_installer"
printf '%s\n' unexpected > "$installer_input/header-entry"
(cd "$installer_input" && 7z a "$header_named_installer" header-entry >/dev/null)
7z rn "$header_named_installer" header-entry header-name.exe >/dev/null
if (
  cd "$temp_dir"
  VOXGOLEM_FILE="$temp_dir/fake-file" VOXGOLEM_OBJDUMP="$temp_dir/fake-llvm-objdump" \
    "$root/scripts/verify-windows-installer.sh" header-name.exe 250000000
); then
  printf '%s\n' 'Windows installer unexpectedly hid an entry matching its argument.' >&2
  exit 1
fi

real_7z="$(command -v 7z)"
mkdir -p "$temp_dir/fake-7z"
# The generated stub retains these variables for its own runtime.
# shellcheck disable=SC2016
printf '%s\n' \
  '#!/usr/bin/env bash' \
  'set -eu' \
  'operation="$1"' \
  '"$REAL_7Z" "$@"' \
  'if [ "$operation" = l ] && [ "${FAKE_7Z_MODE:-}" = duplicate ]; then' \
  '  printf "\\nPath = DirectML.dll\\nSize = 13\\nAttributes = A\\n"' \
  'fi' \
  'if [ "$operation" = x ] && [ "${FAKE_7Z_MODE:-}" = unexpected-extraction ]; then' \
  '  for argument in "$@"; do' \
  '    case "$argument" in -o*) output="${argument#-o}" ;; esac' \
  '  done' \
  '  printf "%s\\n" unexpected > "$output/unexpected.txt"' \
  'fi' > "$temp_dir/fake-7z/7z"
chmod 0755 "$temp_dir/fake-7z/7z"
if PATH="$temp_dir/fake-7z:$PATH" REAL_7Z="$real_7z" FAKE_7Z_MODE=duplicate \
  VOXGOLEM_FILE="$temp_dir/fake-file" VOXGOLEM_OBJDUMP="$temp_dir/fake-llvm-objdump" \
  "$root/scripts/verify-windows-installer.sh" "$installer" 250000000; then
  printf '%s\n' 'Windows installer unexpectedly accepted a duplicate archive entry.' >&2
  exit 1
fi
if PATH="$temp_dir/fake-7z:$PATH" REAL_7Z="$real_7z" FAKE_7Z_MODE=unexpected-extraction \
  VOXGOLEM_FILE="$temp_dir/fake-file" VOXGOLEM_OBJDUMP="$temp_dir/fake-llvm-objdump" \
  "$root/scripts/verify-windows-installer.sh" "$installer" 250000000; then
  printf '%s\n' 'Windows installer unexpectedly accepted an unlisted extracted path.' >&2
  exit 1
fi

jq -e '.bundle.linux.appimage.bundleMediaFramework == true' \
  "$root/apps/desktop-tauri/src-tauri/tauri.conf.json" >/dev/null

version="$("$root/scripts/release-version.sh" 0 42)"
test "$version" = '1970.1.1-42'

"$root/scripts/assert-release-version-newer.sh" '2026.7.27-43' '2026.7.27-42'
"$root/scripts/assert-release-version-newer.sh" '2026.7.28-0' '2026.7.27-9999'
if "$root/scripts/assert-release-version-newer.sh" '2026.7.27-42' '2026.7.27-42'; then
  printf '%s\n' 'Equal application version unexpectedly passed monotonicity validation.' >&2
  exit 1
fi
if "$root/scripts/assert-release-version-newer.sh" '2026.7.26-9999' '2026.7.27-1'; then
  printf '%s\n' 'Backward commit-date version unexpectedly passed monotonicity validation.' >&2
  exit 1
fi

commit_timestamp="$(cd "$root" && git show -s --format=%ct HEAD)"
expected_build_version="$(
  "$root/scripts/release-version.sh" "$commit_timestamp" "${GITHUB_RUN_NUMBER:-0}"
)"
actual_build_version="$(make -s -C "$root" app-version APP_VERSION='arbitrary-version')"
test "$actual_build_version" = "$expected_build_version"

printf '%s' 'synthetic signature' | base64 > "$temp_dir/linux-update.sig"
printf '%s' 'synthetic signature' | base64 > "$temp_dir/windows-update.sig"
GITHUB_REPOSITORY='git-blame-dev/vox-golem' "$root/scripts/generate-update-manifest.sh" \
  '2026.7.27-42' 'v2026.7.27-42' 'vox-golem-linux-x86_64-v2026.7.27-42.AppImage' \
  "$temp_dir/linux-update.sig" 'vox-golem-windows-x86_64-v2026.7.27-42-setup.exe' \
  "$temp_dir/windows-update.sig" "$temp_dir/latest.json"

jq -e '
  .version == "2026.7.27-42" and
  (.platforms | keys) == ["linux-x86_64", "windows-x86_64"] and
  .platforms["linux-x86_64"].signature == "c3ludGhldGljIHNpZ25hdHVyZQ==" and
  .platforms["linux-x86_64"].url == "https://github.com/git-blame-dev/vox-golem/releases/download/v2026.7.27-42/vox-golem-linux-x86_64-v2026.7.27-42.AppImage" and
  .platforms["windows-x86_64"].signature == "c3ludGhldGljIHNpZ25hdHVyZQ==" and
  .platforms["windows-x86_64"].url == "https://github.com/git-blame-dev/vox-golem/releases/download/v2026.7.27-42/vox-golem-windows-x86_64-v2026.7.27-42-setup.exe"
' "$temp_dir/latest.json" >/dev/null

if GITHUB_REPOSITORY='git-blame-dev/vox-golem' "$root/scripts/generate-update-manifest.sh" \
  '2026.7.27-42' 'v2026.7.27-42' 'vox-golem-linux-x86_64-v2026.7.27-42.AppImage' \
  "$temp_dir/linux-update.sig" 'vox-golem-windows-x86_64-v2026.7.27-42-setup.exe' \
  "$temp_dir/missing.sig" "$temp_dir/incomplete.json"; then
  printf '%s\n' 'Updater manifest unexpectedly accepted a missing Windows signature.' >&2
  exit 1
fi

finalizer_root="$temp_dir/finalizer-root"
mkdir -p \
  "$temp_dir/finalizer-bin" \
  "$temp_dir/finalizer-linux/VoxGolem" \
  "$temp_dir/finalizer-input" \
  "$finalizer_root/scripts" \
  "$finalizer_root/apps/desktop-tauri/src-tauri"
cp \
  "$root/scripts/finalize-release-assets.sh" \
  "$root/scripts/generate-update-manifest.sh" \
  "$root/scripts/verify-release-assets.sh" \
  "$finalizer_root/scripts/"
cp "$root/apps/desktop-tauri/src-tauri/tauri.conf.json" \
  "$finalizer_root/apps/desktop-tauri/src-tauri/tauri.conf.json"
printf '%s' 'compiled Linux binary' > "$temp_dir/finalizer-linux/VoxGolem/vox-golem"
printf '%s' 'final AppImage bytes' > "$temp_dir/finalizer-input/VoxGolem.AppImage"
printf '%s' 'final NSIS bytes' > "$temp_dir/finalizer-input/VoxGolem-setup.exe"
# shellcheck disable=SC2016
printf '%s\n' \
  '#!/usr/bin/env bash' \
  'set -eu' \
  'count=0; test ! -f "$FAKE_SIGN_COUNT" || count="$(<"$FAKE_SIGN_COUNT")"' \
  'count=$((count + 1)); printf "%s" "$count" > "$FAKE_SIGN_COUNT"' \
  'for argument in "$@"; do payload="$argument"; done' \
  'printf "%s" "synthetic signature" | base64 > "$payload.sig"' \
  > "$temp_dir/finalizer-bin/cargo-tauri"
# shellcheck disable=SC2016
printf '%s\n' \
  '#!/usr/bin/env bash' \
  'set -eu' \
  'signature_file=""' \
  'while [ "$#" -gt 0 ]; do if [ "$1" = "-x" ]; then signature_file="$2"; shift 2; else shift; fi; done' \
  'test -n "$signature_file" && test "$(<"$signature_file")" = "synthetic signature"' \
  'count=0; test ! -f "$FAKE_VERIFY_COUNT" || count="$(<"$FAKE_VERIFY_COUNT")"' \
  'count=$((count + 1)); printf "%s" "$count" > "$FAKE_VERIFY_COUNT"' \
  > "$temp_dir/finalizer-bin/minisign"
chmod 0755 "$temp_dir/finalizer-bin/cargo-tauri" "$temp_dir/finalizer-bin/minisign"
sentinel="$temp_dir/finalizer-sentinel"
printf '%s' 'must survive' > "$sentinel"
test -f "$sentinel"
ln -s "$temp_dir/missing-finalizer-target" "$finalizer_root/dist-final"
if GITHUB_REPOSITORY='git-blame-dev/vox-golem' TAURI_SIGNING_PRIVATE_KEY='synthetic fixture key' \
  "$finalizer_root/scripts/finalize-release-assets.sh" \
  "$temp_dir/finalizer-input/VoxGolem.AppImage" "$temp_dir/finalizer-linux/VoxGolem" \
  "$temp_dir/finalizer-input/VoxGolem-setup.exe" \
  '2026.7.27-42' 'v2026.7.27-42'; then
  printf '%s\n' 'Dangling finalizer output symlink was unexpectedly accepted.' >&2
  exit 1
fi
test -L "$finalizer_root/dist-final"
rm "$finalizer_root/dist-final"
FAKE_SIGN_COUNT="$temp_dir/sign-count" FAKE_VERIFY_COUNT="$temp_dir/verify-count" \
  TAURI_SIGNER="$temp_dir/finalizer-bin/cargo-tauri" MINISIGN="$temp_dir/finalizer-bin/minisign" \
  TAURI_SIGNING_PRIVATE_KEY='synthetic fixture key' TAURI_SIGNING_PRIVATE_KEY_PASSWORD='' \
  GITHUB_REPOSITORY='git-blame-dev/vox-golem' \
  "$finalizer_root/scripts/finalize-release-assets.sh" \
  "$temp_dir/finalizer-input/VoxGolem.AppImage" "$temp_dir/finalizer-linux/VoxGolem" \
  "$temp_dir/finalizer-input/VoxGolem-setup.exe" \
  '2026.7.27-42' 'v2026.7.27-42'
test "$(<"$temp_dir/sign-count")" -eq 2
test "$(<"$temp_dir/verify-count")" -eq 2
export MINISIGN="$temp_dir/finalizer-bin/minisign"
export FAKE_VERIFY_COUNT="$temp_dir/verify-count"
cmp "$temp_dir/finalizer-input/VoxGolem.AppImage" \
  "$finalizer_root/dist-final/vox-golem-linux-x86_64-v2026.7.27-42.AppImage"
cmp "$temp_dir/finalizer-input/VoxGolem-setup.exe" \
  "$finalizer_root/dist-final/vox-golem-windows-x86_64-v2026.7.27-42-setup.exe"
GITHUB_REPOSITORY='git-blame-dev/vox-golem' "$root/scripts/verify-release-assets.sh" \
  "$finalizer_root/dist-final" 'v2026.7.27-42'

release_dir="$temp_dir/release-assets"
mkdir -p "$release_dir"
for asset in \
  'vox-golem-linux-v2026.7.27-42.zip' \
  'vox-golem-linux-x86_64-v2026.7.27-42.AppImage' \
  'vox-golem-linux-x86_64-v2026.7.27-42.AppImage.sig' \
  'vox-golem-windows-x86_64-v2026.7.27-42-setup.exe' \
  'vox-golem-windows-x86_64-v2026.7.27-42-setup.exe.sig'; do
  printf '%s\n' "$asset" > "$release_dir/$asset"
done
cp "$temp_dir/linux-update.sig" \
  "$release_dir/vox-golem-linux-x86_64-v2026.7.27-42.AppImage.sig"
cp "$temp_dir/windows-update.sig" \
  "$release_dir/vox-golem-windows-x86_64-v2026.7.27-42-setup.exe.sig"
cp "$temp_dir/latest.json" "$release_dir/latest.json"
(
  cd "$release_dir"
  sha256sum \
    'vox-golem-linux-v2026.7.27-42.zip' \
    'vox-golem-linux-x86_64-v2026.7.27-42.AppImage' \
    'vox-golem-linux-x86_64-v2026.7.27-42.AppImage.sig' \
    'vox-golem-windows-x86_64-v2026.7.27-42-setup.exe' \
    'vox-golem-windows-x86_64-v2026.7.27-42-setup.exe.sig' \
    latest.json > SHA256SUMS
)
GITHUB_REPOSITORY='git-blame-dev/vox-golem' "$root/scripts/verify-release-assets.sh" \
  "$release_dir" 'v2026.7.27-42'

cp "$release_dir/vox-golem-linux-x86_64-v2026.7.27-42.AppImage.sig" "$temp_dir/valid-linux.sig"
printf '%s' 'bad signature' | base64 > "$release_dir/vox-golem-linux-x86_64-v2026.7.27-42.AppImage.sig"
jq --arg signature "$(<"$release_dir/vox-golem-linux-x86_64-v2026.7.27-42.AppImage.sig")" \
  '.platforms["linux-x86_64"].signature = $signature' "$release_dir/latest.json" > "$temp_dir/bad-signature-latest.json"
mv "$temp_dir/bad-signature-latest.json" "$release_dir/latest.json"
(
  cd "$release_dir"
  sha256sum \
    'vox-golem-linux-v2026.7.27-42.zip' \
    'vox-golem-linux-x86_64-v2026.7.27-42.AppImage' \
    'vox-golem-linux-x86_64-v2026.7.27-42.AppImage.sig' \
    'vox-golem-windows-x86_64-v2026.7.27-42-setup.exe' \
    'vox-golem-windows-x86_64-v2026.7.27-42-setup.exe.sig' \
    latest.json > SHA256SUMS
)
if GITHUB_REPOSITORY='git-blame-dev/vox-golem' "$root/scripts/verify-release-assets.sh" \
  "$release_dir" 'v2026.7.27-42'; then
  printf '%s\n' 'Release verification unexpectedly accepted a bad updater signature.' >&2
  exit 1
fi
cp "$temp_dir/valid-linux.sig" "$release_dir/vox-golem-linux-x86_64-v2026.7.27-42.AppImage.sig"
cp "$temp_dir/latest.json" "$release_dir/latest.json"
(
  cd "$release_dir"
  sha256sum \
    'vox-golem-linux-v2026.7.27-42.zip' \
    'vox-golem-linux-x86_64-v2026.7.27-42.AppImage' \
    'vox-golem-linux-x86_64-v2026.7.27-42.AppImage.sig' \
    'vox-golem-windows-x86_64-v2026.7.27-42-setup.exe' \
    'vox-golem-windows-x86_64-v2026.7.27-42-setup.exe.sig' \
    latest.json > SHA256SUMS
)

mkdir -p "$temp_dir/publish-bin"
# The generated stub retains these variables for its own runtime.
# shellcheck disable=SC2016
printf '%s\n' \
  '#!/usr/bin/env bash' \
  'set -eu -o pipefail' \
  'state="$FAKE_GH_STATE"' \
  'render_assets() {' \
  '  find "$state/assets" -mindepth 1 -maxdepth 1 -type f -printf "%f\\n" | LC_ALL=C sort | jq -R . | jq -s "map({name: .})"' \
  '}' \
  'render_api_release() {' \
  '  assets="$(render_assets)"' \
  '  jq -n --arg tag "$(<"$state/tag")" --arg target "$(<"$state/target")" --argjson draft "$(<"$state/draft")" --argjson prerelease "$(<"$state/prerelease")" --argjson assets "$assets" "{tag_name: \$tag, target_commitish: \$target, draft: \$draft, prerelease: \$prerelease, assets: \$assets}"' \
  '}' \
  'render_view_release() {' \
  '  assets="$(render_assets)"' \
  '  jq -n --arg target "$(<"$state/target")" --argjson draft "$(<"$state/draft")" --argjson prerelease "$(<"$state/prerelease")" --argjson assets "$assets" "{targetCommitish: \$target, isDraft: \$draft, isPrerelease: \$prerelease, assets: \$assets}"' \
  '}' \
  'command="$1"; shift' \
  'if [ "$command" = api ]; then' \
  '  [ "${1:-}" = --include ] && shift' \
  '  [ "${1:-}" = --jq ] && shift 2' \
  '  if [ "${FAKE_GH_API_FAIL:-}" = 1 ]; then printf "%s\n" "gh: synthetic API failure" >&2; exit 1; fi' \
  '  if [[ "$1" == */commits/main ]]; then if [ -n "${FAKE_MAIN_SEQUENCE_FILE:-}" ]; then n=0; test ! -f "$FAKE_MAIN_SEQUENCE_FILE" || n="$(<"$FAKE_MAIN_SEQUENCE_FILE")"; n=$((n + 1)); printf "%s" "$n" > "$FAKE_MAIN_SEQUENCE_FILE"; [ "$n" -ge 2 ] && { printf "%s" "${FAKE_ADVANCED_MAIN:?}"; exit 0; }; fi; cat "$state/remote-main"; exit 0; fi' \
  '  if [[ "$1" == */git/ref/tags/* ]]; then if [ "${FAKE_TAG_API_FAIL:-}" = 1 ]; then printf "%s\n" "HTTP/2 500 Internal Server Error"; exit 1; fi; if [ "${FAKE_TAG_MISSING:-}" = 1 ] || { [ -z "${FAKE_TAG_TARGET:-}" ] && [ -f "$state/draft" ] && [ "$(<"$state/draft")" = true ]; }; then printf "%s\n" "HTTP/2 404 Not Found"; exit 1; fi; printf "%s\n%s\n" "HTTP/2 200 OK" "{\"object\":{\"sha\":\"annotated-tag-object\"}}"; exit 0; fi' \
  '  if [[ "$1" == */commits/* ]]; then printf "%s" "${FAKE_TAG_COMMIT:-${FAKE_TAG_TARGET:-$(<"$state/target")}}"; exit 0; fi' \
  '  if [ -f "$state/tag" ]; then render_api_release; fi' \
  '  exit 0' \
  'fi' \
  'test "$command" = release' \
  'action="$1"; shift' \
  'case "$action" in' \
  '  create)' \
  '    tag="$1"; shift; target=""' \
  '    while [ "$#" -gt 0 ]; do if [ "$1" = --target ]; then target="$2"; shift 2; else shift; fi; done' \
  '    mkdir -p "$state/assets"' \
  '    printf "%s" "$tag" > "$state/tag"' \
  '    printf "%s" "$target" > "$state/target"' \
  '    printf "%s" true > "$state/draft"' \
  '    printf "%s" false > "$state/prerelease"' \
  '    ;;' \
  '  upload)' \
  '    tag="$1"; asset="$2"' \
  '    test "$tag" = "$(<"$state/tag")"' \
  '    count=0; if [ -f "$state/upload-count" ]; then count="$(<"$state/upload-count")"; fi' \
  '    count=$((count + 1)); printf "%s" "$count" > "$state/upload-count"' \
  '    if [ "${FAKE_GH_FAIL_UPLOAD_AT:-}" = "$count" ]; then exit 1; fi' \
  '    cp "$asset" "$state/assets/$(basename "$asset")"' \
  '    ;;' \
  '  download)' \
  '    shift; destination=""' \
  '    while [ "$#" -gt 0 ]; do if [ "$1" = --dir ]; then destination="$2"; shift 2; else shift; fi; done' \
  '    cp "$state/assets/"* "$destination/"' \
  '    ;;' \
  '  view)' \
  '    if [ "${1:-}" = --repo ]; then printf "%s" "${FAKE_LATEST_TAG:-$(<"$state/tag")}"; else render_view_release; fi' \
  '    ;;' \
  '  edit)' \
  '    if [ "${FAKE_GH_FAIL_PUBLISH:-}" = 1 ]; then exit 1; fi' \
  '    printf "%s" false > "$state/draft"' \
  '    if [ "${FAKE_GH_PUBLISH_PRERELEASE:-}" = 1 ]; then printf "%s" true > "$state/prerelease"; fi' \
  '    ;;' \
  '  *) exit 2 ;;' \
  'esac' > "$temp_dir/publish-bin/gh"
chmod 0755 "$temp_dir/publish-bin/gh"
printf '%s\n' 'Release test notes' > "$temp_dir/release-notes.md"
release_target='0123456789abcdef0123456789abcdef01234567'
for failed_upload in $(seq 1 7); do
  publish_state="$temp_dir/publish-state-$failed_upload"
  mkdir -p "$publish_state"
  printf '%s' "$release_target" > "$publish_state/remote-main"
  if PATH="$temp_dir/publish-bin:$PATH" FAKE_GH_STATE="$publish_state" \
    FAKE_GH_FAIL_UPLOAD_AT="$failed_upload" \
    "$root/scripts/publish-draft-release.sh" 'git-blame-dev/vox-golem' \
    'v2026.7.27-42' "$release_target" "$release_dir" "$temp_dir/release-notes.md"; then
    printf 'Release publication unexpectedly survived upload failure %s.\n' "$failed_upload" >&2
    exit 1
  fi
  test "$(<"$publish_state/draft")" = true
  PATH="$temp_dir/publish-bin:$PATH" FAKE_GH_STATE="$publish_state" \
    "$root/scripts/publish-draft-release.sh" 'git-blame-dev/vox-golem' \
    'v2026.7.27-42' "$release_target" "$release_dir" "$temp_dir/release-notes.md"
  test "$(<"$publish_state/draft")" = false
done

publish_state="$temp_dir/publish-state-main-race"
mkdir -p "$publish_state"
printf '%s' "$release_target" > "$publish_state/remote-main"
if PATH="$temp_dir/publish-bin:$PATH" FAKE_GH_STATE="$publish_state" \
  FAKE_MAIN_SEQUENCE_FILE="$temp_dir/main-sequence" \
  FAKE_ADVANCED_MAIN='abcdef0123456789abcdef0123456789abcdef01' \
  "$root/scripts/publish-draft-release.sh" 'git-blame-dev/vox-golem' \
  'v2026.7.27-42' "$release_target" "$release_dir" "$temp_dir/release-notes.md"; then
  printf '%s\n' 'Publication unexpectedly survived a main-branch race.' >&2
  exit 1
fi
test "$(<"$publish_state/draft")" = true

# A normal draft has no materialized tag; targetCommitish is used instead.
publish_state="$temp_dir/publish-state-tagless-draft"
mkdir -p "$publish_state"
printf '%s' "$release_target" > "$publish_state/remote-main"
PATH="$temp_dir/publish-bin:$PATH" FAKE_GH_STATE="$publish_state" \
  "$root/scripts/publish-draft-release.sh" 'git-blame-dev/vox-golem' \
  'v2026.7.27-42' "$release_target" "$release_dir" "$temp_dir/release-notes.md"
test "$(<"$publish_state/draft")" = false

# Existing annotated tags are peeled through commits/<tag> rather than using
# the tag object's SHA.
FAKE_TAG_TARGET='annotated-tag-object' FAKE_TAG_COMMIT="$release_target" \
PATH="$temp_dir/publish-bin:$PATH" FAKE_GH_STATE="$publish_state" \
  "$root/scripts/publish-draft-release.sh" 'git-blame-dev/vox-golem' \
  'v2026.7.27-42' "$release_target" "$release_dir" "$temp_dir/release-notes.md"

# A published release without a tag must be refused without mutation.
publish_state="$temp_dir/publish-state-published-missing-tag"
mkdir -p "$publish_state/assets"
cp "$release_dir"/* "$publish_state/assets/"
printf '%s' "$release_target" > "$publish_state/target"
printf '%s' "$release_target" > "$publish_state/remote-main"
printf '%s' false > "$publish_state/draft"
printf '%s' false > "$publish_state/prerelease"
if PATH="$temp_dir/publish-bin:$PATH" FAKE_GH_STATE="$publish_state" FAKE_TAG_MISSING=1 \
  "$root/scripts/publish-draft-release.sh" 'git-blame-dev/vox-golem' \
  'v2026.7.27-42' "$release_target" "$release_dir" "$temp_dir/release-notes.md"; then
  printf '%s\n' 'Published release without a tag was unexpectedly accepted.' >&2
  exit 1
fi
test "$(<"$publish_state/draft")" = false

# A tag API outage is not equivalent to an absent draft tag.
publish_state="$temp_dir/publish-state-tag-api-error"
mkdir -p "$publish_state"
printf '%s' "$release_target" > "$publish_state/remote-main"
if PATH="$temp_dir/publish-bin:$PATH" FAKE_GH_STATE="$publish_state" FAKE_TAG_API_FAIL=1 \
  "$root/scripts/publish-draft-release.sh" 'git-blame-dev/vox-golem' \
  'v2026.7.27-42' "$release_target" "$release_dir" "$temp_dir/release-notes.md"; then
  printf '%s\n' 'Tag API failure was unexpectedly accepted.' >&2
  exit 1
fi
test "$(<"$publish_state/draft")" = true

publish_state="$temp_dir/publish-state-final"
mkdir -p "$publish_state"
printf '%s' "$release_target" > "$publish_state/remote-main"
if PATH="$temp_dir/publish-bin:$PATH" FAKE_GH_STATE="$publish_state" FAKE_GH_FAIL_PUBLISH=1 \
  "$root/scripts/publish-draft-release.sh" 'git-blame-dev/vox-golem' \
  'v2026.7.27-42' "$release_target" "$release_dir" "$temp_dir/release-notes.md"; then
  printf '%s\n' 'Release publication unexpectedly survived final publish failure.' >&2
  exit 1
fi
test "$(<"$publish_state/draft")" = true
PATH="$temp_dir/publish-bin:$PATH" FAKE_GH_STATE="$publish_state" \
  "$root/scripts/publish-draft-release.sh" 'git-blame-dev/vox-golem' \
  'v2026.7.27-42' "$release_target" "$release_dir" "$temp_dir/release-notes.md"
test "$(<"$publish_state/draft")" = false

# Verification-only reruns of an older published release must not depend on current main/latest.
printf '%s' 'abcdef0123456789abcdef0123456789abcdef01' > "$publish_state/remote-main"
published_upload_count="$(<"$publish_state/upload-count")"
PATH="$temp_dir/publish-bin:$PATH" FAKE_GH_STATE="$publish_state" \
  FAKE_LATEST_TAG='v2026.8.1-99' \
  "$root/scripts/publish-draft-release.sh" 'git-blame-dev/vox-golem' \
  'v2026.7.27-42' "$release_target" "$release_dir" "$temp_dir/release-notes.md"
test "$(<"$publish_state/draft")" = false
test "$(<"$publish_state/upload-count")" = "$published_upload_count"

publish_state="$temp_dir/publish-state-prerelease-draft"
mkdir -p "$publish_state/assets"
printf '%s' 'v2026.7.27-42' > "$publish_state/tag"
printf '%s' "$release_target" > "$publish_state/target"
printf '%s' "$release_target" > "$publish_state/remote-main"
printf '%s' true > "$publish_state/draft"
printf '%s' true > "$publish_state/prerelease"
if PATH="$temp_dir/publish-bin:$PATH" FAKE_GH_STATE="$publish_state" \
  "$root/scripts/publish-draft-release.sh" 'git-blame-dev/vox-golem' \
  'v2026.7.27-42' "$release_target" "$release_dir" "$temp_dir/release-notes.md"; then
  printf '%s\n' 'Prerelease draft unexpectedly qualified for publication.' >&2
  exit 1
fi
test ! -e "$publish_state/upload-count"

publish_state="$temp_dir/publish-state-wrong-tag"
mkdir -p "$publish_state"
printf '%s' "$release_target" > "$publish_state/remote-main"
if PATH="$temp_dir/publish-bin:$PATH" FAKE_GH_STATE="$publish_state" \
  FAKE_TAG_TARGET='abcdef0123456789abcdef0123456789abcdef01' \
  "$root/scripts/publish-draft-release.sh" 'git-blame-dev/vox-golem' \
  'v2026.7.27-42' "$release_target" "$release_dir" "$temp_dir/release-notes.md"; then
  printf '%s\n' 'Publication unexpectedly accepted a tag pointing to different source.' >&2
  exit 1
fi
test "$(<"$publish_state/draft")" = true

publish_state="$temp_dir/publish-state-wrong-latest"
mkdir -p "$publish_state"
printf '%s' "$release_target" > "$publish_state/remote-main"
if PATH="$temp_dir/publish-bin:$PATH" FAKE_GH_STATE="$publish_state" \
  FAKE_LATEST_TAG='v2026.7.26-1' \
  "$root/scripts/publish-draft-release.sh" 'git-blame-dev/vox-golem' \
  'v2026.7.27-42' "$release_target" "$release_dir" "$temp_dir/release-notes.md"; then
  printf '%s\n' 'Publication unexpectedly accepted an incorrect latest release.' >&2
  exit 1
fi
test "$(<"$publish_state/draft")" = false

publish_state="$temp_dir/publish-state-post-prerelease"
mkdir -p "$publish_state"
printf '%s' "$release_target" > "$publish_state/remote-main"
if PATH="$temp_dir/publish-bin:$PATH" FAKE_GH_STATE="$publish_state" \
  FAKE_GH_PUBLISH_PRERELEASE=1 \
  "$root/scripts/publish-draft-release.sh" 'git-blame-dev/vox-golem' \
  'v2026.7.27-42' "$release_target" "$release_dir" "$temp_dir/release-notes.md"; then
  printf '%s\n' 'Publication unexpectedly accepted post-promotion prerelease metadata.' >&2
  exit 1
fi
test "$(<"$publish_state/prerelease")" = true

publish_state="$temp_dir/publish-state-stale-main"
mkdir -p "$publish_state"
printf '%s' 'abcdef0123456789abcdef0123456789abcdef01' > "$publish_state/remote-main"
if PATH="$temp_dir/publish-bin:$PATH" FAKE_GH_STATE="$publish_state" \
  "$root/scripts/publish-draft-release.sh" 'git-blame-dev/vox-golem' \
  'v2026.7.27-42' "$release_target" "$release_dir" "$temp_dir/release-notes.md"; then
  printf '%s\n' 'Release publication unexpectedly accepted an advanced main branch.' >&2
  exit 1
fi
test "$(<"$publish_state/draft")" = true

jq 'del(.platforms["windows-x86_64"])' "$release_dir/latest.json" > "$temp_dir/incomplete.json"
mv "$temp_dir/incomplete.json" "$release_dir/latest.json"
(
  cd "$release_dir"
  sha256sum \
    'vox-golem-linux-v2026.7.27-42.zip' \
    'vox-golem-linux-x86_64-v2026.7.27-42.AppImage' \
    'vox-golem-linux-x86_64-v2026.7.27-42.AppImage.sig' \
    'vox-golem-windows-x86_64-v2026.7.27-42-setup.exe' \
    'vox-golem-windows-x86_64-v2026.7.27-42-setup.exe.sig' \
    latest.json > SHA256SUMS
)
if GITHUB_REPOSITORY='git-blame-dev/vox-golem' "$root/scripts/verify-release-assets.sh" \
  "$release_dir" 'v2026.7.27-42'; then
  printf '%s\n' 'Release verification unexpectedly accepted an incomplete updater manifest.' >&2
  exit 1
fi

mkdir -p "$temp_dir/release-api-bin"
# The generated stub must retain these variables for its own runtime.
# shellcheck disable=SC2016
printf '%s\n' \
  '#!/usr/bin/env bash' \
  'set -eu' \
  'test "$#" -eq 5' \
  'test "$1" = api' \
  'test "$2" = --paginate' \
  'test "$3" = "repos/example/repository/releases?per_page=100"' \
  'test "$4" = --jq' \
  'test "$5" = ".[]"' \
  'if [ "${FAKE_GH_FAIL:-}" = 1 ]; then exit 1; fi' \
  'printf "%s\n" "$FAKE_GH_RELEASES"' > "$temp_dir/release-api-bin/gh"
chmod 0755 "$temp_dir/release-api-bin/gh"

selector_history="$(printf '%s\n' \
  '{"draft":false,"prerelease":false,"tag_name":"v2026.9.17-1","target_commitish":"older-source"}' \
  '{"draft":false,"prerelease":false,"tag_name":"v2026.9.17-3","target_commitish":"other-source"}' \
  '{"draft":true,"prerelease":false,"tag_name":"v2026.9.17-4","target_commitish":"draft-source"}')"
selected_version="$(
  PATH="$temp_dir/release-api-bin:$PATH" FAKE_GH_RELEASES='' \
    "$root/scripts/select-release-version.sh" example/repository new-source 2026.9.17
)"
test "$selected_version" = '2026.9.17-1'
selected_version="$(
  PATH="$temp_dir/release-api-bin:$PATH" FAKE_GH_RELEASES="$selector_history" \
    "$root/scripts/select-release-version.sh" example/repository new-source 2026.9.17
)"
test "$selected_version" = '2026.9.17-5'
selected_version="$(
  PATH="$temp_dir/release-api-bin:$PATH" \
    FAKE_GH_RELEASES="$selector_history"$'\n''{"draft":true,"prerelease":false,"tag_name":"v2026.9.16-2","target_commitish":"retry-source"}' \
    "$root/scripts/select-release-version.sh" example/repository retry-source 2026.9.18
)"
test "$selected_version" = '2026.9.16-2'
if PATH="$temp_dir/release-api-bin:$PATH" FAKE_GH_FAIL=1 FAKE_GH_RELEASES='' \
  "$root/scripts/select-release-version.sh" example/repository new-source 2026.9.17; then
  printf '%s\n' 'Failed release-history lookup unexpectedly allocated a release version.' >&2
  exit 1
fi
if PATH="$temp_dir/release-api-bin:$PATH" \
  FAKE_GH_RELEASES='{"draft":1,"prerelease":false,"tag_name":"v2026.9.17-1","target_commitish":"older-source"}' \
  "$root/scripts/select-release-version.sh" example/repository new-source 2026.9.17; then
  printf '%s\n' 'Malformed release state unexpectedly allocated a release version.' >&2
  exit 1
fi
if PATH="$temp_dir/release-api-bin:$PATH" \
  FAKE_GH_RELEASES='{"draft":false,"prerelease":true,"tag_name":"v2026.9.17-1","target_commitish":"same-source"}' \
  "$root/scripts/select-release-version.sh" example/repository same-source 2026.9.17; then
  printf '%s\n' 'Same-source canonical prerelease unexpectedly allocated a release version.' >&2
  exit 1
fi
if PATH="$temp_dir/release-api-bin:$PATH" \
  FAKE_GH_RELEASES='{"draft":false,"prerelease":true,"tag_name":"v2026.9.17-1","target_commitish":"same-source"}
{"draft":true,"prerelease":false,"tag_name":"v2026.9.17-2","target_commitish":"same-source"}' \
  "$root/scripts/select-release-version.sh" example/repository same-source 2026.9.17; then
  printf '%s\n' 'Prerelease collision unexpectedly selected a release version.' >&2
  exit 1
fi
if PATH="$temp_dir/release-api-bin:$PATH" \
  FAKE_GH_RELEASES='{"draft":false,"prerelease":false,"tag_name":"v2026.9.17-1","target_commitish":"older-source"}
{"draft":true,"prerelease":false,"tag_name":"v2026.9.17-1","target_commitish":"other-source"}' \
  "$root/scripts/select-release-version.sh" example/repository new-source 2026.9.17; then
  printf '%s\n' 'Duplicate release tag unexpectedly allocated a release version.' >&2
  exit 1
fi
if PATH="$temp_dir/release-api-bin:$PATH" \
  FAKE_GH_RELEASES='{"draft":false,"prerelease":false,"tag_name":"v2026.9.16-1","target_commitish":"same-source"}
{"draft":true,"prerelease":false,"tag_name":"v2026.9.17-2","target_commitish":"same-source"}' \
  "$root/scripts/select-release-version.sh" example/repository same-source 2026.9.17; then
  printf '%s\n' 'Ambiguous same-source releases unexpectedly selected a release version.' >&2
  exit 1
fi

release_history="$(printf '%s\n' \
  '{"draft":true,"prerelease":false,"tag_name":"v2026.7.27-99","assets":[{"name":"latest.json","url":"https://example.invalid/draft"}]}' \
  '{"draft":false,"prerelease":true,"tag_name":"v2026.7.27-100","assets":[{"name":"latest.json","url":"https://example.invalid/prerelease"}]}' \
  '{"draft":false,"prerelease":false,"tag_name":"v2026.7.27-101","assets":[]}' \
  '{"draft":false,"prerelease":false,"tag_name":"v2026.7.27-42","assets":[{"name":"latest.json","url":"https://example.invalid/older-updater"}]}' \
  '{"draft":false,"prerelease":false,"tag_name":"v2026.7.27-9999","assets":[{"name":"latest.json","url":"https://example.invalid/greatest-updater"}]}' \
  '{"draft":false,"prerelease":false,"tag_name":"v2027.1.1","assets":[{"name":"latest.json","url":"https://example.invalid/noncanonical-stable"}]}' \
  "$(for index in $(seq 1 101); do printf '{"draft":false,"prerelease":false,"tag_name":"v2025.1.1-%s","assets":[]}\n' "$index"; done)")"
manifest_url="$(
  PATH="$temp_dir/release-api-bin:$PATH" FAKE_GH_RELEASES="$release_history" \
    SELECTED_UPDATER_VERSION_FILE="$temp_dir/selected-updater-version" \
    "$root/scripts/select-updater-manifest-url.sh" 'example/repository'
)"
test "$manifest_url" = 'https://example.invalid/greatest-updater'
test "$(<"$temp_dir/selected-updater-version")" = '2026.7.27-9999'

printf '%s' 'stale-version' > "$temp_dir/selected-updater-version"
manifest_url="$(
  PATH="$temp_dir/release-api-bin:$PATH" \
    FAKE_GH_RELEASES='{"draft":false,"prerelease":false,"assets":[]}' \
    SELECTED_UPDATER_VERSION_FILE="$temp_dir/selected-updater-version" \
    "$root/scripts/select-updater-manifest-url.sh" 'example/repository'
)"
test -z "$manifest_url"
test ! -s "$temp_dir/selected-updater-version"
if PATH="$temp_dir/release-api-bin:$PATH" FAKE_GH_FAIL=1 FAKE_GH_RELEASES='' \
  "$root/scripts/select-updater-manifest-url.sh" 'example/repository'; then
  printf '%s\n' 'Failed release-history lookup unexpectedly allowed publication.' >&2
  exit 1
fi

"$root/scripts/test-release-workflow.sh"

mkdir -p "$temp_dir/AppDir/apprun-hooks" "$temp_dir/AppDir/usr/lib" "$temp_dir/providers"
printf '%s\n' '#!/bin/sh' 'export GTK_THEME=Adwaita' 'export GDK_BACKEND=x11 # forced fallback' > \
  "$temp_dir/AppDir/apprun-hooks/linuxdeploy-plugin-gtk.sh"
printf '%s\n' 'shared provider' > "$temp_dir/providers/libonnxruntime_providers_shared.so"
printf '%s\n' 'cuda provider' > "$temp_dir/providers/libonnxruntime_providers_cuda.so"
"$root/scripts/prepare-update-appdir.sh" "$temp_dir/AppDir" "$temp_dir/providers"
if grep -Fq 'GDK_BACKEND=x11' "$temp_dir/AppDir/apprun-hooks/linuxdeploy-plugin-gtk.sh"; then
  printf '%s\n' 'X11 override survived AppDir preparation.' >&2
  exit 1
fi
test -x "$temp_dir/AppDir/usr/lib/libonnxruntime_providers_shared.so"
test -x "$temp_dir/AppDir/usr/lib/libonnxruntime_providers_cuda.so"

mkdir -p "$temp_dir/bin" "$temp_dir/plugin-cache"
# The generated stub must retain these variables for its own runtime.
# shellcheck disable=SC2016
printf '%s\n' \
  '#!/usr/bin/env bash' \
  'set -eu' \
  'if [ -n "${FAKE_CURL_ARGS:-}" ]; then printf "%s\n" "$@" > "$FAKE_CURL_ARGS"; fi' \
  'while [ "$#" -gt 0 ]; do' \
  '  if [ "$1" = "--output" ]; then output="$2"; shift 2; else shift; fi' \
  'done' \
  'cp "$FAKE_CURL_SOURCE" "$output"' > "$temp_dir/bin/curl"
chmod 0755 "$temp_dir/bin/curl"

marker="$temp_dir/plugin-executed"
plugin="$temp_dir/plugin-cache/linuxdeploy-plugin-appimage.AppImage"
printf '#!/usr/bin/env bash\nprintf %s executed > %s\n' "'%s'" "$marker" > "$plugin"
chmod 0755 "$plugin"
printf '%s\n' 'wrong plugin download' > "$temp_dir/wrong-plugin"
if PATH="$temp_dir/bin:$PATH" FAKE_CURL_SOURCE="$temp_dir/wrong-plugin" \
  "$root/scripts/prepare-tauri-bundler-tool.sh" "$plugin" 'https://example.invalid/plugin' \
  '0000000000000000000000000000000000000000000000000000000000000000'; then
  printf '%s\n' 'Invalid AppImage plugin unexpectedly passed verification.' >&2
  exit 1
fi
test ! -e "$plugin"
test ! -e "$marker"

printf '%s\n' 'verified plugin' > "$temp_dir/verified-plugin"
verified_sha256="$(sha256sum "$temp_dir/verified-plugin" | cut -d ' ' -f 1)"
PATH="$temp_dir/bin:$PATH" FAKE_CURL_SOURCE="$temp_dir/verified-plugin" \
  FAKE_CURL_ARGS="$temp_dir/curl-args" GITHUB_TOKEN='test-ci-token' \
  "$root/scripts/prepare-tauri-bundler-tool.sh" "$plugin" \
  'https://api.github.com/repos/example/tools/releases/assets/1' \
  "$verified_sha256"
test -x "$plugin"
printf '%s  %s\n' "$verified_sha256" "$plugin" | sha256sum -c - >/dev/null
grep -Fx 'Authorization: Bearer test-ci-token' "$temp_dir/curl-args" >/dev/null

rm -f "$plugin"
PATH="$temp_dir/bin:$PATH" FAKE_CURL_SOURCE="$temp_dir/verified-plugin" \
  FAKE_CURL_ARGS="$temp_dir/non-api-curl-args" GITHUB_TOKEN='test-ci-token' \
  "$root/scripts/prepare-tauri-bundler-tool.sh" "$plugin" 'https://example.invalid/plugin' \
  "$verified_sha256"
if grep -F 'test-ci-token' "$temp_dir/non-api-curl-args"; then
  printf '%s\n' 'GitHub token was forwarded to a non-API URL.' >&2
  exit 1
fi

rm -f "$plugin"
PATH="$temp_dir/bin:$PATH" FAKE_CURL_SOURCE="$temp_dir/verified-plugin" \
  FAKE_CURL_ARGS="$temp_dir/unauthenticated-curl-args" GITHUB_TOKEN='' \
  "$root/scripts/prepare-tauri-bundler-tool.sh" "$plugin" \
  'https://api.github.com/repos/example/tools/releases/assets/1' \
  "$verified_sha256"
if grep -F 'Authorization:' "$temp_dir/unauthenticated-curl-args"; then
  printf '%s\n' 'Authorization header was added without a GitHub token.' >&2
  exit 1
fi

rm -f "$plugin"
mkdir "$plugin"
printf '%s\n' 'planted directory content' > "$plugin/content"
PATH="$temp_dir/bin:$PATH" FAKE_CURL_SOURCE="$temp_dir/verified-plugin" \
  "$root/scripts/prepare-tauri-bundler-tool.sh" "$plugin" 'https://example.invalid/plugin' \
  "$verified_sha256"
test -x "$plugin"

rm -f "$plugin"
mkdir "$temp_dir/planted-directory"
ln -s "$temp_dir/planted-directory" "$plugin"
PATH="$temp_dir/bin:$PATH" FAKE_CURL_SOURCE="$temp_dir/verified-plugin" \
  "$root/scripts/prepare-tauri-bundler-tool.sh" "$plugin" 'https://example.invalid/plugin' \
  "$verified_sha256"
test -x "$plugin"
test -d "$temp_dir/planted-directory"

healthy_log="$temp_dir/healthy-appimage.log"
printf '%s\n' 'VOXGOLEM_STARTUP_READY' > "$healthy_log"
sleep 3 & healthy_pid=$!
"$root/scripts/wait-for-appimage-startup.sh" "$healthy_pid" "$healthy_log" 2 1
wait "$healthy_pid"

delayed_warning_log="$temp_dir/delayed-warning-appimage.log"
(
  printf '%s\n' 'VOXGOLEM_STARTUP_READY'
  sleep 1
  printf '%s\n' 'GStreamer element appsink not found. Please install it.'
  sleep 2
) > "$delayed_warning_log" & delayed_warning_pid=$!
if "$root/scripts/wait-for-appimage-startup.sh" \
  "$delayed_warning_pid" "$delayed_warning_log" 2 2; then
  printf '%s\n' 'Delayed GStreamer warning unexpectedly passed AppImage smoke.' >&2
  exit 1
fi
wait "$delayed_warning_pid"
