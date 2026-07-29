#!/usr/bin/env bash
set -eu -o pipefail

installer="${1:?usage: verify-windows-installer.sh INSTALLER MAX_BYTES}"
max_bytes="${2:?usage: verify-windows-installer.sh INSTALLER MAX_BYTES}"
root="$(cd "$(dirname "$0")/.." && pwd)"
file_command="${VOXGOLEM_FILE:-file}"
if [ ! -f "$installer" ] || [ -L "$installer" ]; then
  printf 'Windows installer must be a regular non-symlink file: %s\n' "$installer" >&2
  exit 1
fi
command -v 7z >/dev/null || {
  printf '%s\n' 'Missing 7z. Install 7zip before verifying the Windows installer.' >&2
  exit 1
}
command -v "$file_command" >/dev/null || {
  printf 'Missing installer content inspector: %s\n' "$file_command" >&2
  exit 1
}
"$file_command" "$installer" | grep -Fq 'Nullsoft Installer self-extracting archive' || {
  printf '%s\n' 'Windows installer is not an NSIS executable.' >&2
  exit 1
}

size="$(stat -c '%s' "$installer")"
test "$size" -le "$max_bytes" || {
  printf 'Windows installer exceeds size limit: %s > %s bytes.\n' "$size" "$max_bytes" >&2
  exit 1
}

expected=(
  DirectML.dll
  MSVCP140.dll
  MSVCP140_1.dll
  VCRUNTIME140.dll
  VCRUNTIME140_1.dll
  onnxruntime_providers_cuda.dll
  onnxruntime_providers_shared.dll
  vox-golem.exe
)
expected_internals=(
  "\$PLUGINSDIR/NSISdl.dll"
  "\$PLUGINSDIR/StartMenu.dll"
  "\$PLUGINSDIR/System.dll"
  "\$PLUGINSDIR/modern-wizard.bmp"
  "\$PLUGINSDIR/nsDialogs.dll"
  "\$PLUGINSDIR/nsis_tauri_utils.dll"
)
expected_archive=("${expected_internals[@]}" "${expected[@]}" uninstall.exe)
mapfile -t expected_archive < <(printf '%s\n' "${expected_archive[@]}" | LC_ALL=C sort)
workspace="$(mktemp -d)"
trap 'rm -rf "$workspace"' EXIT
listing="$workspace/listing.txt"
7z l -slt "$installer" > "$listing"

archive_records=()
while IFS= read -r -d '' value; do
  archive_records+=("$value")
done < <(
  in_entries=false
  record_path=''
  record_size=''
  record_attributes=''
  emit_record() {
    if [ -n "$record_path" ]; then
      printf '%s\0%s\0%s\0' "$record_path" "$record_size" "$record_attributes"
    fi
    record_path=''
    record_size=''
    record_attributes=''
  }
  while IFS= read -r line; do
    if [ "$line" = '----------' ]; then
      in_entries=true
      continue
    fi
    if [ "$in_entries" != true ]; then
      continue
    fi
    if [ -z "$line" ]; then
      emit_record
      continue
    fi
    case "$line" in
      'Path = '*) record_path="${line#Path = }" ;;
      'Size = '*) record_size="${line#Size = }" ;;
      'Attributes = '*) record_attributes="${line#Attributes = }" ;;
    esac
  done < "$listing"
  emit_record
)
if [ "$(( ${#archive_records[@]} % 3 ))" -ne 0 ]; then
  printf '%s\n' 'Failed to parse Windows installer archive records.' >&2
  exit 1
fi

archive_entries=()
for ((index = 0; index < ${#archive_records[@]}; index += 3)); do
  archive_entries+=("${archive_records[index]}")
done
mapfile -t archive_entries < <(printf '%s\n' "${archive_entries[@]}" | LC_ALL=C sort)

if [ "${archive_entries[*]}" != "${expected_archive[*]}" ]; then
  printf '%s\n' 'Windows installer contents do not match the exact payload and internal allowlist.' >&2
  printf 'Expected: %s\n' "${expected_archive[*]}" >&2
  printf 'Actual:   %s\n' "${archive_entries[*]}" >&2
  exit 1
fi

max_unpacked_bytes_for() {
  case "$1" in
    DirectML.dll) printf '%s\n' 25000000 ;;
    MSVCP140.dll) printf '%s\n' 1000000 ;;
    MSVCP140_1.dll) printf '%s\n' 100000 ;;
    VCRUNTIME140.dll) printf '%s\n' 500000 ;;
    VCRUNTIME140_1.dll) printf '%s\n' 100000 ;;
    onnxruntime_providers_cuda.dll) printf '%s\n' 110000000 ;;
    onnxruntime_providers_shared.dll) printf '%s\n' 100000 ;;
    vox-golem.exe) printf '%s\n' 50000000 ;;
    uninstall.exe|"\$PLUGINSDIR/"*) printf '%s\n' 500000 ;;
    *) return 1 ;;
  esac
}

