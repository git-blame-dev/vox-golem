#!/usr/bin/env bash
set -eu -o pipefail

if [ "$#" -ne 5 ]; then
  printf 'Usage: %s APPIMAGE LINUX_PACKAGE_DIR WINDOWS_INSTALLER VERSION TAG\n' "$0" >&2
  exit 2
fi
appimage="$1"
linux_package_dir="$2"
windows_installer="$3"
version="${4#v}"
tag="$5"
root="$(cd "$(dirname "$0")/.." && pwd)"
signer="${TAURI_SIGNER:-cargo-tauri}"

: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"
: "${TAURI_SIGNING_PRIVATE_KEY:?TAURI_SIGNING_PRIVATE_KEY is required}"
: "${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:=}"
test "$tag" = "v$version" || { printf '%s\n' 'Release tag does not match version.' >&2; exit 2; }
output_dir="$root/dist-final"
if [ -L "$output_dir" ] || {
  [ -e "$output_dir" ] && [ ! -d "$output_dir" ]
}; then
  printf 'Refusing unsafe output directory: %s\n' "$output_dir" >&2
  exit 1
fi
for path in "$appimage" "$windows_installer"; do
  test -f "$path" && test ! -L "$path" || { printf 'Unsafe or missing payload: %s\n' "$path" >&2; exit 1; }
done
test -d "$linux_package_dir" && test ! -L "$linux_package_dir" || {
  printf 'Unsafe or missing Linux package directory: %s\n' "$linux_package_dir" >&2
  exit 1
}

rm -rf -- "$output_dir"
mkdir -p "$output_dir"
output_dir="$(realpath "$output_dir")"
linux_package_dir="$(realpath "$linux_package_dir")"
linux_zip="vox-golem-linux-${tag}.zip"
linux_asset="vox-golem-linux-x86_64-${tag}.AppImage"
windows_asset="vox-golem-windows-x86_64-${tag}-setup.exe"
cp -- "$appimage" "$output_dir/$linux_asset"
cp -- "$windows_installer" "$output_dir/$windows_asset"
(cd "$(dirname "$linux_package_dir")" && zip -q -r -D "$output_dir/$linux_zip" "$(basename "$linux_package_dir")")

for asset in "$linux_asset" "$windows_asset"; do
  "$signer" signer sign --password "$TAURI_SIGNING_PRIVATE_KEY_PASSWORD" "$output_dir/$asset"
done

GITHUB_REPOSITORY="$GITHUB_REPOSITORY" "$root/scripts/generate-update-manifest.sh" \
  "$version" "$tag" "$linux_asset" "$output_dir/$linux_asset.sig" \
  "$windows_asset" "$output_dir/$windows_asset.sig" "$output_dir/latest.json"
(
  cd "$output_dir"
  sha256sum "$linux_zip" "$linux_asset" "$linux_asset.sig" \
    "$windows_asset" "$windows_asset.sig" latest.json > SHA256SUMS
)
"$root/scripts/verify-release-assets.sh" "$output_dir" "$tag"
