#!/usr/bin/env bash
set -eu -o pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
temp_dir="$(mktemp -d)"
trap 'rm -rf "$temp_dir"' EXIT

jq -e '.bundle.linux.appimage.bundleMediaFramework == true' \
  "$root/apps/desktop-tauri/src-tauri/tauri.conf.json" >/dev/null

version="$("$root/scripts/release-version.sh" 0 42)"
test "$version" = '1970.1.1-42'

"$root/scripts/assert-release-version-newer.sh" '2026.7.27-43' '2026.7.27-42'
"$root/scripts/assert-release-version-newer.sh" '2026.7.28-0' '2026.7.27-9999'
if "$root/scripts/assert-release-version-newer.sh" '2026.7.27-42' '2026.7.27-42'; then
  printf '%s\n' 'Equal application version unexpectedly passed monotonicity validation.' >&2
  exit 1
fi
if "$root/scripts/assert-release-version-newer.sh" '2026.7.26-9999' '2026.7.27-1'; then
  printf '%s\n' 'Backward commit-date version unexpectedly passed monotonicity validation.' >&2
  exit 1
fi

commit_timestamp="$(cd "$root" && git show -s --format=%ct HEAD)"
expected_build_version="$(
  "$root/scripts/release-version.sh" "$commit_timestamp" "${GITHUB_RUN_NUMBER:-0}"
)"
actual_build_version="$(make -s -C "$root" app-version APP_VERSION='arbitrary-version')"
test "$actual_build_version" = "$expected_build_version"

printf '%s\n' 'trusted signature' > "$temp_dir/update.sig"
GITHUB_REPOSITORY='git-blame-dev/vox-golem' "$root/scripts/generate-update-manifest.sh" \
  '2026.7.27-42' 'v2026.7.27-42' 'vox-golem-linux-x86_64-v2026.7.27-42.AppImage' \
  "$temp_dir/update.sig" "$temp_dir/latest.json"

jq -e '
  .version == "2026.7.27-42" and
  .platforms["linux-x86_64"].signature == "trusted signature" and
  .platforms["linux-x86_64"].url == "https://github.com/git-blame-dev/vox-golem/releases/download/v2026.7.27-42/vox-golem-linux-x86_64-v2026.7.27-42.AppImage"
' "$temp_dir/latest.json" >/dev/null

mkdir -p "$temp_dir/AppDir/apprun-hooks" "$temp_dir/AppDir/usr/lib" "$temp_dir/providers"
printf '%s\n' '#!/bin/sh' 'export GTK_THEME=Adwaita' 'export GDK_BACKEND=x11 # forced fallback' > \
  "$temp_dir/AppDir/apprun-hooks/linuxdeploy-plugin-gtk.sh"
printf '%s\n' 'shared provider' > "$temp_dir/providers/libonnxruntime_providers_shared.so"
printf '%s\n' 'cuda provider' > "$temp_dir/providers/libonnxruntime_providers_cuda.so"
"$root/scripts/prepare-update-appdir.sh" "$temp_dir/AppDir" "$temp_dir/providers"
if grep -Fq 'GDK_BACKEND=x11' "$temp_dir/AppDir/apprun-hooks/linuxdeploy-plugin-gtk.sh"; then
  printf '%s\n' 'X11 override survived AppDir preparation.' >&2
  exit 1
fi
test -x "$temp_dir/AppDir/usr/lib/libonnxruntime_providers_shared.so"
test -x "$temp_dir/AppDir/usr/lib/libonnxruntime_providers_cuda.so"

mkdir -p "$temp_dir/bin" "$temp_dir/plugin-cache"
# The generated stub must retain these variables for its own runtime.
# shellcheck disable=SC2016
printf '%s\n' \
  '#!/usr/bin/env bash' \
  'set -eu' \
  'if [ -n "${FAKE_CURL_ARGS:-}" ]; then printf "%s\n" "$@" > "$FAKE_CURL_ARGS"; fi' \
  'while [ "$#" -gt 0 ]; do' \
  '  if [ "$1" = "--output" ]; then output="$2"; shift 2; else shift; fi' \
  'done' \
  'cp "$FAKE_CURL_SOURCE" "$output"' > "$temp_dir/bin/curl"
chmod 0755 "$temp_dir/bin/curl"

