#!/usr/bin/env bash
set -eu -o pipefail

repository="${1:?usage: publish-draft-release.sh REPOSITORY VERSION TARGET_SHA DIST_DIR NOTES_FILE}"
version="${2:?usage: publish-draft-release.sh REPOSITORY VERSION TARGET_SHA DIST_DIR NOTES_FILE}"
target_sha="${3:?usage: publish-draft-release.sh REPOSITORY VERSION TARGET_SHA DIST_DIR NOTES_FILE}"
dist_dir="${4:?usage: publish-draft-release.sh REPOSITORY VERSION TARGET_SHA DIST_DIR NOTES_FILE}"
notes_file="${5:?usage: publish-draft-release.sh REPOSITORY VERSION TARGET_SHA DIST_DIR NOTES_FILE}"
root="$(cd "$(dirname "$0")/.." && pwd)"

case "$version" in v[0-9A-Za-z.-]*) ;; *) printf 'Invalid release version: %s\n' "$version" >&2; exit 1 ;; esac
case "$target_sha" in *[!0-9a-fA-F]*|'') printf 'Invalid release target SHA: %s\n' "$target_sha" >&2; exit 1 ;; esac
export GITHUB_REPOSITORY="$repository"
if [ ! -f "$notes_file" ] || [ -L "$notes_file" ]; then
  printf 'Release notes must be a regular non-symlink file: %s\n' "$notes_file" >&2
  exit 1
fi
"$root/scripts/verify-release-assets.sh" "$dist_dir" "$version"

release_json="$(
  gh api "repos/${repository}/releases" --paginate \
    --jq ".[] | select(.tag_name == \"${version}\")"
)"
if [ -z "$release_json" ]; then
  gh release create "$version" \
    --repo "$repository" \
    --draft \
    --target "$target_sha" \
    --title "VoxGolem ${version}" \
    --notes-file "$notes_file"
else
  test "$(jq -er '.draft' <<< "$release_json")" = true || {
    printf 'Refusing to modify already-published release %s.\n' "$version" >&2
    exit 1
  }
  test "$(jq -er '.target_commitish | strings' <<< "$release_json")" = "$target_sha" || {
    printf 'Draft release %s targets an unexpected commit.\n' "$version" >&2
    exit 1
  }
fi

mapfile -t assets < <(find "$dist_dir" -mindepth 1 -maxdepth 1 -type f -printf '%f\n' | LC_ALL=C sort)
for asset in "${assets[@]}"; do
  gh release upload "$version" "$dist_dir/$asset" --repo "$repository" --clobber
done

remote_dir="$(mktemp -d)"
trap 'rm -rf "$remote_dir"' EXIT
gh release download "$version" --repo "$repository" --dir "$remote_dir"
"$root/scripts/verify-release-assets.sh" "$remote_dir" "$version"
for asset in "${assets[@]}"; do
  cmp "$dist_dir/$asset" "$remote_dir/$asset"
done

release_json="$(gh release view "$version" --repo "$repository" --json assets,isDraft,targetCommitish)"
test "$(jq -er '.isDraft' <<< "$release_json")" = true
test "$(jq -er '.targetCommitish | strings' <<< "$release_json")" = "$target_sha"
mapfile -t remote_assets < <(jq -er '.assets[].name' <<< "$release_json" | LC_ALL=C sort)
test "${remote_assets[*]}" = "${assets[*]}" || {
  printf 'Draft release %s has an unexpected remote asset set.\n' "$version" >&2
  exit 1
}

current_main="$(gh api "repos/${repository}/commits/main" --jq '.sha')"
test "$current_main" = "$target_sha" || {
  printf 'Refusing to publish %s because main is now %s.\n' "$target_sha" "$current_main" >&2
  exit 1
}
gh release edit "$version" --repo "$repository" --draft=false --latest

release_json="$(gh release view "$version" --repo "$repository" --json assets,isDraft,targetCommitish)"
test "$(jq -er '.isDraft' <<< "$release_json")" = false
test "$(jq -er '.targetCommitish | strings' <<< "$release_json")" = "$target_sha"
mapfile -t remote_assets < <(jq -er '.assets[].name' <<< "$release_json" | LC_ALL=C sort)
test "${remote_assets[*]}" = "${assets[*]}" || {
  printf 'Published release %s has an unexpected remote asset set.\n' "$version" >&2
  exit 1
}
