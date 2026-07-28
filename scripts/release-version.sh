#!/usr/bin/env bash
set -eu -o pipefail

if [ "$#" -ne 2 ]; then
  printf 'Usage: %s COMMIT_UNIX_TIMESTAMP CI_RUN_NUMBER\n' "$0" >&2
  exit 2
fi

timestamp="$1"
run_number="$2"
case "$timestamp" in ''|*[!0-9]*) printf '%s\n' 'Commit timestamp must be an integer.' >&2; exit 2;; esac
case "$run_number" in ''|*[!0-9]*) printf '%s\n' 'CI run number must be an integer.' >&2; exit 2;; esac

date_part="$(date -u -d "@$timestamp" +'%Y.%-m.%-d')"
printf '%s-%s\n' "$date_part" "$run_number"
