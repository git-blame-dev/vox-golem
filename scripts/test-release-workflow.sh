#!/usr/bin/env bash
set -eu -o pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
temp_dir="$(mktemp -d)"
trap 'rm -rf "$temp_dir"' EXIT
project="$temp_dir/project"
mkdir -p "$project/scripts" "$project/apps/desktop-tauri/src-tauri" \
  "$project/target/release/bundle/appimage" \
  "$project/target/x86_64-pc-windows-msvc/release/bundle/nsis" \
  "$project/dist/VoxGolem" "$temp_dir/bin"
cp "$root"/scripts/{assert-release-version-newer.sh,finalize-release-assets.sh,generate-update-manifest.sh,preflight-release.sh,select-updater-manifest-url.sh,verify-release-assets.sh} "$project/scripts/"
cp "$root/apps/desktop-tauri/src-tauri/tauri.conf.json" "$project/apps/desktop-tauri/src-tauri/"

workflow_step() {
  bun -e '
    const [path, name] = Bun.argv.slice(1);
    const workflow = Bun.YAML.parse(await Bun.file(path).text());
    const step = workflow.jobs.release.steps.find((candidate) => candidate.name === name);
    if (!step?.run) throw new Error(`workflow step not found: ${name}`);
    process.stdout.write(`${step.run}\n`);
  ' "$root/.github/workflows/release.yml" "$1"
}

bun -e '
  import assert from "node:assert/strict";
  const workflow = Bun.YAML.parse(await Bun.file(Bun.argv[1]).text());
  const steps = workflow.jobs.release.steps;
  const index = (name) => steps.findIndex((step) => step.name === name);
  const install = index("Install release dependencies");
  const linuxIndex = index("Build Linux AppImage with pinned Tauri action");
  const prepareWindowsIndex = index("Prepare Windows native bundle inputs");
  const windowsIndex = index("Build Windows NSIS with pinned Tauri action");
  const finalizer = index("Sign and validate final canonical bytes");
  const publisher = index("Complete and publish draft");
  assert.ok(install >= 0 && install < linuxIndex);
  assert.equal(steps[install].run, "bun install --frozen-lockfile");
  const linux = steps[linuxIndex];
  const windows = steps[windowsIndex];
  const action = "tauri-apps/tauri-action@1deb371b0cd8bd54025b384f1cd735e725c4060f";
  assert.equal(linux.uses, action);
  assert.equal(windows.uses, action);
  assert.equal(linux.with.releaseDraft, true);
  assert.equal(linux.with.releaseCommitish, "${{ github.sha }}");
  assert.equal(windows.with.releaseId, "${{ steps.linux.outputs.releaseId }}");
  assert.equal(steps[prepareWindowsIndex].run, "make prepare-pc-bundle REAL_CLANG_CL=/usr/bin/clang-19");
  assert.ok(linuxIndex < windowsIndex && windowsIndex < finalizer && finalizer < publisher);
' "$root/.github/workflows/release.yml"

cat > "$temp_dir/bin/cargo-tauri" <<'EOF'
#!/usr/bin/env bash
set -eu
for argument in "$@"; do payload="$argument"; done
printf '%s' 'synthetic signature' | base64 > "$payload.sig"
EOF
cat > "$temp_dir/bin/minisign" <<'EOF'
#!/usr/bin/env bash
set -eu
exit 0
EOF
cat > "$project/scripts/verify-windows-installer.sh" <<'EOF'
#!/usr/bin/env bash
set -eu
test -f "$1"
test "$2" = 250000000
EOF
chmod 0755 "$temp_dir/bin/"* "$project/scripts/"*.sh
printf '%s' 'AppImage bytes' > "$project/target/release/bundle/appimage/VoxGolem.AppImage"
printf '%s' 'Windows installer bytes' > "$project/target/x86_64-pc-windows-msvc/release/bundle/nsis/VoxGolem_2026.7.27-42_x64-setup.exe"
printf '%s' 'Linux executable bytes' > "$project/dist/VoxGolem/vox-golem"