marker="$temp_dir/plugin-executed"
plugin="$temp_dir/plugin-cache/linuxdeploy-plugin-appimage.AppImage"
printf '#!/usr/bin/env bash\nprintf %s executed > %s\n' "'%s'" "$marker" > "$plugin"
chmod 0755 "$plugin"
printf '%s\n' 'wrong plugin download' > "$temp_dir/wrong-plugin"
if PATH="$temp_dir/bin:$PATH" FAKE_CURL_SOURCE="$temp_dir/wrong-plugin" \
  "$root/scripts/prepare-tauri-bundler-tool.sh" "$plugin" 'https://example.invalid/plugin' \
  '0000000000000000000000000000000000000000000000000000000000000000'; then
  printf '%s\n' 'Invalid AppImage plugin unexpectedly passed verification.' >&2
  exit 1
fi
test ! -e "$plugin"
test ! -e "$marker"

printf '%s\n' 'verified plugin' > "$temp_dir/verified-plugin"
verified_sha256="$(sha256sum "$temp_dir/verified-plugin" | cut -d ' ' -f 1)"
PATH="$temp_dir/bin:$PATH" FAKE_CURL_SOURCE="$temp_dir/verified-plugin" \
  FAKE_CURL_ARGS="$temp_dir/curl-args" GITHUB_TOKEN='test-ci-token' \
  "$root/scripts/prepare-tauri-bundler-tool.sh" "$plugin" \
  'https://api.github.com/repos/example/tools/releases/assets/1' \
  "$verified_sha256"
test -x "$plugin"
printf '%s  %s\n' "$verified_sha256" "$plugin" | sha256sum -c - >/dev/null
grep -Fx 'Authorization: Bearer test-ci-token' "$temp_dir/curl-args" >/dev/null

rm -f "$plugin"
PATH="$temp_dir/bin:$PATH" FAKE_CURL_SOURCE="$temp_dir/verified-plugin" \
  FAKE_CURL_ARGS="$temp_dir/non-api-curl-args" GITHUB_TOKEN='test-ci-token' \
  "$root/scripts/prepare-tauri-bundler-tool.sh" "$plugin" 'https://example.invalid/plugin' \
  "$verified_sha256"
if grep -F 'test-ci-token' "$temp_dir/non-api-curl-args"; then
  printf '%s\n' 'GitHub token was forwarded to a non-API URL.' >&2
  exit 1
fi

rm -f "$plugin"
PATH="$temp_dir/bin:$PATH" FAKE_CURL_SOURCE="$temp_dir/verified-plugin" \
  FAKE_CURL_ARGS="$temp_dir/unauthenticated-curl-args" GITHUB_TOKEN='' \
  "$root/scripts/prepare-tauri-bundler-tool.sh" "$plugin" \
  'https://api.github.com/repos/example/tools/releases/assets/1' \
  "$verified_sha256"
if grep -F 'Authorization:' "$temp_dir/unauthenticated-curl-args"; then
  printf '%s\n' 'Authorization header was added without a GitHub token.' >&2
  exit 1
fi

rm -f "$plugin"
mkdir "$plugin"
printf '%s\n' 'planted directory content' > "$plugin/content"
PATH="$temp_dir/bin:$PATH" FAKE_CURL_SOURCE="$temp_dir/verified-plugin" \
  "$root/scripts/prepare-tauri-bundler-tool.sh" "$plugin" 'https://example.invalid/plugin' \
  "$verified_sha256"
test -x "$plugin"

rm -f "$plugin"
mkdir "$temp_dir/planted-directory"
ln -s "$temp_dir/planted-directory" "$plugin"
PATH="$temp_dir/bin:$PATH" FAKE_CURL_SOURCE="$temp_dir/verified-plugin" \
  "$root/scripts/prepare-tauri-bundler-tool.sh" "$plugin" 'https://example.invalid/plugin' \
  "$verified_sha256"
test -x "$plugin"
test -d "$temp_dir/planted-directory"

healthy_log="$temp_dir/healthy-appimage.log"
printf '%s\n' 'VOXGOLEM_STARTUP_READY' > "$healthy_log"
sleep 3 & healthy_pid=$!
"$root/scripts/wait-for-appimage-startup.sh" "$healthy_pid" "$healthy_log" 2 1
wait "$healthy_pid"

delayed_warning_log="$temp_dir/delayed-warning-appimage.log"
(
  printf '%s\n' 'VOXGOLEM_STARTUP_READY'
  sleep 1
  printf '%s\n' 'GStreamer element appsink not found. Please install it.'
  sleep 2
) > "$delayed_warning_log" & delayed_warning_pid=$!
if "$root/scripts/wait-for-appimage-startup.sh" \
  "$delayed_warning_pid" "$delayed_warning_log" 2 2; then
  printf '%s\n' 'Delayed GStreamer warning unexpectedly passed AppImage smoke.' >&2
  exit 1
fi
wait "$delayed_warning_pid"
