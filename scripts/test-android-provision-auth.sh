#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
temp_dir="$(mktemp -d)"
trap 'rm -rf "$temp_dir"' EXIT
mkdir -p "$temp_dir/bin"

canonical_auth() {
  jq -c '{openai: (.openai | {access, expires} + (if has("accountId") then {accountId} else {} end))}' "$1"
}

cat >"$temp_dir/bin/adb" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

case " $* " in
  *' get-state '*)
    if [[ -n "${FAKE_SOURCE_PATH:-}" ]]; then
      mv -f "$FAKE_SOURCE_REPLACEMENT" "$FAKE_SOURCE_PATH"
    fi
    printf '%s\n' 'device'
    ;;
  *' shell -T '*)
    command="$*"
    expected="${command##* }"
    tmp="$FAKE_DEVICE_FILE.tmp.$$"
    trap 'rm -f "$tmp"' EXIT HUP INT TERM
    cat >"$tmp"
    if [[ "${FAKE_TRUNCATE_TRANSFER:-}" == 1 ]]; then
      truncate -s "$((expected - 1))" "$tmp"
    fi
    if [[ "$(stat -c '%s' "$tmp")" != "$expected" ]]; then
      exit 1
    fi
    if [[ "${FAKE_DELAY_TRANSFER:-}" == 1 ]]; then sleep 0.1; fi
    mv -f "$tmp" "$FAKE_DEVICE_FILE"
    trap - EXIT HUP INT TERM
    ;;
  *' stat -c %s files/codex-auth.json '*)
    size="$(stat -c '%s' "$FAKE_DEVICE_FILE")"
    printf '%s\r\n' "$((size + ${FAKE_SIZE_OFFSET:-0}))"
    ;;
  *)
    ;;
esac
EOF
chmod 0755 "$temp_dir/bin/adb"

auth="$temp_dir/auth.json"
printf '%s\n' '{"openai":{"access":"test-secret","accountId":"account","expires":4102444800}}' >"$auth"
output="$(
  ADB="$temp_dir/bin/adb" FAKE_DEVICE_FILE="$temp_dir/device-auth.json" \
    "$root/scripts/android-provision-auth.sh" --device 'device:1234' --auth "$auth"
)"
canonical_auth "$auth" >"$temp_dir/expected-auth.json"
cmp "$temp_dir/expected-auth.json" "$temp_dir/device-auth.json"
test "$output" = 'Provisioned app-private auth on device:1234.'
case "$output" in
  *'test-secret'*)
    printf '%s\n' 'Provisioning output exposed credential content.' >&2
    exit 1
    ;;
esac

canonical_auth "$auth" >"$temp_dir/validated-auth.json"
printf '%s\n' '{"openai":{"access":"replacement","accountId":"account","expires":4102444800}}' >"$temp_dir/replacement.json"
ADB="$temp_dir/bin/adb" FAKE_DEVICE_FILE="$temp_dir/device-auth.json" \
  FAKE_SOURCE_PATH="$auth" FAKE_SOURCE_REPLACEMENT="$temp_dir/replacement.json" \
  "$root/scripts/android-provision-auth.sh" --device 'device:1234' --auth "$auth" >/dev/null
cmp "$temp_dir/validated-auth.json" "$temp_dir/device-auth.json"

printf '%s\n' 'prior credential' >"$temp_dir/device-auth.json"
if ADB="$temp_dir/bin/adb" FAKE_DEVICE_FILE="$temp_dir/device-auth.json" \
  FAKE_TRUNCATE_TRANSFER=1 "$root/scripts/android-provision-auth.sh" \
  --device 'device:1234' --auth "$auth" >/dev/null 2>&1; then
  printf '%s\n' 'Mismatched transfer length unexpectedly passed verification.' >&2
  exit 1
fi
test "$(cat "$temp_dir/device-auth.json")" = 'prior credential'
if compgen -G "$temp_dir/device-auth.json.tmp.*" >/dev/null; then
  printf '%s\n' 'Interrupted transfer left a temporary credential file.' >&2
  exit 1
fi

printf '%s\n' '{"openai":{"access":""}}' >"$temp_dir/invalid.json"
if ADB="$temp_dir/bin/adb" FAKE_DEVICE_FILE="$temp_dir/device-auth.json" \
  "$root/scripts/android-provision-auth.sh" --device 'device:1234' \
  --auth "$temp_dir/invalid.json" >/dev/null 2>&1; then
  printf '%s\n' 'Invalid auth record unexpectedly passed validation.' >&2
  exit 1
