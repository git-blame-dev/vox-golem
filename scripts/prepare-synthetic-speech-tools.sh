#!/usr/bin/env bash
# Isolated Linux eSpeak NG for deterministic synthetic fixtures; FFmpeg is separate.
set -eu -o pipefail

if [ "$#" -ne 1 ] || [[ "$1" != /* ]]; then
  printf 'Usage: %s ABSOLUTE_INSTALL_PREFIX\n' "$0" >&2
  exit 1
fi

prefix="$(realpath -m -- "$1")"
commit=4870adfa25b1a32b4361592f1be8a40337c58d6c # Official 1.52.0 tag.
archive_sha256=cd83f84c4e495f281ac14e919aecf2834306ec1ea1b498de8ef6000f3a0f90de
url="https://codeload.github.com/espeak-ng/espeak-ng/tar.gz/$commit"

mkdir -p -- "$(dirname -- "$prefix")"
work="$(mktemp -d "$(dirname -- "$prefix")/.synthetic-speech.XXXXXX")"
trap 'rm -rf -- "$work"' EXIT

verify_tool() {
  local version
  version="$(env -u ESPEAK_DATA_PATH "$prefix/bin/espeak-ng" --version)" || return 1
  # Do not mistake 1.51 (which lacks -D) for a usable installation.
  if [[ ! "$version" =~ ^eSpeak\ NG\ text-to-speech:\ ([0-9]+\.[0-9]+\.[0-9]+) ]]; then
    return 1
  fi
  if [ "$(printf '%s\n' 1.52.0 "${BASH_REMATCH[1]}" | sort -V | head -n 1)" != 1.52.0 ]; then
    return 1
  fi
  for sample in first second; do
    env -u ESPEAK_DATA_PATH "$prefix/bin/espeak-ng" -D -v en-us+f3 -s 150 \
      -w "$work/$sample.wav" 'Synthetic deterministic speech fixture.' || return 1
    test -s "$work/$sample.wav" || return 1
  done
  cmp "$work/first.wav" "$work/second.wav" || return 1
  printf '%s\n' "$version"
}

if [ -x "$prefix/bin/espeak-ng" ] && verify_tool; then
  printf 'Reusing compatible deterministic eSpeak NG in %s\n' "$prefix"
  exit 0
fi
if [ -e "$prefix" ] || [ -L "$prefix" ]; then
  printf 'Refusing to replace an existing incompatible prefix: %s\n' "$prefix" >&2
  exit 1
fi

curl --fail --location --proto '=https' --proto-redir '=https' --tlsv1.2 \
  --retry 3 --connect-timeout 30 --max-time 300 --output "$work/source.tar.gz" "$url"
printf '%s  %s\n' "$archive_sha256" "$work/source.tar.gz" | sha256sum -c -
tar -xzf "$work/source.tar.gz" -C "$work"
source_dir="$work/espeak-ng-$commit"

# 1.52.0 cmake/deps.cmake fetches Sonic unconditionally when it is absent.
# Honor USE_LIBSONIC=OFF before that FetchContent branch. This changes dependency
# setup only, not synthesis; the pinned archive makes this substitution fixed.
sed -i 's/^else()$/elseif(USE_LIBSONIC)/' "$source_dir/cmake/deps.cmake"

env -u ESPEAK_DATA_PATH cmake -S "$source_dir" -B "$work/build" \
  -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$prefix" \
  -DBUILD_SHARED_LIBS=OFF -DBUILD_TESTING=OFF -DESPEAK_BUILD_MANPAGES=OFF \
  -DUSE_ASYNC=OFF -DUSE_MBROLA=OFF -DUSE_LIBSONIC=OFF -DUSE_LIBPCAUDIO=OFF \
  -DFETCHCONTENT_FULLY_DISCONNECTED=ON
env -u ESPEAK_DATA_PATH cmake --build "$work/build" --target espeak-ng-bin data --parallel 2
cmake --install "$work/build"
verify_tool
printf 'Prepared deterministic eSpeak NG in %s; add %s/bin to PATH.\n' "$prefix" "$prefix"
