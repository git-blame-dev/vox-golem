#!/usr/bin/env bash
set -eu -o pipefail

if [ "$#" -ne 2 ]; then
  printf 'Usage: %s RELEASE_DIRECTORY TAG\n' "$0" >&2
  exit 2
fi
: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"

release_dir="$1"
tag="$2"
version="${tag#v}"

if [ ! -d "$release_dir" ] || [ -L "$release_dir" ]; then
  printf 'Release directory is missing or unsafe: %s\n' "$release_dir" >&2
  exit 1
fi
if [[ ! "$GITHUB_REPOSITORY" =~ ^[0-9A-Za-z._-]+/[0-9A-Za-z._-]+$ ]] || \
  [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9]+)?$ ]] || \
  [[ "$tag" != "v$version" ]]; then
  printf '%s\n' 'Release repository or tag is invalid.' >&2
  exit 2
fi

linux_zip="vox-golem-linux-${tag}.zip"
linux_asset="vox-golem-linux-x86_64-${tag}.AppImage"
windows_asset="vox-golem-windows-x86_64-${tag}-setup.exe"
expected_files=(
  SHA256SUMS
  latest.json
  "$linux_zip"
  "$linux_asset"
  "$linux_asset.sig"
  "$windows_asset"
  "$windows_asset.sig"
)
mapfile -t actual_files < <(find "$release_dir" -mindepth 1 -maxdepth 1 -printf '%f\n' | sort)
mapfile -t sorted_expected_files < <(printf '%s\n' "${expected_files[@]}" | sort)
if [ "$(printf '%s\n' "${actual_files[@]}")" != "$(printf '%s\n' "${sorted_expected_files[@]}")" ]; then
  printf '%s\n' 'Release assets do not match the exact combined-platform allowlist.' >&2
  printf 'Expected: %s\n' "${sorted_expected_files[*]}" >&2
  printf 'Actual:   %s\n' "${actual_files[*]}" >&2
  exit 1
fi
for file in "${expected_files[@]}"; do
  if [ ! -f "$release_dir/$file" ] || [ -L "$release_dir/$file" ]; then
    printf 'Release asset is not a regular non-symlink file: %s\n' "$file" >&2
    exit 1
  fi
done

expected_checksums=(
  latest.json
  "$linux_zip"
  "$linux_asset"
  "$linux_asset.sig"
  "$windows_asset"
  "$windows_asset.sig"
)
mapfile -t checksum_files < <(cut -d ' ' -f 3- "$release_dir/SHA256SUMS" | sort)
mapfile -t sorted_expected_checksums < <(printf '%s\n' "${expected_checksums[@]}" | sort)
if [ "$(printf '%s\n' "${checksum_files[@]}")" != "$(printf '%s\n' "${sorted_expected_checksums[@]}")" ]; then
  printf '%s\n' 'SHA256SUMS does not cover the exact release asset set.' >&2
  exit 1
fi
(cd "$release_dir" && sha256sum --strict -c SHA256SUMS)

linux_signature="$(<"$release_dir/$linux_asset.sig")"
windows_signature="$(<"$release_dir/$windows_asset.sig")"
linux_url="https://github.com/${GITHUB_REPOSITORY}/releases/download/${tag}/${linux_asset}"
windows_url="https://github.com/${GITHUB_REPOSITORY}/releases/download/${tag}/${windows_asset}"
jq -e \
  --arg version "$version" \
  --arg linux_signature "$linux_signature" \
  --arg linux_url "$linux_url" \
  --arg windows_signature "$windows_signature" \
  --arg windows_url "$windows_url" '
    (. | keys) == ["platforms", "version"] and
    .version == $version and
    (.platforms | keys) == ["linux-x86_64", "windows-x86_64"] and
    (.platforms["linux-x86_64"] | keys) == ["signature", "url"] and
    .platforms["linux-x86_64"].signature == $linux_signature and
    .platforms["linux-x86_64"].url == $linux_url and
    (.platforms["windows-x86_64"] | keys) == ["signature", "url"] and
    .platforms["windows-x86_64"].signature == $windows_signature and
    .platforms["windows-x86_64"].url == $windows_url
  ' "$release_dir/latest.json" >/dev/null
