#!/usr/bin/env bash
# Owned, synthetic startup only. Namespace failure is a smoke failure, never a
# reason to fall back to the user's network, credentials or desktop services.
set -eu -o pipefail
if [ "$#" -ne 2 ]; then
  printf 'Usage: %s native|appimage EXECUTABLE\n' "$0" >&2
  exit 2
fi
arguments=()
case "$1" in
  native) post_ready=0 ;;
  appimage) post_ready=5; arguments=(--appimage-extract-and-run) ;;
  *) exit 2 ;;
esac
executable="$(realpath -- "$2")"
root="$(cd "$(dirname "$0")/.." && pwd)"
umask 077
smoke_dir="$(mktemp -d /tmp/voxgolem-smoke.XXXXXXXX)"
smoke_pid=
cleanup() {
  status="$?"
  trap - EXIT
  # Always address the group, even if the launch process has already exited.
  if [ -n "$smoke_pid" ]; then
    kill -TERM -- "-$smoke_pid" 2>/dev/null || true
    for _ in 1 2 3 4 5; do
      kill -0 -- "-$smoke_pid" 2>/dev/null || break
      sleep 1
    done
    kill -KILL -- "-$smoke_pid" 2>/dev/null || true
    wait "$smoke_pid" 2>/dev/null || true
  fi
  if [ "$status" -ne 0 ] && [ -f "$smoke_dir/output.log" ]; then
    printf '%s\n' 'Linux smoke diagnostics:' >&2
    cat "$smoke_dir/output.log" >&2 || true
  fi
  rm -rf -- "$smoke_dir"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
mkdir "$smoke_dir"/{home,config,data,state,cache,runtime,tmp,work}
printf '%s\n' 'web_enabled = false' > "$smoke_dir/config/config.toml"
printf '%s\n' 'auto_update_download = false' 'auto_update_install = false' \
  'tts_enabled = false' 'web_enabled = false' > "$smoke_dir/config/state.toml"
# Start the test display inside the same network namespace: abstract X11 sockets
# belong to that namespace. Private bus/audio endpoints avoid host services.
(
  cd "$smoke_dir/work"
  exec setsid env -i PATH=/usr/bin:/bin LANG=C.UTF-8 \
    HOME="$smoke_dir/home" XDG_CONFIG_HOME="$smoke_dir/config" \
    XDG_DATA_HOME="$smoke_dir/data" XDG_STATE_HOME="$smoke_dir/state" \
    XDG_CACHE_HOME="$smoke_dir/cache" XDG_RUNTIME_DIR="$smoke_dir/runtime" \
    TMPDIR="$smoke_dir/tmp" \
    DBUS_SESSION_BUS_ADDRESS="unix:path=$smoke_dir/runtime/no-bus" \
    DBUS_SYSTEM_BUS_ADDRESS="unix:path=$smoke_dir/runtime/no-system-bus" \
    PULSE_SERVER="unix:$smoke_dir/runtime/no-pulse" \
    VOXGOLEM_CONFIG_PATH="$smoke_dir/config/config.toml" \
    VOXGOLEM_STARTUP_DIAGNOSTICS=1 \
    unshare --user --map-root-user --net -- xvfb-run -a "$executable" "${arguments[@]}"
) > "$smoke_dir/output.log" 2>&1 & smoke_pid=$!
bash "$root/scripts/wait-for-appimage-startup.sh" "$smoke_pid" "$smoke_dir/output.log" 30 "$post_ready"
