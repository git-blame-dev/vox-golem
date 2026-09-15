#!/usr/bin/env bash
set -eu -o pipefail

if [ "$#" -ne 4 ]; then
  printf 'Usage: %s REPOSITORY TAG APP_VERSION TARGET_SHA\n' "$0" >&2
  exit 2
fi
repository="$1"; tag="$2"; app_version="$3"; target_sha="$4"
root="$(cd "$(dirname "$0")/.." && pwd)"
: "${GH_TOKEN:?GH_TOKEN is required}"
export TAG="$tag"
work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT

release_json="$(gh api --paginate "repos/${repository}/releases?per_page=100" --jq '.[] | select(.tag_name == env.TAG)' || {
  printf '%s\n' 'Unable to query release history.' >&2
  exit 1
})"
if [ -n "$release_json" ]; then
  test "$(jq -er '.prerelease' <<<"$release_json")" = false || { printf '%s\n' 'Release must not be a prerelease.' >&2; exit 1; }
  test "$(jq -er '.target_commitish | strings' <<<"$release_json")" = "$target_sha" || { printf '%s\n' 'Release targets the wrong source.' >&2; exit 1; }
fi

tag_response="$(gh api --include "repos/${repository}/git/ref/tags/${tag}" 2>&1)" || tag_rc=$?
tag_rc="${tag_rc:-0}"
tag_status="$(sed -nE 's#^HTTP/[0-9.]+ ([0-9]{3}) .*#\1#p' <<<"$tag_response" | head -n 1)"
if [ "$tag_status" = 404 ]; then
  tag_exists=false
elif [ "$tag_rc" -eq 0 ] && [[ "$tag_status" =~ ^2[0-9][0-9]$ ]]; then
  tag_exists=true
  resolved="$(gh api "repos/${repository}/commits/${tag}" --jq '.sha')" || {
    printf '%s\n' 'Unable to peel release tag.' >&2
    exit 1
  }
  test "$resolved" = "$target_sha" || { printf '%s\n' 'Release tag resolves to the wrong source.' >&2; exit 1; }
else
  printf 'Unable to resolve release tag (HTTP %s).\n' "${tag_status:-unknown}" >&2
  exit 1
fi

if [ -n "$release_json" ] && [ "$(jq -er '.draft' <<<"$release_json")" = false ]; then
  test "$tag_exists" = true || { printf '%s\n' 'Published release tag does not exist.' >&2; exit 1; }
  published_dir="$work_dir/published-release"
  mkdir "$published_dir"
  gh release download "$tag" --repo "$repository" --dir "$published_dir" >&2
  GITHUB_REPOSITORY="$repository" "$root/scripts/verify-release-assets.sh" "$published_dir" "$tag" >&2
  printf 'build=false\n'
  exit 0
fi

selected_version_file="$work_dir/selected-version"
manifest_url="$(SELECTED_UPDATER_VERSION_FILE="$selected_version_file" "$root/scripts/select-updater-manifest-url.sh" "$repository")"
if [ -n "$manifest_url" ]; then
  latest_manifest="$work_dir/latest.json"
  curl --fail --location --header 'Accept: application/octet-stream' --header "Authorization: Bearer $GH_TOKEN" --output "$latest_manifest" "$manifest_url" >&2
  published_version="$(jq -er '.version | strings' "$latest_manifest")"
  test "$published_version" = "$(<"$selected_version_file")" || { printf '%s\n' 'Published updater manifest version mismatch.' >&2; exit 1; }
  "$root/scripts/assert-release-version-newer.sh" "$app_version" "$published_version" >&2
fi
printf 'build=true\n'
