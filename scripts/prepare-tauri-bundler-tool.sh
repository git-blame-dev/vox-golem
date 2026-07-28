#!/usr/bin/env bash
set -eu -o pipefail

if [ "$#" -ne 3 ]; then
  printf 'Usage: %s TARGET URL SHA256\n' "$0" >&2
  exit 1
fi

target="$1"
url="$2"
expected_sha256="$3"
target_dir="$(dirname "$target")"

mkdir -p "$target_dir"
if [ -f "$target" ] && [ ! -L "$target" ] &&
  printf '%s  %s\n' "$expected_sha256" "$target" | sha256sum -c - >/dev/null 2>&1; then
  chmod 0755 "$target"
  exit 0
fi
if [ -e "$target" ] || [ -L "$target" ]; then
  rm -rf -- "$target"
fi

download="$(mktemp "$target_dir/.tauri-bundler-tool.XXXXXX")"
trap 'rm -f "$download"' EXIT
curl --fail --location --proto '=https' --tlsv1.2 \
  --header 'Accept: application/octet-stream' --output "$download" "$url"
printf '%s  %s\n' "$expected_sha256" "$download" | sha256sum -c -
chmod 0755 "$download"
mv "$download" "$target"
trap - EXIT