fi

for invalid in \
  '{"openai":{"access":"test-secret","accountId":"account","expires":1.5}}' \
  '{"openai":{"access":"test-secret","accountId":"account","expires":9007199254740992}}' \
  '{"openai":{"access":"bad\nvalue","accountId":"account","expires":4102444800}}' \
  '{"openai":{"access":"test-secret","accountId":"bad\rvalue","expires":4102444800}}'
do
  printf '%s\n' "$invalid" >"$temp_dir/invalid.json"
  if ADB="$temp_dir/bin/adb" FAKE_DEVICE_FILE="$temp_dir/device-auth.json" \
    "$root/scripts/android-provision-auth.sh" --device 'device:1234' \
    --auth "$temp_dir/invalid.json" >/dev/null 2>&1; then
    printf '%s\n' 'Runtime-incompatible auth record unexpectedly passed validation.' >&2
    exit 1
  fi
  test "$(cat "$temp_dir/device-auth.json")" = 'prior credential'
done

for compatible_expiry in '4102444800.0' '4.1024448e9'; do
  printf '{"openai":{"access":"test-secret","accountId":"account","expires":%s}}\n' \
    "$compatible_expiry" >"$temp_dir/compatible.json"
  ADB="$temp_dir/bin/adb" FAKE_DEVICE_FILE="$temp_dir/device-auth.json" \
    "$root/scripts/android-provision-auth.sh" --device 'device:1234' \
    --auth "$temp_dir/compatible.json" >/dev/null
  canonical_auth "$temp_dir/compatible.json" >"$temp_dir/expected-compatible.json"
  cmp "$temp_dir/expected-compatible.json" "$temp_dir/device-auth.json"
done

printf '%s\n' '{"anthropic":{"type":"api","key":"ignored"},"openai":{"type":"oauth","refresh":"refresh-secret","access":"test-secret","accountId":"account","expires":4102444800}}' >"$temp_dir/standard-opencode.json"
ADB="$temp_dir/bin/adb" FAKE_DEVICE_FILE="$temp_dir/device-auth.json" \
  "$root/scripts/android-provision-auth.sh" --device 'device:1234' \
  --auth "$temp_dir/standard-opencode.json" >/dev/null
canonical_auth "$temp_dir/standard-opencode.json" >"$temp_dir/expected-standard.json"
cmp "$temp_dir/expected-standard.json" "$temp_dir/device-auth.json"

printf '%s\n' '{"openai":{"type":"oauth","refresh":"refresh-secret","access":"test-secret","expires":4102444800}}' >"$temp_dir/standard-without-account.json"
ADB="$temp_dir/bin/adb" FAKE_DEVICE_FILE="$temp_dir/device-auth.json" \
  "$root/scripts/android-provision-auth.sh" --device 'device:1234' \
  --auth "$temp_dir/standard-without-account.json" >/dev/null
canonical_auth "$temp_dir/standard-without-account.json" >"$temp_dir/expected-without-account.json"
cmp "$temp_dir/expected-without-account.json" "$temp_dir/device-auth.json"

printf '%s\n' 'prior credential' >"$temp_dir/device-auth.json"
for records in \
  $'{"anthropic":{"type":"api"}}\n{"openai":{"access":"valid","accountId":"account","expires":4102444800}}' \
  $'{"openai":{"access":"valid","accountId":"account","expires":4102444800}}\n{"anthropic":{"type":"api"}}'
do
  printf '%s\n' "$records" >"$temp_dir/multiple-records.json"
  if TMPDIR="$temp_dir" ADB="$temp_dir/bin/adb" FAKE_DEVICE_FILE="$temp_dir/device-auth.json" \
    "$root/scripts/android-provision-auth.sh" --device 'device:1234' \
    --auth "$temp_dir/multiple-records.json" >/dev/null 2>&1; then
    printf '%s\n' 'Multiple top-level auth records unexpectedly passed validation.' >&2
    exit 1
  fi
  test "$(cat "$temp_dir/device-auth.json")" = 'prior credential'
  if compgen -G "$temp_dir/voxgolem-android-auth*" >/dev/null; then
    printf '%s\n' 'Multiple-record validation left a host snapshot.' >&2
    exit 1
  fi
done

