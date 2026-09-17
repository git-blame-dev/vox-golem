#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
  printf 'usage: %s REPOSITORY TARGET_SHA [UTC_DATE]\n' "$0" >&2
  exit 2
fi

repository=$1
target_sha=$2
release_date=${3:-$(date -u '+%Y.%-m.%-d')}
releases_file=$(mktemp)
trap 'rm -f "$releases_file"' EXIT

if ! gh api --paginate "repos/$repository/releases?per_page=100" --jq '.[]' > "$releases_file"; then
  printf 'could not query GitHub releases for version selection\n' >&2
  exit 1
fi

python3 - "$releases_file" "$target_sha" "$release_date" <<'PY'
import datetime
import json
import pathlib
import re
import sys

source, target_sha, release_date = sys.argv[1:]
date_match = re.fullmatch(r"(\d{4})\.([1-9]\d?)\.([1-9]\d?)", release_date)
if not date_match:
    raise SystemExit(f"release date is not canonical: {release_date}")
year, month, day = map(int, date_match.groups())
try:
    datetime.date(year, month, day)
except ValueError as error:
    raise SystemExit(f"release date is invalid: {error}")

canonical = re.compile(r"^v(\d{4})\.([1-9]\d?)\.([1-9]\d?)-([1-9]\d*)$")
seen_tags = set()
reserved_ordinals = []
same_source = []

for line_number, line in enumerate(pathlib.Path(source).read_text().splitlines(), 1):
    if not line.strip():
        continue
    try:
        release = json.loads(line)
    except json.JSONDecodeError as error:
        raise SystemExit(f"invalid GitHub release response on line {line_number}: {error}")
    if not isinstance(release, dict):
        raise SystemExit(f"invalid GitHub release response on line {line_number}")

    tag = release.get("tag_name", "")
    if not isinstance(tag, str):
        raise SystemExit(f"invalid release tag metadata on line {line_number}")
    match = canonical.fullmatch(tag)
    if not match:
        continue
    tag_year, tag_month, tag_day = map(int, match.groups()[:3])
    try:
        datetime.date(tag_year, tag_month, tag_day)
    except ValueError as error:
        raise SystemExit(f"invalid calendar release tag {tag}: {error}")
    if tag in seen_tags:
        raise SystemExit(f"GitHub returned duplicate releases for {tag}")
    seen_tags.add(tag)

    target = release.get("target_commitish", "")
    if not isinstance(target, str) or not target:
        raise SystemExit(f"invalid release target metadata for {tag}")
    if not isinstance(release.get("draft"), bool) or not isinstance(release.get("prerelease"), bool):
        raise SystemExit(f"invalid release state metadata for {tag}")

    tag_date = ".".join(match.groups()[:3])
    if tag_date == release_date:
        reserved_ordinals.append(int(match.group(4)))
    if target == target_sha:
        same_source.append((tag[1:], release["prerelease"]))

if len(same_source) > 1:
    raise SystemExit(f"multiple releases target source {target_sha}")
if same_source:
    version, prerelease = same_source[0]
    if prerelease:
        raise SystemExit(f"canonical release v{version} for source {target_sha} is a prerelease")
    print(version)
else:
    print(f"{release_date}-{max(reserved_ordinals, default=0) + 1}")
PY
