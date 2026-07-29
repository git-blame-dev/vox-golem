#!/usr/bin/env bash
set -eu -o pipefail

makensis="${VOXGOLEM_MAKENSIS:-makensis}"
if ! command -v "$makensis" >/dev/null 2>&1; then
  printf '%s\n' 'Missing makensis. Install the pinned NSIS 3.09 toolchain.' >&2
  exit 1
fi

version="$($makensis -VERSION)"
if [ "$version" != 'v3.09-4' ]; then
  printf 'Unsupported makensis version: expected v3.09-4, found %s.\n' "$version" >&2
  exit 1
fi
