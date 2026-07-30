#!/usr/bin/env bash
set -euo pipefail

readonly PACKAGE='dev.gitblame.voxgolem.android'
readonly MAX_AUTH_BYTES=65536
# `adb shell -T` preserves raw stdin without allocating a terminal. Platform
# Tools 37 `exec-out` produced an empty file on the target Pixel 6.
# shellcheck disable=SC2016
readonly REMOTE_WRITABLE='umask 077 && if test ! -e files; then mkdir -m 700 files; fi && test -d files && test ! -L files && test -w files'
# shellcheck disable=SC2016
readonly REMOTE_INSTALL='cd files && expected=$1 && umask 077 && tmp=.codex-auth.json.tmp.$$ && trap "rm -f \"$tmp\"" EXIT HUP INT TERM && cat > "$tmp" && test "$(stat -c %s "$tmp")" = "$expected" && chmod 600 "$tmp" && mv -f "$tmp" codex-auth.json && trap - EXIT HUP INT TERM'
# shellcheck disable=SC2016
readonly REMOTE_VERIFY='cd files && test -f codex-auth.json && test ! -L codex-auth.json && test "$(stat -c %a codex-auth.json)" = 600 && test "$(stat -c %u codex-auth.json)" = "$(id -u)"'

usage() {
  printf 'Usage: %s --device <adb-serial> --auth <auth.json>\n' "$0" >&2
}

device=''
auth_path=''
while (($# > 0)); do
  case "$1" in
    --device)
      (($# >= 2)) || { usage; exit 2; }
      device="$2"
      shift 2
      ;;
    --auth)
      (($# >= 2)) || { usage; exit 2; }
      auth_path="$2"
      shift 2
      ;;
    *)
      usage
      exit 2
      ;;
  esac
done

[[ -n "$device" && -n "$auth_path" ]] || { usage; exit 2; }
[[ -f "$auth_path" && ! -L "$auth_path" && -r "$auth_path" ]] || {
  printf '%s\n' 'Auth source must be a readable, non-symlink regular file.' >&2
  exit 1
}

source_snapshot="$(mktemp "${TMPDIR:-/tmp}/voxgolem-android-auth-source.XXXXXX")"
auth_snapshot="$(mktemp "${TMPDIR:-/tmp}/voxgolem-android-auth.XXXXXX")"
trap 'rm -f "$source_snapshot" "$auth_snapshot"' EXIT
chmod 600 "$source_snapshot"
chmod 600 "$auth_snapshot"
dd if="$auth_path" of="$source_snapshot" bs=$((MAX_AUTH_BYTES + 1)) count=1 status=none

source_size="$(stat -c '%s' -- "$source_snapshot")"
((source_size > 0 && source_size <= MAX_AUTH_BYTES)) || {
  printf 'Auth source must contain between 1 and %s bytes.\n' "$MAX_AUTH_BYTES" >&2
  exit 1
}

jq --slurp -e 'length == 1' "$source_snapshot" >/dev/null || {
  printf '%s\n' 'Auth source must contain exactly one JSON record.' >&2
  exit 1
}

jq --stream --slurp -e '
  . as $records |
  [$records[] | select(length == 2)] as $entries |
  (all($entries[]; (.[0][0] != "openai") or (.[0] | length <= 2))) and
  ([$entries[] | select(.[0] == ["openai"])] | length == 0) and
  ([$records[] | select(length == 1 and .[0][0] == "openai" and (.[0] | length == 2))] | length == 1) and
  ([$entries[] | select(.[0] == ["openai", "access"])] | length == 1) and
  ([$entries[] | select(.[0] == ["openai", "accountId"])] | length <= 1) and
  ([$entries[] | select(.[0] == ["openai", "expires"])] | length == 1)
' "$source_snapshot" >/dev/null || {
  printf '%s\n' 'Auth source contains duplicate or missing credential fields.' >&2
  exit 1
}

jq -e '
  (.openai | type == "object") and
  (.openai.access | type == "string" and length > 0 and
    (explode | all(. >= 32 and . <= 126))) and
  ((.openai | has("accountId") | not) or
    (.openai.accountId | type == "string" and length > 0 and
      (explode | all(. >= 32 and . <= 126)))) and
  (.openai.expires | type == "number" and . > 0 and floor == . and
    . <= 9007199254740991)
' "$source_snapshot" >/dev/null || {
  printf '%s\n' 'Auth source is not a compatible OpenCode auth record.' >&2
  exit 1
}

jq -c '{openai: (.openai | {access, expires} + (if has("accountId") then {accountId} else {} end))}' \
  "$source_snapshot" >"$auth_snapshot"
auth_path="$auth_snapshot"
auth_size="$(stat -c '%s' -- "$auth_path")"

adb_bin="${ADB:-adb}"
command -v "$adb_bin" >/dev/null || {
  printf 'ADB executable not found: %s\n' "$adb_bin" >&2
  exit 1
}
adb_command=("$adb_bin" -s "$device")

device_state="$("${adb_command[@]}" get-state 2>/dev/null || true)"
[[ "$device_state" == 'device' ]] || {
  printf 'ADB device is not connected and authorized: %s\n' "$device" >&2
  exit 1
}

"${adb_command[@]}" shell pm path "$PACKAGE" >/dev/null 2>&1 || {
  printf 'Debug package is not installed on %s.\n' "$device" >&2
  exit 1
}

"${adb_command[@]}" shell run-as "$PACKAGE" sh -c "'$REMOTE_WRITABLE'" >/dev/null 2>&1 || {
  printf '%s\n' 'Installed package does not permit app-private run-as provisioning.' >&2
  exit 1
}

"${adb_command[@]}" shell -T \
  "run-as $PACKAGE sh -c '$REMOTE_INSTALL' sh $auth_size" \
  <"$auth_path"

"${adb_command[@]}" shell run-as "$PACKAGE" sh -c \
  "'$REMOTE_VERIFY'" \
  >/dev/null
remote_size="$("${adb_command[@]}" shell run-as "$PACKAGE" stat -c %s files/codex-auth.json)"
remote_size="${remote_size//$'\r'/}"
[[ "$remote_size" == "$auth_size" ]] || {
  printf '%s\n' 'App-private auth transfer length did not match the source.' >&2
  exit 1
}

printf 'Provisioned app-private auth on %s.\n' "$device"
