#!/usr/bin/env bash
set -eu -o pipefail

package_dir="${1:?usage: verify-windows-package.sh PACKAGE_DIRECTORY}"
objdump="${VOXGOLEM_OBJDUMP:-llvm-objdump}"
file_command="${VOXGOLEM_FILE:-file}"
test -d "$package_dir" || {
  printf 'Windows package directory does not exist: %s\n' "$package_dir" >&2
  exit 1
}
command -v "$objdump" >/dev/null || {
  printf 'Missing PE import inspector: %s\n' "$objdump" >&2
  exit 1
}
command -v "$file_command" >/dev/null || {
  printf 'Missing PE architecture inspector: %s\n' "$file_command" >&2
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
max_total_bytes=175000000
mapfile -t actual < <(find "$package_dir" -mindepth 1 -maxdepth 1 -printf '%f\n' | LC_ALL=C sort)

if [ "${actual[*]}" != "${expected[*]}" ]; then
  printf '%s\n' 'Windows package does not match the exact lightweight allowlist.' >&2
  printf 'Expected: %s\n' "${expected[*]}" >&2
  printf 'Actual:   %s\n' "${actual[*]}" >&2
  exit 1
fi

total_bytes=0
for file in "${expected[@]}"; do
  path="$package_dir/$file"
  if [ ! -f "$path" ] || [ -L "$path" ]; then
    printf 'Windows package entry must be a regular non-symlink file: %s\n' "$file" >&2
    exit 1
  fi
  "$file_command" "$path" | grep -Eq 'PE32\+ executable.*x86-64' || {
    printf 'Windows package entry is not an x86-64 PE file: %s\n' "$file" >&2
    exit 1
  }
  file_bytes="$(stat -c '%s' "$path")"
  case "$file" in
    DirectML.dll) max_file_bytes=25000000 ;;
    MSVCP140.dll) max_file_bytes=1000000 ;;
    MSVCP140_1.dll) max_file_bytes=100000 ;;
    VCRUNTIME140.dll) max_file_bytes=500000 ;;
    VCRUNTIME140_1.dll) max_file_bytes=100000 ;;
    onnxruntime_providers_cuda.dll) max_file_bytes=110000000 ;;
    onnxruntime_providers_shared.dll) max_file_bytes=100000 ;;
    vox-golem.exe) max_file_bytes=50000000 ;;
  esac
  if [ "$file_bytes" -gt "$max_file_bytes" ]; then
    printf 'Windows package entry exceeds its size limit: %s (%s > %s bytes).\n' \
      "$file" "$file_bytes" "$max_file_bytes" >&2
    exit 1
  fi
  total_bytes=$((total_bytes + file_bytes))
  if [ "$total_bytes" -gt "$max_total_bytes" ]; then
    printf 'Windows package exceeds its extracted size limit: %s > %s bytes.\n' \
      "$total_bytes" "$max_total_bytes" >&2
    exit 1
  fi
  printf '%s\t%s bytes\n' "$file" "$file_bytes"

  pe_headers="$("$objdump" -p "$path")" || {
    printf 'Failed to inspect PE imports: %s\n' "$file" >&2
    exit 1
  }
  while IFS= read -r line; do
    [[ "$line" == *'DLL Name: '* ]] || continue
    dependency="${line#*DLL Name: }"
    dependency="${dependency,,}"
    case "$dependency" in
      directml.dll|msvcp140.dll|msvcp140_1.dll|onnxruntime_providers_cuda.dll|onnxruntime_providers_shared.dll|vcruntime140.dll|vcruntime140_1.dll)
        ;;
      cublaslt64_12.dll|cublas64_12.dll|cufft64_11.dll|cudart64_12.dll|cudnn64_9.dll)
        # These approved GPU prerequisites remain external system capabilities.
        ;;
      api-ms-win-*.dll|advapi32.dll|bcryptprimitives.dll|comctl32.dll|crypt32.dll|d3d12.dll|dbghelp.dll|dwmapi.dll|dxgi.dll|gdi32.dll|kernel32.dll|ntdll.dll|ole32.dll|oleaut32.dll|setupapi.dll|shell32.dll|shlwapi.dll|ucrtbase.dll|user32.dll|ws2_32.dll)
        ;;
      *)
        printf 'Unplanned Windows runtime dependency in %s: %s\n' "$file" "$dependency" >&2
        exit 1
        ;;
    esac
  done <<< "$pe_headers"
done
printf 'Windows package total: %s bytes\n' "$total_bytes"
