#!/usr/bin/env bash
set -eu -o pipefail

if [ "$#" -ne 5 ]; then
  printf 'Usage: %s VERSION TAG ASSET SIGNATURE_FILE OUTPUT\n' "$0" >&2
  exit 2
fi
: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"

version="${1#v}"
tag="$2"
asset="$3"
signature_file="$4"
output="$5"

if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9]+)?$ ]]; then
  printf 'Invalid updater semantic version: %s\n' "$version" >&2
  exit 2
fi
if [[ ! "$tag" =~ ^v[0-9A-Za-z._-]+$ ]] || [[ ! "$asset" =~ ^[0-9A-Za-z._-]+$ ]]; then
  printf '%s\n' 'Unsafe release tag or asset name.' >&2
  exit 2
fi
test -s "$signature_file" || { printf 'Missing updater signature: %s\n' "$signature_file" >&2; exit 1; }

signature="$(<"$signature_file")"
url="https://github.com/${GITHUB_REPOSITORY}/releases/download/${tag}/${asset}"
jq -n \
  --arg version "$version" \
  --arg signature "$signature" \
  --arg url "$url" \
  '{version: $version, platforms: {"linux-x86_64": {signature: $signature, url: $url}}}' > "$output"