max_declared_total_bytes=176000000
declared_total_bytes=0
for ((index = 0; index < ${#archive_records[@]}; index += 3)); do
  path="${archive_records[index]}"
  unpacked_bytes="${archive_records[index + 1]}"
  if [ -z "$unpacked_bytes" ] && [ "$path" = uninstall.exe ]; then
    continue
  fi
  case "$unpacked_bytes" in
    ''|*[!0-9]*)
      printf 'Installer archive entry has no valid declared size: %s\n' "$path" >&2
      exit 1
      ;;
  esac
  if [ "${#unpacked_bytes}" -gt 9 ]; then
    printf 'Installer archive entry has an excessive declared size: %s\n' "$path" >&2
    exit 1
  fi
  max_entry_bytes="$(max_unpacked_bytes_for "$path")"
  if [ "$unpacked_bytes" -gt "$max_entry_bytes" ]; then
    printf 'Installer archive entry exceeds its declared size limit: %s (%s > %s bytes).\n' \
      "$path" "$unpacked_bytes" "$max_entry_bytes" >&2
    exit 1
  fi
  declared_total_bytes=$((declared_total_bytes + unpacked_bytes))
  if [ "$declared_total_bytes" -gt "$max_declared_total_bytes" ]; then
    printf 'Installer archive exceeds its declared unpacked size limit: %s > %s bytes.\n' \
      "$declared_total_bytes" "$max_declared_total_bytes" >&2
    exit 1
  fi
done

7z t -bd "$installer" >/dev/null
extracted="$workspace/extracted"
mkdir "$extracted"
(ulimit -f 1024; 7z x -bd -y "-o$extracted" "$installer" uninstall.exe >/dev/null)
7z x -bd -y "-o$extracted" "$installer" '-x!uninstall.exe' >/dev/null

expected_extracted=("\$PLUGINSDIR|d" "uninstall.exe|f")
for path in "${expected_internals[@]}" "${expected[@]}"; do
  expected_extracted+=("$path|f")
done
mapfile -t expected_extracted < <(printf '%s\n' "${expected_extracted[@]}" | LC_ALL=C sort)
mapfile -t actual_extracted < <(
  find "$extracted" -mindepth 1 -printf '%P|%y\n' | LC_ALL=C sort
)
if [ "${actual_extracted[*]}" != "${expected_extracted[*]}" ]; then
  printf '%s\n' 'Extracted installer tree does not match the exact file-type allowlist.' >&2
  printf 'Expected: %s\n' "${expected_extracted[*]}" >&2
  printf 'Actual:   %s\n' "${actual_extracted[*]}" >&2
  exit 1
fi

uninstaller="$extracted/uninstall.exe"
uninstaller_bytes="$(stat -c '%s' "$uninstaller")"
if [ "$uninstaller_bytes" -gt 500000 ]; then
  printf 'Extracted uninstaller exceeds its size limit: %s bytes.\n' "$uninstaller_bytes" >&2
  exit 1
fi
"$file_command" "$uninstaller" | grep -Eq 'PE32 executable.*Intel 80386' || {
  printf '%s\n' 'Extracted uninstaller is not a 32-bit PE executable.' >&2
  exit 1
}

for internal in "${expected_internals[@]}"; do
  internal_path="$extracted/$internal"
  if [ ! -f "$internal_path" ] || [ -L "$internal_path" ]; then
    printf 'Failed to extract regular NSIS internal file: %s\n' "$internal" >&2
    exit 1
  fi
  internal_bytes="$(stat -c '%s' "$internal_path")"
  if [ "$internal_bytes" -gt 500000 ]; then
    printf 'NSIS internal file exceeds its size limit: %s (%s bytes).\n' \
      "$internal" "$internal_bytes" >&2
    exit 1
  fi
  case "$internal" in
    *.dll)
      "$file_command" "$internal_path" | grep -Eq 'PE32 executable.*Intel 80386' || {
        printf 'NSIS internal file is not a 32-bit PE DLL: %s\n' "$internal" >&2
        exit 1
      }
      ;;
    *.bmp)
      "$file_command" "$internal_path" | grep -Fq 'bitmap' || {
        printf 'NSIS internal file is not a bitmap: %s\n' "$internal" >&2
        exit 1
      }
      ;;
  esac
done
mkdir "$extracted/payload"
for file in "${expected[@]}"; do
  if [ ! -f "$extracted/$file" ] || [ -L "$extracted/$file" ]; then
    printf 'Failed to extract regular Windows installer payload file: %s\n' "$file" >&2
    exit 1
  fi
  mv "$extracted/$file" "$extracted/payload/$file"
done
"$root/scripts/verify-windows-package.sh" "$extracted/payload"
printf 'Windows NSIS installer: %s (%s bytes)\n' "$installer" "$size"
