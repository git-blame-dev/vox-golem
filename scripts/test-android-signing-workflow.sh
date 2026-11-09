#!/usr/bin/env bash
set -eu -o pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
temp_dir="$(mktemp -d)"
trap 'rm -rf "$temp_dir"' EXIT

# Exercise the actual signing recipe up to apksigner with synthetic outputs.
# SDK location selection itself is established separately against SDK classes;
# these fixtures cover both an XDG default and the workflow's explicit override.
bun -e '
  const job = Bun.YAML.parse(await Bun.file(`${Bun.argv[1]}/.github/workflows/ci.yml`).text()).jobs["android-artifact"];
  const step = job.steps.find(s => s.name === "Sign and verify the optimized phone CI trial");
  await Bun.write(`${Bun.argv[2]}/sign.sh`, step.run);
  const setup = job.steps.find(s => s.name === "Configure isolated Android user directory");
  await Bun.write(`${Bun.argv[2]}/setup.sh`, setup.run);
' "$root" "$temp_dir"

RUNNER_TEMP="$temp_dir" GITHUB_ENV="$temp_dir/job.env" bash "$temp_dir/setup.sh"
sed -n 's/^ANDROID_USER_HOME=//p' "$temp_dir/job.env" > "$temp_dir/location"
test -s "$temp_dir/location"
test -d "$(cat "$temp_dir/location")"

mkdir -p "$temp_dir/sdk/build-tools/36.0.0"
ln -s "$temp_dir/sdk/build-tools/36.0.0" "$temp_dir/bin"
cat > "$temp_dir/bin/zipalign" <<'TOOL'
#!/usr/bin/env bash
exit 0
TOOL
cat > "$temp_dir/bin/apksigner" <<'TOOL'
#!/usr/bin/env bash
set -eu
test "$1" = sign
test "$2" = --ks
test "$3" = "$EXPECTED_KEYSTORE"
test "$(cat "$3")" = synthetic-debug-key
# Stop at the real SDK signing boundary; no fake APK is cryptographically valid.
exit 42
TOOL
chmod 0755 "$temp_dir/bin/"*

for missing in none apk mapping keystore; do
  project="$temp_dir/$missing"
  build="$project/apps/android-tauri/src-tauri/gen/android/app/build"
  mkdir -p "$build/outputs/apk/universal/release" "$build/outputs/mapping/universalRelease" \
    "$project/home" "$project/xdg/.android"
  apk="$build/outputs/apk/universal/release/app-universal-release-unsigned.apk"
  mapping="$build/outputs/mapping/universalRelease/mapping.txt"
  location="$(cat "$temp_dir/location")"
  sdk_user_home="${location:-$project/xdg/.android}"
  mkdir -p "$sdk_user_home"
  keystore="$sdk_user_home/debug.keystore"
  printf '%s\n' synthetic-apk > "$apk"
  printf '%s\n' synthetic-mapping > "$mapping"
  printf '%s\n' synthetic-debug-key > "$keystore"
  case "$missing" in
    apk) rm "$apk" ;;
    mapping) rm "$mapping" ;;
    keystore) rm "$keystore" ;;
  esac
  set +e
  (
    cd "$project"
    export HOME="$project/home" XDG_CONFIG_HOME="$project/xdg"
    unset ANDROID_USER_HOME
    if [ -n "$location" ]; then export ANDROID_USER_HOME="$location"; fi
    ANDROID_HOME="$temp_dir/sdk" EXPECTED_KEYSTORE="$keystore" \
      bash -e -o pipefail -c "$(cat "$temp_dir/sign.sh")"
  ) > "$temp_dir/$missing.log" 2>&1
  status=$?
  set -e
  if [ "$missing" = none ]; then
    test "$status" -eq 42 || {
      cat "$temp_dir/$missing.log" >&2
      printf 'Valid SDK keystore fixture did not reach signing (status %s).\n' "$status" >&2
      exit 1
    }
  else
    test "$status" -eq 1
    grep -Fq "::error::Android CI signing precondition missing: $missing" "$temp_dir/$missing.log"
  fi
done
printf '%s\n' 'Android CI signing preconditions passed (SDK signing boundary stubbed).'
