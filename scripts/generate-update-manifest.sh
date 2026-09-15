#!/usr/bin/env bash
set -euo pipefail

# Generate the generic updater document from already-final payload bytes.  This
# script deliberately does not sign anything; signing belongs to the release
# finalizer and each payload is signed exactly once there.
if (($# != 7)); then
  printf 'Usage: %s VERSION TAG LINUX_ASSET LINUX_SIGNATURE WINDOWS_ASSET WINDOWS_SIGNATURE OUTPUT\n' "$0" >&2
  exit 2
fi
: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"
version="${1#v}"; tag="$2"; linux_asset="$3"; linux_sig_file="$4"; windows_asset="$5"; windows_sig_file="$6"; output="$7"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9]+)?$ ]] || { printf 'Invalid version\n' >&2; exit 2; }
[[ "$tag" == "v$version" && "$linux_asset" == "vox-golem-linux-x86_64-${tag}.AppImage" && "$windows_asset" == "vox-golem-windows-x86_64-${tag}-setup.exe" ]] || { printf 'Invalid release identity\n' >&2; exit 2; }
test -s "$linux_sig_file" && test -s "$windows_sig_file"
linux_url="https://github.com/${GITHUB_REPOSITORY}/releases/download/${tag}/${linux_asset}"
windows_url="https://github.com/${GITHUB_REPOSITORY}/releases/download/${tag}/${windows_asset}"
jq -n --arg version "$version" --arg ls "$(<"$linux_sig_file")" --arg lu "$linux_url" --arg ws "$(<"$windows_sig_file")" --arg wu "$windows_url" \
  '{version:$version,platforms:{"linux-x86_64":{signature:$ls,url:$lu},"windows-x86_64":{signature:$ws,url:$wu}}}' >"$output"