finalizer_command="$(workflow_step 'Sign and validate final canonical bytes')"
(
  cd "$project"
  PATH="$temp_dir/bin:$PATH" MINISIGN="$temp_dir/bin/minisign" \
    TAURI_SIGNER="$temp_dir/bin/cargo-tauri" \
    GITHUB_REPOSITORY='example/repository' GITHUB_SHA='0123456789abcdef0123456789abcdef01234567' \
    APP_VERSION='2026.7.27-42' TAG='v2026.7.27-42' \
    TAURI_SIGNING_PRIVATE_KEY='synthetic key' TAURI_SIGNING_PRIVATE_KEY_PASSWORD='' \
    bash -e -o pipefail -c "$finalizer_command"
)
test -d "$project/dist-final"
test "$(<"$project/release-notes.md")" = 'Automated release from 0123456789abcdef0123456789abcdef01234567.'

cat > "$temp_dir/bin/gh" <<'EOF'
#!/usr/bin/env bash
set -eu -o pipefail
command="$1"; shift
if [ "$command" = api ]; then
  include=false; jq_filter=; endpoint=
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --include) include=true; shift ;;
      --paginate) shift ;;
      --jq) jq_filter="$2"; shift 2 ;;
      *) endpoint="$1"; shift ;;
    esac
  done
  case "$endpoint" in
    *'/releases?per_page=100')
      [ "${FAKE_RELEASE_API_ERROR:-}" != true ] || exit 1
      if [ -n "$jq_filter" ]; then jq -c "$jq_filter" <<<"${FAKE_RELEASES:-[]}"; else printf '%s\n' "${FAKE_RELEASES:-[]}"; fi
      ;;
    *'/git/ref/tags/'*)
      case "${FAKE_TAG_STATE:-absent}" in
        present) printf '%s\n%s\n' 'HTTP/2 200 OK' '{"object":{"sha":"tag-object"}}' ;;
        absent) printf '%s\n' 'HTTP/2 404 Not Found'; exit 1 ;;
        error) printf '%s\n' 'HTTP/2 500 Internal Server Error'; exit 1 ;;
      esac
      ;;
    *'/commits/'*)
      [ "${FAKE_TAG_PEEL_ERROR:-}" != true ] || exit 1
      printf '%s\n' "${FAKE_TAG_SHA:?FAKE_TAG_SHA is required}"
      ;;
    *) exit 2 ;;
  esac
  exit 0
fi
test "$command" = release
test "$1" = download; shift 2
destination=
while [ "$#" -gt 0 ]; do
  if [ "$1" = --dir ]; then destination="$2"; shift 2; else shift; fi
done
cp "$FAKE_RELEASE_ASSETS/"* "$destination/"
EOF
cat > "$temp_dir/bin/curl" <<'EOF'
#!/usr/bin/env bash
set -eu
output=
while [ "$#" -gt 0 ]; do
  if [ "$1" = --output ]; then output="$2"; shift 2; else shift; fi
done
cp "$FAKE_MANIFEST" "$output"
EOF
chmod 0755 "$temp_dir/bin/gh" "$temp_dir/bin/curl"

preflight_command="$(workflow_step 'Verify release eligibility')"
target='0123456789abcdef0123456789abcdef01234567'
candidate='v2026.7.27-42'
draft="$(jq -cn --arg tag "$candidate" --arg target "$target" '[{tag_name:$tag,target_commitish:$target,draft:true,prerelease:false,assets:[]}]')"
published="$(jq -cn --arg tag "$candidate" --arg target "$target" '[{tag_name:$tag,target_commitish:$target,draft:false,prerelease:false,assets:[]}]')"

