#!/usr/bin/env bash
set -eu -o pipefail

if [ "$#" -ne 7 ]; then
  printf 'Usage: %s VERSION TAG LINUX_ASSET LINUX_SIGNATURE WINDOWS_ASSET WINDOWS_SIGNATURE OUTPUT\n' "$0" >&2
  exit 2
fi
: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"

version="${1#v}"
tag="$2"
linux_asset="$3"
linux_signature_file="$4"
windows_asset="$5"
windows_signature_file="$6"
output="$7"

if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9]+)?$ ]]; then
  printf 'Invalid updater semantic version: %s\n' "$version" >&2
  exit 2
fi
if [[ ! "$GITHUB_REPOSITORY" =~ ^[0-9A-Za-z._-]+/[0-9A-Za-z._-]+$ ]] || \
  [[ "$tag" != "v$version" ]] || \
  [[ "$linux_asset" != "vox-golem-linux-x86_64-${tag}.AppImage" ]] || \
  [[ "$windows_asset" != "vox-golem-windows-x86_64-${tag}-setup.exe" ]]; then
  printf '%s\n' 'Release repository, tag, or updater asset identity is invalid.' >&2
  exit 2
fi
test -s "$linux_signature_file" || { printf 'Missing updater signature: %s\n' "$linux_signature_file" >&2; exit 1; }
test -s "$windows_signature_file" || { printf 'Missing updater signature: %s\n' "$windows_signature_file" >&2; exit 1; }

linux_signature="$(<"$linux_signature_file")"
windows_signature="$(<"$windows_signature_file")"
linux_url="https://github.com/${GITHUB_REPOSITORY}/releases/download/${tag}/${linux_asset}"
windows_url="https://github.com/${GITHUB_REPOSITORY}/releases/download/${tag}/${windows_asset}"
jq -n \
  --arg version "$version" \
  --arg linux_signature "$linux_signature" \
  --arg linux_url "$linux_url" \
  --arg windows_signature "$windows_signature" \
  --arg windows_url "$windows_url" \
  '{version: $version, platforms: {
    "linux-x86_64": {signature: $linux_signature, url: $linux_url},
    "windows-x86_64": {signature: $windows_signature, url: $windows_url}
  }}' > "$output"
