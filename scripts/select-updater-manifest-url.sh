#!/usr/bin/env bash
set -eu -o pipefail

if [ "$#" -ne 1 ]; then
  printf 'Usage: %s OWNER/REPOSITORY\n' "$0" >&2
  exit 2
fi

repository="$1"

gh api --paginate "repos/${repository}/releases?per_page=100" --jq '.[]' | jq -sr '
  [
    .[]
    | select(.draft == false and .prerelease == false)
    | .assets[]?
    | select(.name == "latest.json")
    | .url
    | strings
  ][0] // empty
'
