#!/usr/bin/env bash
set -eu -o pipefail

if [ "$#" -ne 1 ]; then
  printf 'Usage: %s OWNER/REPOSITORY\n' "$0" >&2
  exit 2
fi

repository="$1"

selection="$(gh api --paginate "repos/${repository}/releases?per_page=100" --jq '.[]' | jq -src '
  [ .[]
    | select(.draft == false and .prerelease == false)
    | select(.tag_name | type == "string" and test("^v[0-9]+\\.[0-9]+\\.[0-9]+-[0-9]+$"))
    | . as $release
    | ($release.tag_name[1:] | split("-") as $base
       | ($base[0] | split(".") | map(tonumber)) + [($base[1] | tonumber)]) as $version
    | ($release.assets[]? | select(.name == "latest.json" and (.url | type == "string"))
       | {version: $version, url: .url, tag: $release.tag_name})
  ] | sort_by(.version) | last // empty
')"
if [ -z "$selection" ]; then
  if [ -n "${SELECTED_UPDATER_VERSION_FILE:-}" ]; then
    : > "$SELECTED_UPDATER_VERSION_FILE"
  fi
  exit 0
fi
if [ -n "${SELECTED_UPDATER_VERSION_FILE:-}" ]; then
  jq -r '.tag[1:]' <<< "$selection" > "$SELECTED_UPDATER_VERSION_FILE"
fi
jq -r '.url' <<< "$selection"