invoke_preflight() {
  local releases="$1" tag_state="$2" tag_sha="${3:-$target}" manifest="${4:-}"
  preflight_output_file="$temp_dir/github-output"
  preflight_stdout_file="$temp_dir/preflight-stdout"
  : > "$preflight_output_file"
  : > "$preflight_stdout_file"
  set +e
  (
    cd "$project"
    PATH="$temp_dir/bin:$PATH" MINISIGN="$temp_dir/bin/minisign" GH_TOKEN='synthetic token' \
      GITHUB_OUTPUT="$preflight_output_file" GITHUB_REPOSITORY='example/repository' \
      TAG="$candidate" APP_VERSION='2026.7.27-42' TARGET_SHA="$target" \
      FAKE_RELEASES="$releases" FAKE_TAG_STATE="$tag_state" FAKE_TAG_SHA="$tag_sha" \
      FAKE_RELEASE_ASSETS="$project/dist-final" FAKE_MANIFEST="$manifest" \
      FAKE_RELEASE_API_ERROR="${FAKE_RELEASE_API_ERROR:-}" \
      FAKE_TAG_PEEL_ERROR="${FAKE_TAG_PEEL_ERROR:-}" \
      bash -e -o pipefail -c "$preflight_command" > "$preflight_stdout_file"
  )
  preflight_status=$?
  set -e
}

expect_preflight_success() {
  local expected="$1"; shift
  invoke_preflight "$@"
  if [ "$preflight_status" -ne 0 ]; then
    printf 'Preflight unexpectedly failed with status %s.\n' "$preflight_status" >&2
    exit 1
  fi
  test ! -s "$preflight_stdout_file"
  test "$(<"$preflight_output_file")" = "$expected"
}

expect_preflight_failure() {
  invoke_preflight "$@"
  if [ "$preflight_status" -eq 0 ]; then
    printf 'Preflight unexpectedly succeeded with output %s.\n' "$(<"$preflight_output_file")" >&2
    exit 1
  fi
}

expect_preflight_success 'build=true' '[]' absent
expect_preflight_success 'build=true' "$draft" absent
expect_preflight_success 'build=true' "$draft" present "$target"
expect_preflight_success 'build=false' "$published" present "$target"

for failure in \
  '[] present deadbeef' \
  "$draft present deadbeef" \
  "$published absent $target" \
  "$published present deadbeef" \
  '[] error deadbeef'; do
  read -r releases tag_state tag_sha <<< "$failure"
  expect_preflight_failure "$releases" "$tag_state" "$tag_sha"
done

FAKE_RELEASE_API_ERROR=true expect_preflight_failure '[]' absent
FAKE_TAG_PEEL_ERROR=true expect_preflight_failure "$draft" present "$target"

printf '%s\n' '{"version":"2026.7.26-1"}' > "$temp_dir/prior-latest.json"
history='[{"tag_name":"v2026.7.26-1","target_commitish":"older","draft":false,"prerelease":false,"assets":[{"name":"latest.json","url":"https://example.invalid/latest.json"}]}]'
expect_preflight_success 'build=true' "$history" absent "$target" "$temp_dir/prior-latest.json"
printf '%s\n' '{"version":"2026.7.28-1"}' > "$temp_dir/newer-latest.json"
advanced_history='[{"tag_name":"v2026.7.28-1","target_commitish":"newer","draft":false,"prerelease":false,"assets":[{"name":"latest.json","url":"https://example.invalid/latest.json"}]}]'
expect_preflight_failure "$advanced_history" absent "$target" "$temp_dir/newer-latest.json"

# Prove rejection assertions fail closed: an always-success preflight mutant
# must make the negative-test helper fail, regardless of its output text.
if (
  preflight_command='printf "%s\n" "build=true" >> "$GITHUB_OUTPUT"'
  expect_preflight_failure '[]' absent
); then
  printf '%s\n' 'Negative preflight tests did not detect an always-success mutant.' >&2
  exit 1
fi

test ! -e "$project/published-release"
test ! -e "$project/published-latest.json"