for duplicate in \
  '{"openai":{"access":"first","access":"second","accountId":"account","expires":4102444800}}' \
  '{"openai":{"access":"secret","accountId":"account","expires":4102444800,"expires":4102444801}}' \
  '{"openai":{"access":"first","accountId":"account","expires":4102444800},"openai":{"access":"second","accountId":"account","expires":4102444800}}'
do
  printf '%s\n' "$duplicate" >"$temp_dir/duplicate.json"
  if ADB="$temp_dir/bin/adb" FAKE_DEVICE_FILE="$temp_dir/device-auth.json" \
    "$root/scripts/android-provision-auth.sh" --device 'device:1234' \
    --auth "$temp_dir/duplicate.json" >/dev/null 2>&1; then
    printf '%s\n' 'Duplicate auth fields unexpectedly passed validation.' >&2
    exit 1
  fi
  test "$(cat "$temp_dir/device-auth.json")" = 'prior credential'
done

for duplicate_container in \
  '{"openai":{"access":{"ignored":true},"access":"valid","accountId":"account","expires":4102444800}}' \
  '{"openai":{"access":"valid","accountId":{"ignored":true},"accountId":"account","expires":4102444800}}' \
  '{"openai":{"access":"valid","accountId":"account","expires":{"ignored":true},"expires":4102444800}}' \
  '{"openai":{"ignored":{"nested":true}},"openai":{"access":"valid","accountId":"account","expires":4102444800}}'
do
  printf '%s\n' "$duplicate_container" >"$temp_dir/duplicate.json"
  if ADB="$temp_dir/bin/adb" FAKE_DEVICE_FILE="$temp_dir/device-auth.json" \
    "$root/scripts/android-provision-auth.sh" --device 'device:1234' \
    --auth "$temp_dir/duplicate.json" >/dev/null 2>&1; then
    printf '%s\n' 'Container-valued duplicate auth fields unexpectedly passed validation.' >&2
    exit 1
  fi
  test "$(cat "$temp_dir/device-auth.json")" = 'prior credential'
done

dd if=/dev/zero of="$temp_dir/oversized.json" bs=65537 count=1 status=none
if TMPDIR="$temp_dir" ADB="$temp_dir/bin/adb" FAKE_DEVICE_FILE="$temp_dir/device-auth.json" \
  "$root/scripts/android-provision-auth.sh" --device 'device:1234' \
  --auth "$temp_dir/oversized.json" >/dev/null 2>&1; then
  printf '%s\n' 'Oversized auth source unexpectedly passed validation.' >&2
  exit 1
fi
test "$(cat "$temp_dir/device-auth.json")" = 'prior credential'
if compgen -G "$temp_dir/voxgolem-android-auth.*" >/dev/null; then
  printf '%s\n' 'Oversized auth validation left a host snapshot.' >&2
  exit 1
fi

printf '%s\n' '{"openai":{"access":"credential-a","accountId":"account","expires":4102444800}}' >"$temp_dir/auth-a.json"
printf '%s\n' '{"openai":{"access":"credential-b","accountId":"account","expires":4102444800}}' >"$temp_dir/auth-b.json"
canonical_auth "$temp_dir/auth-a.json" >"$temp_dir/expected-auth-a.json"
canonical_auth "$temp_dir/auth-b.json" >"$temp_dir/expected-auth-b.json"
ADB="$temp_dir/bin/adb" FAKE_DEVICE_FILE="$temp_dir/device-auth.json" FAKE_DELAY_TRANSFER=1 \
  "$root/scripts/android-provision-auth.sh" --device 'device:1234' \
  --auth "$temp_dir/auth-a.json" >/dev/null & first_pid=$!
ADB="$temp_dir/bin/adb" FAKE_DEVICE_FILE="$temp_dir/device-auth.json" FAKE_DELAY_TRANSFER=1 \
  "$root/scripts/android-provision-auth.sh" --device 'device:1234' \
  --auth "$temp_dir/auth-b.json" >/dev/null & second_pid=$!
wait "$first_pid"
wait "$second_pid"
if ! cmp -s "$temp_dir/expected-auth-a.json" "$temp_dir/device-auth.json" && \
  ! cmp -s "$temp_dir/expected-auth-b.json" "$temp_dir/device-auth.json"; then
  printf '%s\n' 'Concurrent provisioning produced a mixed credential file.' >&2
  exit 1
fi
if compgen -G "$temp_dir/device-auth.json.tmp.*" >/dev/null; then
  printf '%s\n' 'Concurrent provisioning left a temporary credential file.' >&2
  exit 1
fi
