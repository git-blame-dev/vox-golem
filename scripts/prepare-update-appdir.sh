#!/usr/bin/env bash
set -eu -o pipefail

if [ "$#" -ne 2 ]; then
  printf 'Usage: %s APPDIR PROVIDER_DIRECTORY\n' "$0" >&2
  exit 2
fi

appdir="$1"
provider_dir="$2"
hook="$appdir/apprun-hooks/linuxdeploy-plugin-gtk.sh"
test -d "$appdir/usr/lib" || { printf 'Missing AppImage library directory: %s\n' "$appdir/usr/lib" >&2; exit 1; }
test -f "$hook" || { printf 'Missing GTK AppRun hook: %s\n' "$hook" >&2; exit 1; }

temporary_hook="${hook}.tmp"
trap 'rm -f "$temporary_hook"' EXIT
while IFS= read -r line || [ -n "$line" ]; do
  case "$line" in
    *'export GDK_BACKEND=x11'*) continue;;
  esac
  printf '%s\n' "$line" >> "$temporary_hook"
done < "$hook"
chmod --reference="$hook" "$temporary_hook"
mv "$temporary_hook" "$hook"

for provider in libonnxruntime_providers_shared.so libonnxruntime_providers_cuda.so; do
  source="$provider_dir/$provider"
  test -f "$source" || { printf 'Missing ONNX Runtime provider: %s\n' "$source" >&2; exit 1; }
  install -m 0755 "$source" "$appdir/usr/lib/$provider"
done
