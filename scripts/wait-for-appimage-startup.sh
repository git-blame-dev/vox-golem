#!/usr/bin/env bash
set -eu -o pipefail

if [ "$#" -ne 4 ]; then
  printf 'Usage: %s PID LOG READY_TIMEOUT_SECONDS POST_READY_SECONDS\n' "$0" >&2
  exit 1
fi

pid="$1"
log="$2"
ready_timeout="$3"
post_ready="$4"
warning='GStreamer element appsink not found'

for ((second = 0; second < ready_timeout; second += 1)); do
  sleep 1
  if grep -Fq "$warning" "$log"; then
    printf '%s\n' 'Linux AppImage smoke failed: GStreamer appsink is unavailable.' >&2
    exit 1
  fi
  if grep -Fq 'VOXGOLEM_STARTUP_READY' "$log"; then
    for ((observed = 0; observed < post_ready; observed += 1)); do
      sleep 1
      if grep -Fq "$warning" "$log"; then
        printf '%s\n' 'Linux AppImage smoke failed: GStreamer appsink is unavailable.' >&2
        exit 1
      fi
      if ! kill -0 "$pid" 2>/dev/null; then
        printf '%s\n' 'Linux AppImage smoke failed after startup readiness.' >&2
        exit 1
      fi
    done
    if grep -Fq "$warning" "$log"; then
      printf '%s\n' 'Linux AppImage smoke failed: GStreamer appsink is unavailable.' >&2
      exit 1
    fi
    printf '%s\n' 'Linux AppImage smoke passed: startup-ready marker remained healthy.'
    exit 0
  fi
  if ! kill -0 "$pid" 2>/dev/null; then
    printf '%s\n' 'Linux AppImage smoke failed.' >&2
    exit 1
  fi
done

printf '%s\n' 'Linux AppImage smoke timed out.' >&2
exit 1
