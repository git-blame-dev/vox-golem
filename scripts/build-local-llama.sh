#!/usr/bin/env bash
set -euo pipefail

readonly commit='ac2557cb24def295888ef47f1a35b401d978c510'
readonly version='b10054'
source_dir="${LLAMA_SOURCE:?Set LLAMA_SOURCE to the pinned llama.cpp checkout}"
cache_root="${XDG_CACHE_HOME:-$HOME/.cache}/voxgolem/llama.cpp/$version"
runtime_root="${XDG_DATA_HOME:-$HOME/.local/share}/voxgolem/runtimes/llama.cpp/$version"
build_dir="${LLAMA_BUILD_DIR:-$cache_root/build-cuda}"
destination="${LLAMA_RUNTIME_DIR:-$runtime_root}"
cuda_root="${CUDAToolkit_ROOT:-/usr/local/cuda-13.3}"
architecture="${LLAMA_CUDA_ARCHITECTURES:-86}"
jobs="${LLAMA_BUILD_JOBS:-4}"
patchelf_tool="${PATCHELF:-patchelf}"

for tool in git cmake ninja realpath "$patchelf_tool"; do
  command -v "$tool" >/dev/null || { printf 'Missing build tool: %s\n' "$tool" >&2; exit 1; }
done
[[ -x "$cuda_root/bin/nvcc" ]] || { printf 'CUDA compiler unavailable under %s\n' "$cuda_root" >&2; exit 1; }
[[ "$jobs" =~ ^[1-9][0-9]*$ ]] || { printf '%s\n' 'LLAMA_BUILD_JOBS must be a positive integer.' >&2; exit 1; }
[[ "$architecture" =~ ^[0-9]+$ ]] || { printf '%s\n' 'Set one numeric CUDA architecture.' >&2; exit 1; }
[[ "$source_dir" = /* && "$build_dir" = /* && "$destination" = /* && "$cuda_root" = /* ]] || {
  printf '%s\n' 'Source, build, runtime and CUDA paths must be absolute.' >&2
  exit 1
}
source_dir="$(realpath -m -- "$source_dir")"
build_dir="$(realpath -m -- "$build_dir")"
destination="$(realpath -m -- "$destination")"
repository_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
for output in "$build_dir" "$destination"; do
  for checkout in "$source_dir" "$repository_dir"; do
    case "$output/" in
      "$checkout/"*) printf 'Output must be outside source checkouts: %s\n' "$output" >&2; exit 1 ;;
    esac
    case "$checkout/" in
      "$output/"*) printf 'Output must not contain a source checkout: %s\n' "$output" >&2; exit 1 ;;
    esac
  done
done
case "$build_dir/" in
  "$destination/"*) printf '%s\n' 'Build and runtime directories must not overlap.' >&2; exit 1 ;;
esac
case "$destination/" in
  "$build_dir/"*) printf '%s\n' 'Build and runtime directories must not overlap.' >&2; exit 1 ;;
esac
[[ "$(git -C "$source_dir" rev-parse HEAD)" == "$commit" ]] || {
  printf 'The source checkout must be pinned to %s.\n' "$commit" >&2
  exit 1
}
[[ -z "$(git -C "$source_dir" status --porcelain=v1)" ]] || {
  printf '%s\n' 'The pinned source checkout must be clean.' >&2
  exit 1
}
if [[ -e "$destination" || -L "$destination" ]]; then
  printf 'Runtime destination already exists; leave it intact: %s\n' "$destination" >&2
  exit 1
fi

cmake -S "$source_dir" -B "$build_dir" -G Ninja \
  -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_BUILD_WITH_INSTALL_RPATH=ON -DCMAKE_INSTALL_RPATH='$ORIGIN' \
  -DCMAKE_CUDA_COMPILER="$cuda_root/bin/nvcc" \
  -DCUDAToolkit_ROOT="$cuda_root" \
  -DCMAKE_CUDA_ARCHITECTURES="$architecture" \
  -DGGML_CUDA=ON -DGGML_NATIVE=OFF \
  -DLLAMA_BUILD_TESTS=OFF -DLLAMA_BUILD_EXAMPLES=OFF -DLLAMA_BUILD_APP=OFF \
  -DLLAMA_BUILD_SERVER=ON -DLLAMA_BUILD_UI=OFF -DLLAMA_USE_PREBUILT_UI=OFF
cmake --build "$build_dir" --target llama-server --parallel "$jobs"
[[ -x "$build_dir/bin/llama-server" ]] || { printf '%s\n' 'The build did not produce llama-server.' >&2; exit 1; }

mkdir -p "$(dirname "$destination")"
stage="$(mktemp -d "${destination}.stage.XXXXXX")"
trap 'rm -rf -- "$stage"' EXIT
mkdir "$stage/bin"
cp -a "$build_dir/bin/." "$stage/bin/"
for artifact in "$stage"/bin/*; do
  [[ -f "$artifact" && ! -L "$artifact" ]] || continue
  "$patchelf_tool" --set-rpath '$ORIGIN' "$artifact"
  [[ "$("$patchelf_tool" --print-rpath "$artifact")" == '$ORIGIN' ]] || {
    printf 'Runtime library path validation failed: %s\n' "$artifact" >&2
    exit 1
  }
done
cp "$source_dir/LICENSE" "$stage/LICENSE"
printf 'version=%s\nsource_commit=%s\ncuda_architecture=%s\n' \
  "$version" "$commit" "$architecture" > "$stage/BUILD_INFO"
"$cuda_root/bin/nvcc" --version >> "$stage/BUILD_INFO"
(
  cd "$stage"
  find bin -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum > SHA256SUMS
  sha256sum -c SHA256SUMS
)
mv -nT "$stage" "$destination"
if [[ -e "$stage" ]]; then
  printf 'Runtime publication refused; destination appeared during the build: %s\n' "$destination" >&2
  exit 1
fi
trap - EXIT
printf 'Standalone runtime: %s/bin/llama-server\n' "$destination"
