#!/usr/bin/env bash
set -eu -o pipefail

if [ "$#" -ne 2 ]; then
  printf 'Usage: %s CANDIDATE_VERSION PREVIOUS_VERSION\n' "$0" >&2
  exit 2
fi

candidate="${1#v}"
previous="${2#v}"
version_pattern='^([0-9]+)\.([0-9]+)\.([0-9]+)-([0-9]+)$'

if [[ ! "$candidate" =~ $version_pattern ]]; then
  printf 'Invalid candidate application version: %s\n' "$candidate" >&2
  exit 2
fi
candidate_parts=("${BASH_REMATCH[@]:1}")

if [[ ! "$previous" =~ $version_pattern ]]; then
  printf 'Invalid previous application version: %s\n' "$previous" >&2
  exit 2
fi
previous_parts=("${BASH_REMATCH[@]:1}")

for index in 0 1 2 3; do
  candidate_part=$((10#${candidate_parts[$index]}))
  previous_part=$((10#${previous_parts[$index]}))
  if ((candidate_part > previous_part)); then
    exit 0
  fi
  if ((candidate_part < previous_part)); then
    printf 'Application version %s must be newer than published version %s.\n' \
      "$candidate" "$previous" >&2
    exit 1
  fi
done

printf 'Application version %s must be newer than published version %s.\n' \
  "$candidate" "$previous" >&2
exit 1
