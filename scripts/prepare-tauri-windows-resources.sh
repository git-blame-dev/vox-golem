#!/usr/bin/env bash
set -eu -o pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
staging_dir="$root/target/tauri-windows-resources"
mkdir -p "$staging_dir"

vc_dir="${VOXGOLEM_VC_RUNTIME_DIR:-$root/.deps/vc-runtime/14.51.36247.0/bin}"
for file in MSVCP140.dll MSVCP140_1.dll VCRUNTIME140.dll VCRUNTIME140_1.dll; do
  test -f "$vc_dir/$file" || {
    printf 'Missing Windows VC runtime resource: %s\n' "$vc_dir/$file" >&2
    exit 1
  }
  install -m 0644 "$vc_dir/$file" "$staging_dir/$file"
done

if [ "${1:-}" = "--prepare" ]; then
  exit 0
fi
release_dir="${VOXGOLEM_WINDOWS_RELEASE_DIR:-$root/target/x86_64-pc-windows-msvc/release}"
for file in DirectML.dll onnxruntime_providers_cuda.dll onnxruntime_providers_shared.dll; do
  test -f "$release_dir/$file" || {
    printf 'Missing compiled Windows ONNX resource: %s\n' "$release_dir/$file" >&2
    exit 1
  }
  install -m 0644 "$release_dir/$file" "$staging_dir/$file"
done
