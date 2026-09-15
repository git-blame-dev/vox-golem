#!/usr/bin/env bash
set -eu -o pipefail

repository="${1:?usage: publish-draft-release.sh REPOSITORY VERSION TARGET_SHA DIST_DIR NOTES_FILE}"
version="${2:?usage: publish-draft-release.sh REPOSITORY VERSION TARGET_SHA DIST_DIR NOTES_FILE}"
target_sha="${3:?usage: publish-draft-release.sh REPOSITORY VERSION TARGET_SHA DIST_DIR NOTES_FILE}"
dist_dir="${4:?usage: publish-draft-release.sh REPOSITORY VERSION TARGET_SHA DIST_DIR NOTES_FILE}"
notes_file="${5:?usage: publish-draft-release.sh REPOSITORY VERSION TARGET_SHA DIST_DIR NOTES_FILE}"
root="$(cd "$(dirname "$0")/.." && pwd)"

verify_tag_source() {
  local required="${1:-false}" response rc http_status resolved
  response="$(gh api --include "repos/${repository}/git/ref/tags/${version}" 2>&1)" || rc=$?
  rc="${rc:-0}"
  http_status=
  while IFS= read -r line; do
    if [[ "$line" =~ ^HTTP/[0-9.]+[[:space:]]+([0-9]{3})[[:space:]] ]]; then
      http_status="${BASH_REMATCH[1]}"
      break
    fi
  done <<< "$response"
  if [ "$http_status" = 404 ]; then
    [ "$required" = false ] && return 0
    printf 'Published release tag %s does not exist.\n' "$version" >&2
    exit 1
  fi
  if [ "$rc" -ne 0 ] || [ -z "$http_status" ] || [ "$http_status" -lt 200 ] || [ "$http_status" -ge 300 ]; then
    printf 'Unable to resolve release tag %s (HTTP %s).\n' "$version" "${http_status:-unknown}" >&2
    exit 1
  fi
  # Resolve through the commits endpoint so annotated tags are peeled to the
  # commit SHA rather than compared using the tag object's SHA.
  resolved="$(gh api "repos/${repository}/commits/${version}" --jq '.sha')"
  test "$resolved" = "$target_sha" || {
    printf 'Tag %s does not resolve to expected source.\n' "$version" >&2
    exit 1
  }
}

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
  if [ "$(jq -er '.draft' <<< "$release_json")" = false ]; then
    test "$(jq -er '.prerelease' <<< "$release_json")" = false || { printf 'Published release is prerelease.\n' >&2; exit 1; }
    test "$(jq -er '.target_commitish | strings' <<< "$release_json")" = "$target_sha" || { printf 'Published release targets wrong source.\n' >&2; exit 1; }
    verify_tag_source true
    remote_dir="$(mktemp -d)"; trap 'rm -rf "$remote_dir"' EXIT
    gh release download "$version" --repo "$repository" --dir "$remote_dir"
    "$root/scripts/verify-release-assets.sh" "$remote_dir" "$version"
    for asset in "$dist_dir"/*; do cmp "$asset" "$remote_dir/$(basename "$asset")"; done
    exit 0
  fi
  test "$(jq -er '.prerelease' <<< "$release_json")" = false || {
    printf 'Draft release %s must not be a prerelease.\n' "$version" >&2
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

release_json="$(gh release view "$version" --repo "$repository" --json assets,isDraft,isPrerelease,targetCommitish)"
test "$(jq -er '.isDraft' <<< "$release_json")" = true
test "$(jq -er '.isPrerelease' <<< "$release_json")" = false
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
verify_tag_source false
latest_dir="$(mktemp -d)"
selected_version_file="$latest_dir/selected-version"
trap 'rm -rf "$remote_dir" "$latest_dir"' EXIT
manifest_url="$(SELECTED_UPDATER_VERSION_FILE="$selected_version_file" "$root/scripts/select-updater-manifest-url.sh" "$repository")"
if [ -n "$manifest_url" ]; then
  curl --fail --location --header 'Accept: application/octet-stream' \
    --header "Authorization: Bearer ${GH_TOKEN:?GH_TOKEN is required}" \
    --output "$latest_dir/latest.json" "$manifest_url"
  published_version="$(jq -er '.version | strings' "$latest_dir/latest.json")"
  test "$published_version" = "$(<"$selected_version_file")" || {
    printf '%s\n' 'Published updater manifest version does not match its release tag.' >&2
    exit 1
  }
  "$root/scripts/assert-release-version-newer.sh" "${version#v}" "$published_version"
fi
current_main="$(gh api "repos/${repository}/commits/main" --jq '.sha')"
test "$current_main" = "$target_sha" || {
  printf 'Refusing to publish %s because main is now %s.\n' "$target_sha" "$current_main" >&2
  exit 1
}
gh release edit "$version" --repo "$repository" --draft=false --latest

release_json="$(gh release view "$version" --repo "$repository" --json assets,isDraft,isPrerelease,targetCommitish)"
test "$(jq -er '.isDraft' <<< "$release_json")" = false
test "$(jq -er '.isPrerelease' <<< "$release_json")" = false
test "$(jq -er '.targetCommitish | strings' <<< "$release_json")" = "$target_sha"
verify_tag_source true
mapfile -t remote_assets < <(jq -er '.assets[].name' <<< "$release_json" | LC_ALL=C sort)
test "${remote_assets[*]}" = "${assets[*]}" || {
  printf 'Published release %s has an unexpected remote asset set.\n' "$version" >&2
  exit 1
}
latest_tag="$(gh release view --repo "$repository" --json tagName --jq '.tagName')"
test "$latest_tag" = "$version" || {
  printf 'Latest release resolved to %s instead of %s.\n' "$latest_tag" "$version" >&2
  exit 1
}
