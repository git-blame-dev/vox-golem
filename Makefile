.SUFFIXES:

SHELL := /bin/bash
.SHELLFLAGS := -eu -o pipefail -c
export CARGO_INCREMENTAL ?= 0

WINDOWS_TARGET := x86_64-pc-windows-msvc
LINUX_RELEASE_DIR := $(CURDIR)/target/release
LINUX_STAGED_RELEASE_DIR := $(CURDIR)/dist/VoxGolem
LINUX_APPIMAGE_DIR := $(CURDIR)/target/release/bundle/appimage
override TAURI_CACHE_ROOT := $(if $(XDG_CACHE_HOME),$(XDG_CACHE_HOME),$(HOME)/.cache)
override TAURI_BUNDLER_CACHE_DIR := $(TAURI_CACHE_ROOT)/tauri
override TAURI_APPRUN := $(TAURI_BUNDLER_CACHE_DIR)/AppRun-x86_64
override TAURI_APPRUN_URL := https://api.github.com/repos/tauri-apps/binary-releases/releases/assets/274691722
override TAURI_APPRUN_SHA256 := f30140a43a0a59e46db21bdefdf749b9e9f2c6946e92afabbacf98b8ae73fb4f
override TAURI_LINUXDEPLOY := $(TAURI_BUNDLER_CACHE_DIR)/linuxdeploy-x86_64.AppImage
override TAURI_LINUXDEPLOY_URL := https://api.github.com/repos/tauri-apps/binary-releases/releases/assets/182515537
override TAURI_LINUXDEPLOY_SHA256 := e762bea85c8eb0d4b3508d46e5c1f037f717d0f9303ae3b4aafc8b04991fa1ef
override TAURI_GTK_PLUGIN := $(TAURI_BUNDLER_CACHE_DIR)/linuxdeploy-plugin-gtk.sh
override TAURI_GTK_PLUGIN_URL := https://raw.githubusercontent.com/tauri-apps/linuxdeploy-plugin-gtk/b5eb8d05b4c0ed40107fe2158c5d8527f94568ef/linuxdeploy-plugin-gtk.sh
override TAURI_GTK_PLUGIN_SHA256 := cb379f9b0733e9ad9f8bd78f8c2fa038aef2478523bb7d4c8e64ff6a1ea3501a
override TAURI_GSTREAMER_PLUGIN := $(TAURI_BUNDLER_CACHE_DIR)/linuxdeploy-plugin-gstreamer.sh
override TAURI_GSTREAMER_PLUGIN_URL := https://raw.githubusercontent.com/tauri-apps/linuxdeploy-plugin-gstreamer/2a2e67491c32995a3f279ad0ecbe77abd512b42a/linuxdeploy-plugin-gstreamer.sh
override TAURI_GSTREAMER_PLUGIN_SHA256 := c107b49d84edbffc6ab226ed1007e0626a4f7aa2c3a36b7782bef62351d49e94
override LINUXDEPLOY_APPIMAGE_PLUGIN := $(TAURI_BUNDLER_CACHE_DIR)/linuxdeploy-plugin-appimage.AppImage
override LINUXDEPLOY_APPIMAGE_PLUGIN_URL := https://api.github.com/repos/linuxdeploy/linuxdeploy-plugin-appimage/releases/assets/462804774
override LINUXDEPLOY_APPIMAGE_PLUGIN_SHA256 := 1da16a46fa5e058ae740e7c35ed0d36d86cb869ac9cc8a5fd9a1847d7978d99a
override APPIMAGE_RUNTIME := $(TAURI_BUNDLER_CACHE_DIR)/runtime-x86_64
override APPIMAGE_RUNTIME_URL := https://api.github.com/repos/AppImage/type2-runtime/releases/assets/456065460
override APPIMAGE_RUNTIME_SHA256 := 1cc49bcf1e2ccd593c379adb17c9f85a36d619088296504de95b1d06215aebbf
override MAX_LINUX_APPIMAGE_BYTES := 300000000
BUILD_SEQUENCE := $(if $(GITHUB_RUN_NUMBER),$(GITHUB_RUN_NUMBER),0)
override APP_VERSION := $(shell scripts/release-version.sh "$$(git show -s --format=%ct HEAD)" '$(BUILD_SEQUENCE)')
LINUX_ORT_LIB_DIR ?= $(if $(ORT_LIB_PATH),$(ORT_LIB_PATH),$(LINUX_RELEASE_DIR))
WINDOWS_RELEASE_DIR := $(CURDIR)/target/$(WINDOWS_TARGET)/release
WINDOWS_STAGED_RELEASE_DIR := $(CURDIR)/dist/VoxGolem-windows
WINDOWS_NSIS_DIR := $(WINDOWS_RELEASE_DIR)/bundle/nsis
WINDOWS_NSIS_INSTALLER := $(WINDOWS_NSIS_DIR)/VoxGolem_$(APP_VERSION)_x64-setup.exe
IMPORT_LIB_DIR := $(CURDIR)/target/import-libs
CROSS_SHIM_DIR := $(CURDIR)/target/cross-shims
ESPEAK_COMPAT_HEADER := $(CROSS_SHIM_DIR)/espeak_windows_compat.h
WINDOWS_NSIS_TEMPLATE := $(CROSS_SHIM_DIR)/windows-installer.nsi
TAURI_NSIS_TEMPLATE_SOURCE := $(CURDIR)/.deps/tauri-bundler/2.9.1/installer.nsi
TAURI_NSIS_TEMPLATE_URL := https://raw.githubusercontent.com/tauri-apps/tauri/e5ae5b93cdd310045191cc0526f253140ad64b87/crates/tauri-bundler/src/bundle/windows/nsis/installer.nsi
TAURI_NSIS_TEMPLATE_SHA256 := ee84148e405adc4d736a46456dd8345a644751bd1f28a335dd7fd833a32d7c3e
MAX_WINDOWS_INSTALLER_BYTES := 250000000
WINDOWS_CROSS_BIN := $(CURDIR)/scripts/windows-cross
WINDOWS_CLANG_CL := $(WINDOWS_CROSS_BIN)/clang-cl
REAL_CLANG_CL := $(shell command -v clang-cl 2>/dev/null || command -v clang-19 2>/dev/null)
STDCXX_IMPORT_LIB := $(IMPORT_LIB_DIR)/stdc++.lib
DIRECTML_IMPORT_LIB := $(IMPORT_LIB_DIR)/DirectML.lib
PATHCCH_IMPORT_LIB := $(IMPORT_LIB_DIR)/PathCch.lib
VC_REDIST_VERSION := 14.51.36247.0
VC_REDIST_URL := https://download.visualstudio.microsoft.com/download/pr/ebdab8e5-1d7b-4d9f-a11b-cbb1720c3b12/843068991DAAA1F73AD9F6239BCE4D0F6A07A51F18C37EA2A867E9BECA71295C/VC_redist.x64.exe
VC_REDIST_SHA256 := 843068991daaa1f73ad9f6239bce4d0f6a07a51f18c37ea2a867e9beca71295c
VC_REDIST_ARCHIVE := $(CURDIR)/.deps/downloads/VC_redist.x64-$(VC_REDIST_VERSION).exe
VC_RUNTIME_ROOT := $(CURDIR)/.deps/vc-runtime/$(VC_REDIST_VERSION)
VC_RUNTIME_DIR := $(VC_RUNTIME_ROOT)/bin

ORT_RUNTIME_DLLS := \
	DirectML.dll \
	onnxruntime_providers_cuda.dll \
	onnxruntime_providers_shared.dll

VC_RUNTIME_DLLS := \
	MSVCP140.dll \
	MSVCP140_1.dll \
	VCRUNTIME140.dll \
	VCRUNTIME140_1.dll

REQUIRED_DIST_FILES := \
	vox-golem.exe \
	DirectML.dll \
	onnxruntime_providers_cuda.dll \
	onnxruntime_providers_shared.dll \
	$(VC_RUNTIME_DLLS)

LINUX_ORT_PROVIDER_LIBS := \
	libonnxruntime_providers_shared.so \
	libonnxruntime_providers_cuda.so

.DEFAULT_GOAL := help

.PHONY: help app-version test test-release-tools check-linux-tools app app-smoke packaged-smoke app-dev linux dist verify-dist update-bundle verify-update-bundle update-bundle-smoke check-pc-tools pc pc-dist verify-pc-dist pc-installer verify-pc-installer clean

help:
	@printf '%s\n' 'Targets:'
	@printf '%s\n' '  make app      Run the native Linux Tauri app'
	@printf '%s\n' '  make app-smoke Build and require the native Linux shell-ready marker'
	@printf '%s\n' '  make packaged-smoke Run the staged Linux package from a separate directory'
	@printf '%s\n' '  make app-dev  Run only the frontend development server'
	@printf '%s\n' '  make test     Run deterministic Linux frontend and Rust checks'
	@printf '%s\n' '  make app-version Print the generated application build identity'
	@printf '%s\n' '  make linux    Build the native Linux Tauri binary'
	@printf '%s\n' '  make dist     Stage the Linux binary under dist/VoxGolem'
	@printf '%s\n' '  make update-bundle Build the generated-version Linux AppImage update bundle'
	@printf '%s\n' '  make pc       Cross-build the optional Windows app from Linux'
	@printf '%s\n' '  make pc-dist  Stage optional Windows files under dist/VoxGolem-windows'
	@printf '%s\n' '  make pc-installer Build the lightweight Windows NSIS installer from Linux'
	@printf '%s\n' '  make clean    Remove generated build and staging output'

app-version:
	@test -n '$(APP_VERSION)' || { printf '%s\n' 'Failed to generate application version.' >&2; exit 1; }
	@printf '%s\n' '$(APP_VERSION)'

test:
	bun install --frozen-lockfile
	bun run typecheck
	bun run lint
	bun run test
	bun run build
	$(MAKE) --no-print-directory test-release-tools
	cargo fmt --all -- --check
	cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
	cargo test --locked --workspace

test-release-tools:
	@command -v jq >/dev/null || { printf '%s\n' 'Missing jq.' >&2; exit 1; }
	bash scripts/test-release-tools.sh

check-linux-tools:
	@command -v cargo-tauri >/dev/null || { printf '%s\n' 'Missing Tauri CLI. Install with: cargo install tauri-cli --version 2.11.1 --locked' >&2; exit 1; }
	@pkg-config --exists webkit2gtk-4.1 gtk+-3.0 || { printf '%s\n' 'Missing Linux Tauri development libraries (WebKitGTK 4.1 and GTK 3).' >&2; exit 1; }

app: check-linux-tools
	bun install --frozen-lockfile
	cargo tauri dev -- --locked

app-smoke: linux
	@set -eu; \
		app_pid=; \
		smoke_log='/tmp/vox-golem-app-smoke.'$$$$'.log'; \
		smoke_config='/tmp/vox-golem-app-smoke-config.'$$$$; \
		cleanup() { \
			status="$$1"; \
			if [ -n "$$app_pid" ] && kill -0 "$$app_pid" 2>/dev/null; then \
				kill -TERM -- "-$$app_pid" 2>/dev/null || true; \
				for _ in 1 2 3 4 5; do \
					kill -0 "$$app_pid" 2>/dev/null || break; \
					sleep 1; \
				 done; \
				kill -KILL -- "-$$app_pid" 2>/dev/null || true; \
				wait "$$app_pid" 2>/dev/null || true; \
			fi; \
			if [ "$$status" -ne 0 ] && [ -f "$$smoke_log" ]; then \
				printf '%s\n' 'Linux app smoke diagnostics:' >&2; \
				cat "$$smoke_log" >&2 || true; \
			fi; \
			rm -f "$$smoke_config" "$$smoke_log" || true; \
		}; \
		trap 'exit 130' INT; trap 'exit 143' TERM; \
		trap 'status=$$?; cleanup "$$status"; exit "$$status"' EXIT; \
		setsid env VOXGOLEM_CONFIG_PATH="$$smoke_config" '$(LINUX_RELEASE_DIR)/vox-golem' >"$$smoke_log" 2>&1 & app_pid=$$!; \
		for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30; do \
			sleep 1; \
			state="$$(ps -o stat= -p "$$app_pid" 2>/dev/null || true)"; \
			if ! kill -0 "$$app_pid" 2>/dev/null || case "$$state" in Z*) true;; *) false;; esac; then \
				wait "$$app_pid" || status=$$?; \
				printf 'Linux app smoke failed: process exited before the 30-second deadline (status %s). Output: %s\n' "$${status:-unknown}" "$$smoke_log" >&2; \
				exit 1; \
			fi; \
			if grep -q 'VOXGOLEM_STARTUP_READY' "$$smoke_log"; then \
				printf '%s\n' 'Linux app smoke passed: startup-ready marker observed.'; \
				exit 0; \
			fi; \
		done; \
		printf 'Linux app smoke timed out after 30 seconds: startup-ready marker was not observed. Output: %s\n' "$$smoke_log" >&2; \
		exit 1

app-dev:
	bun run dev

linux: check-linux-tools
	bun install --frozen-lockfile
	cargo tauri build --no-bundle -- --locked
	@printf 'Linux app: %s\n' '$(LINUX_RELEASE_DIR)/vox-golem'

dist: linux
	@rm -rf '$(LINUX_STAGED_RELEASE_DIR)'
	@mkdir -p '$(LINUX_STAGED_RELEASE_DIR)'
	cp '$(LINUX_RELEASE_DIR)/vox-golem' '$(LINUX_STAGED_RELEASE_DIR)/vox-golem'
	@for file in $(LINUX_ORT_PROVIDER_LIBS); do \
		source='$(LINUX_ORT_LIB_DIR)/'$$file; \
		if [ ! -f "$$source" ]; then \
			printf 'Missing TTS-capable ONNX Runtime provider asset: %s (expected in exact LINUX_ORT_LIB_DIR=%s; set ORT_LIB_PATH to override)\n' "$$file" '$(LINUX_ORT_LIB_DIR)' >&2; \
			exit 1; \
		fi; \
		install -m 0755 "$$source" '$(LINUX_STAGED_RELEASE_DIR)/'$$file; \
	done
	@$(MAKE) --no-print-directory verify-dist

verify-dist:
	@test -x '$(LINUX_STAGED_RELEASE_DIR)/vox-golem' || { printf 'Missing staged Linux binary: %s\n' '$(LINUX_STAGED_RELEASE_DIR)/vox-golem' >&2; exit 1; }
	@for file in $(LINUX_ORT_PROVIDER_LIBS); do \
		test -f '$(LINUX_STAGED_RELEASE_DIR)'/$$file || { printf 'Missing staged ONNX Runtime provider: %s\n' "$$file" >&2; exit 1; }; \
		test "$$(stat -c '%a' '$(LINUX_STAGED_RELEASE_DIR)'/$$file)" = 755 || { printf 'Provider has incorrect mode: %s\n' "$$file" >&2; exit 1; }; \
	done
	@test "$$(find '$(LINUX_STAGED_RELEASE_DIR)' -maxdepth 1 -type f | wc -l)" -eq 3 || { printf '%s\n' 'Unexpected files in Linux package.' >&2; exit 1; }
	@printf 'vox-golem\t%s bytes\n' "$$(stat -c '%s' '$(LINUX_STAGED_RELEASE_DIR)/vox-golem')"

update-bundle: check-linux-tools
	@test -n '$(APP_VERSION)' || { printf '%s\n' 'Failed to generate application version.' >&2; exit 1; }
	@case '$(APP_VERSION)' in *[!0-9A-Za-z.-]*|'') printf 'Invalid APP_VERSION: %s\n' '$(APP_VERSION)' >&2; exit 1;; esac
	@test -n '$(XDG_CACHE_HOME)$(HOME)' || { printf '%s\n' 'An absolute XDG_CACHE_HOME or HOME is required for Tauri tool pinning.' >&2; exit 1; }
	@case '$(TAURI_CACHE_ROOT)' in /*) ;; *) printf 'Tauri cache root must be absolute: %s\n' '$(TAURI_CACHE_ROOT)' >&2; exit 1;; esac
	bash scripts/prepare-tauri-bundler-tool.sh '$(TAURI_APPRUN)' '$(TAURI_APPRUN_URL)' '$(TAURI_APPRUN_SHA256)'
	bash scripts/prepare-tauri-bundler-tool.sh '$(TAURI_LINUXDEPLOY)' '$(TAURI_LINUXDEPLOY_URL)' '$(TAURI_LINUXDEPLOY_SHA256)'
	bash scripts/prepare-tauri-bundler-tool.sh '$(TAURI_GTK_PLUGIN)' '$(TAURI_GTK_PLUGIN_URL)' '$(TAURI_GTK_PLUGIN_SHA256)'
	bash scripts/prepare-tauri-bundler-tool.sh '$(TAURI_GSTREAMER_PLUGIN)' '$(TAURI_GSTREAMER_PLUGIN_URL)' '$(TAURI_GSTREAMER_PLUGIN_SHA256)'
	bash scripts/prepare-tauri-bundler-tool.sh '$(LINUXDEPLOY_APPIMAGE_PLUGIN)' '$(LINUXDEPLOY_APPIMAGE_PLUGIN_URL)' '$(LINUXDEPLOY_APPIMAGE_PLUGIN_SHA256)'
	bash scripts/prepare-tauri-bundler-tool.sh '$(APPIMAGE_RUNTIME)' '$(APPIMAGE_RUNTIME_URL)' '$(APPIMAGE_RUNTIME_SHA256)'
	@rm -rf '$(LINUX_APPIMAGE_DIR)'
	bun install --frozen-lockfile
	LDAI_RUNTIME_FILE='$(APPIMAGE_RUNTIME)' cargo tauri build --bundles appimage --config '{"version":"$(APP_VERSION)","bundle":{"createUpdaterArtifacts":false}}' -- --locked
	@test -x '$(LINUXDEPLOY_APPIMAGE_PLUGIN)' || { printf 'Missing Tauri AppImage plugin: %s\n' '$(LINUXDEPLOY_APPIMAGE_PLUGIN)' >&2; exit 1; }
	@printf '%s  %s\n' '$(LINUXDEPLOY_APPIMAGE_PLUGIN_SHA256)' '$(LINUXDEPLOY_APPIMAGE_PLUGIN)' | sha256sum -c -
	@set -eu; \
		mapfile -t appimages < <(find '$(LINUX_APPIMAGE_DIR)' -maxdepth 1 -type f -name '*.AppImage'); \
		test "$${#appimages[@]}" -eq 1; \
		appimage="$${appimages[0]}"; appdir='$(LINUX_APPIMAGE_DIR)/VoxGolem.AppDir'; \
		bash scripts/prepare-update-appdir.sh "$$appdir" '$(LINUX_RELEASE_DIR)'; \
		rm "$$appimage"; \
		(cd '$(LINUX_APPIMAGE_DIR)' && ARCH=x86_64 LDAI_RUNTIME_FILE='$(APPIMAGE_RUNTIME)' '$(LINUXDEPLOY_APPIMAGE_PLUGIN)' --appimage-extract-and-run --appdir="$$appdir"); \
		mapfile -t rebuilt < <(find '$(LINUX_APPIMAGE_DIR)' -maxdepth 1 -type f -name '*.AppImage'); \
		test "$${#rebuilt[@]}" -eq 1 || { printf 'Expected one rebuilt AppImage, found %s.\n' "$${#rebuilt[@]}" >&2; exit 1; }; \
		mv "$${rebuilt[0]}" "$$appimage"
	@$(MAKE) --no-print-directory verify-update-bundle

verify-update-bundle:
	@set -eu; \
		mapfile -t appimages < <(find '$(LINUX_APPIMAGE_DIR)' -maxdepth 1 -type f -name '*.AppImage' | sort); \
		test "$${#appimages[@]}" -eq 1 || { printf 'Expected one AppImage, found %s.\n' "$${#appimages[@]}" >&2; exit 1; }; \
		test -x "$${appimages[0]}" || { printf 'AppImage is not executable: %s\n' "$${appimages[0]}" >&2; exit 1; }; \
		temp_dir="$$(mktemp -d)"; trap 'rm -rf "$$temp_dir"' EXIT; \
		(cd "$$temp_dir" && "$${appimages[0]}" --appimage-extract >/dev/null); \
		for provider in $(LINUX_ORT_PROVIDER_LIBS); do \
			test -x "$$temp_dir/squashfs-root/usr/lib/$$provider" || { printf 'AppImage is missing executable provider: %s\n' "$$provider" >&2; exit 1; }; \
		done; \
		mapfile -t appsink_plugins < <(find "$$temp_dir/squashfs-root/usr/lib" -type f -name 'libgstapp.so*'); \
		test "$${#appsink_plugins[@]}" -gt 0 || { printf '%s\n' 'AppImage is missing the GStreamer appsink plugin.' >&2; exit 1; }; \
		if grep -R -Fq 'GDK_BACKEND=x11' "$$temp_dir/squashfs-root/apprun-hooks"; then printf '%s\n' 'AppImage launcher forces unsupported X11.' >&2; exit 1; fi; \
		mapfile -t bundled_cuda < <(find "$$temp_dir/squashfs-root/usr/lib" -maxdepth 1 \( -name 'libcublas*.so*' -o -name 'libcudart*.so*' -o -name 'libcudnn*.so*' -o -name 'libcufft*.so*' -o -name 'libcurand*.so*' -o -name 'libnvrtc*.so*' -o -name 'libcuda*.so*' \)); \
		test "$${#bundled_cuda[@]}" -eq 0 || { printf 'AppImage unexpectedly bundles CUDA system libraries:\n%s\n' "$${bundled_cuda[*]}" >&2; exit 1; }; \
		size="$$(stat -c '%s' "$${appimages[0]}")"; test "$$size" -le '$(MAX_LINUX_APPIMAGE_BYTES)' || { printf 'AppImage exceeds size limit: %s > %s bytes.\n' "$$size" '$(MAX_LINUX_APPIMAGE_BYTES)' >&2; exit 1; }; \
		printf 'Linux updater AppImage: %s (%s bytes)\n' "$${appimages[0]}" "$$(stat -c '%s' "$${appimages[0]}")"

update-bundle-smoke: verify-update-bundle
	@set -eu; \
		appimage="$$(find '$(LINUX_APPIMAGE_DIR)' -maxdepth 1 -type f -name '*.AppImage')"; \
		smoke_pid=; smoke_log="/tmp/vox-golem-appimage-smoke.$$$$.log"; smoke_config="$$(mktemp /tmp/vox-golem-appimage-config.XXXXXX)"; \
		cleanup() { status="$$1"; if [ -n "$$smoke_pid" ] && kill -0 "$$smoke_pid" 2>/dev/null; then kill -TERM -- "-$$smoke_pid" 2>/dev/null || true; sleep 1; kill -KILL -- "-$$smoke_pid" 2>/dev/null || true; wait "$$smoke_pid" 2>/dev/null || true; fi; if [ "$$status" -ne 0 ] && [ -f "$$smoke_log" ]; then cat "$$smoke_log" >&2 || true; fi; rm -f "$$smoke_config" "$$smoke_log" || true; }; \
		trap 'status=$$?; cleanup "$$status"; exit "$$status"' EXIT; \
		setsid env VOXGOLEM_CONFIG_PATH="$$smoke_config" "$$appimage" >"$$smoke_log" 2>&1 & smoke_pid=$$!; \
		bash scripts/wait-for-appimage-startup.sh "$$smoke_pid" "$$smoke_log" 30 5

packaged-smoke: verify-dist
	@set -eu; \
		smoke_pid=; smoke_log="/tmp/vox-golem-packaged-smoke.$$$$.log"; smoke_config="$$(mktemp /tmp/vox-golem-packaged-config.XXXXXX)"; \
		cleanup() { \
			status="$$1"; \
			if [ -n "$$smoke_pid" ] && kill -0 "$$smoke_pid" 2>/dev/null; then \
				kill -TERM -- "-$$smoke_pid" 2>/dev/null || true; sleep 1; kill -KILL -- "-$$smoke_pid" 2>/dev/null || true; wait "$$smoke_pid" 2>/dev/null || true; \
			fi; \
			if [ "$$status" -ne 0 ] && [ -f "$$smoke_log" ]; then \
				printf '%s\n' 'Packaged Linux smoke diagnostics:' >&2; \
				cat "$$smoke_log" >&2 || true; \
			fi; \
			rm -f "$$smoke_config" "$$smoke_log" || true; \
		}; \
		trap 'exit 130' INT; trap 'exit 143' TERM; \
		trap 'status=$$?; cleanup "$$status"; exit "$$status"' EXIT; \
		(cd /tmp && setsid env VOXGOLEM_CONFIG_PATH="$$smoke_config" '$(LINUX_STAGED_RELEASE_DIR)/vox-golem') >"$$smoke_log" 2>&1 & smoke_pid=$$!; \
		for _ in $$(seq 1 30); do \
			sleep 1; \
			if grep -q 'VOXGOLEM_STARTUP_READY' "$$smoke_log"; then printf '%s\n' 'Packaged Linux smoke passed: startup-ready marker observed.'; exit 0; fi; \
			if ! kill -0 "$$smoke_pid" 2>/dev/null; then printf 'Packaged Linux smoke failed; output: %s\n' "$$smoke_log" >&2; exit 1; fi; \
		done; \
		printf 'Packaged Linux smoke timed out; output: %s\n' "$$smoke_log" >&2; exit 1

check-pc-tools:
	@command -v cargo-xwin >/dev/null || { printf '%s\n' 'Missing cargo-xwin. Install with: cargo install cargo-xwin --locked' >&2; exit 1; }
	@command -v cargo-tauri >/dev/null || { printf '%s\n' 'Missing Tauri CLI. Install with: cargo install tauri-cli --version 2.11.1 --locked' >&2; exit 1; }
	@bash scripts/verify-nsis-version.sh
	@command -v cmake >/dev/null || { printf '%s\n' 'Missing cmake. Install CMake before running make pc.' >&2; exit 1; }
	@command -v ninja >/dev/null || { printf '%s\n' 'Missing ninja. cargo-xwin requires Ninja for CMake crates.' >&2; exit 1; }
	@test -n '$(REAL_CLANG_CL)' || { printf '%s\n' 'Missing clang-cl or clang-19. Install LLVM/Clang 19 or newer before running make pc.' >&2; exit 1; }
	@clang_version="$$('$(REAL_CLANG_CL)' --driver-mode=cl --version | { read -r first_line; printf '%s\n' "$$first_line"; })"; \
		clang_major="$${clang_version#* version }"; \
		clang_major="$${clang_major%%.*}"; \
		case "$$clang_major" in ''|*[!0-9]*) clang_major=0 ;; esac; \
		if [ "$$clang_major" -lt 19 ]; then \
			printf 'Clang 19 or newer is required for the Windows STL used by cargo-xwin; found: %s\n' "$$clang_version" >&2; \
			exit 1; \
		fi
	@command -v llvm-rc >/dev/null || { printf '%s\n' 'Missing llvm-rc. Install LLVM tools before running make pc.' >&2; exit 1; }
	@command -v llvm-mt >/dev/null || { printf '%s\n' 'Missing llvm-mt. Install LLVM tools before running make pc.' >&2; exit 1; }
	@command -v llvm-lib >/dev/null || { printf '%s\n' 'Missing llvm-lib. Install LLVM tools before running make pc.' >&2; exit 1; }
	@command -v llvm-dlltool >/dev/null || { printf '%s\n' 'Missing llvm-dlltool. Install LLVM tools before running make pc.' >&2; exit 1; }
	@command -v lld-link >/dev/null || { printf '%s\n' 'Missing lld-link. Install lld before running make pc.' >&2; exit 1; }

pc: check-pc-tools $(ESPEAK_COMPAT_HEADER) $(STDCXX_IMPORT_LIB) $(DIRECTML_IMPORT_LIB) $(PATHCCH_IMPORT_LIB)
	bun install --frozen-lockfile
	PATH='$(WINDOWS_CROSS_BIN)':"$$PATH" \
	VOXGOLEM_REAL_CLANG_CL='$(REAL_CLANG_CL)' \
	VOXGOLEM_ESPEAK_COMPAT_HEADER='$(ESPEAK_COMPAT_HEADER)' \
	CMAKE_GENERATOR=Ninja \
	RUSTFLAGS="$${RUSTFLAGS:+$${RUSTFLAGS} }-L native=$(IMPORT_LIB_DIR)" \
	cargo tauri build --runner cargo-xwin --target $(WINDOWS_TARGET) --no-bundle -- --locked
	@printf 'Windows app: %s\n' '$(WINDOWS_RELEASE_DIR)/vox-golem.exe'

pc-dist: pc
	@rm -rf '$(WINDOWS_STAGED_RELEASE_DIR)'
	@mkdir -p '$(WINDOWS_STAGED_RELEASE_DIR)'
	cp '$(WINDOWS_RELEASE_DIR)/vox-golem.exe' '$(WINDOWS_STAGED_RELEASE_DIR)/vox-golem.exe'
	@for file in $(ORT_RUNTIME_DLLS); do \
		if [ -f '$(WINDOWS_RELEASE_DIR)'/$$file ]; then \
			cp '$(WINDOWS_RELEASE_DIR)'/$$file '$(WINDOWS_STAGED_RELEASE_DIR)'/$$file; \
		fi; \
	done
	@$(MAKE) --no-print-directory $(VC_RUNTIME_DIR)/.complete
	@for file in $(VC_RUNTIME_DLLS); do \
		cp '$(VC_RUNTIME_DIR)'/$$file '$(WINDOWS_STAGED_RELEASE_DIR)'/$$file; \
	done
	@$(MAKE) --no-print-directory verify-pc-dist
	@printf 'Staged Windows release files: %s\n' '$(WINDOWS_STAGED_RELEASE_DIR)'

pc-installer: pc-dist $(WINDOWS_NSIS_TEMPLATE)
	@test -n '$(APP_VERSION)' || { printf '%s\n' 'Failed to generate application version.' >&2; exit 1; }
	@case '$(APP_VERSION)' in *[!0-9A-Za-z.-]*|'') printf 'Invalid APP_VERSION: %s\n' '$(APP_VERSION)' >&2; exit 1;; esac
	PATH='$(WINDOWS_CROSS_BIN)':"$$PATH" \
	VOXGOLEM_REAL_CLANG_CL='$(REAL_CLANG_CL)' \
	VOXGOLEM_ESPEAK_COMPAT_HEADER='$(ESPEAK_COMPAT_HEADER)' \
	CMAKE_GENERATOR=Ninja \
	RUSTFLAGS="$${RUSTFLAGS:+$${RUSTFLAGS} }-L native=$(IMPORT_LIB_DIR)" \
	cargo tauri build --runner cargo-xwin --target $(WINDOWS_TARGET) --bundles nsis --config '{"version":"$(APP_VERSION)","bundle":{"createUpdaterArtifacts":false}}' -- --locked
	@$(MAKE) --no-print-directory verify-pc-installer

verify-pc-installer:
	@test -f '$(WINDOWS_NSIS_INSTALLER)' || { printf 'Missing Windows NSIS installer: %s\n' '$(WINDOWS_NSIS_INSTALLER)' >&2; exit 1; }
	@test "$$(find '$(WINDOWS_NSIS_DIR)' -maxdepth 1 -type f -name '*.exe' | wc -l)" -eq 1 || { printf '%s\n' 'Expected exactly one Windows NSIS installer.' >&2; exit 1; }
	@bash scripts/verify-windows-installer.sh '$(WINDOWS_NSIS_INSTALLER)' '$(MAX_WINDOWS_INSTALLER_BYTES)'
	@grep -Fq 'Call CreateOrUpdateStartMenuShortcut' '$(WINDOWS_RELEASE_DIR)/nsis/x64/installer.nsi'
	@! grep -Fq 'Call CreateOrUpdateDesktopShortcut' '$(WINDOWS_RELEASE_DIR)/nsis/x64/installer.nsi'
	@! grep -Fq 'MUI_FINISHPAGE_SHOWREADME' '$(WINDOWS_RELEASE_DIR)/nsis/x64/installer.nsi'
	@grep -Fq 'StrCpy $$INSTDIR "$$LOCALAPPDATA\Programs\VoxGolem"' '$(WINDOWS_RELEASE_DIR)/nsis/x64/installer.nsi'
	@! grep -Fq 'MUI_PAGE_DIRECTORY' '$(WINDOWS_RELEASE_DIR)/nsis/x64/installer.nsi'
	@! grep -Fq 'Call RestorePreviousInstallLocation' '$(WINDOWS_RELEASE_DIR)/nsis/x64/installer.nsi'

verify-pc-dist:
	@bash scripts/verify-windows-package.sh '$(WINDOWS_STAGED_RELEASE_DIR)'

clean:
	rm -rf '$(CURDIR)/dist' '$(CURDIR)/package' '$(CURDIR)/target' '$(CURDIR)/frontend/app/dist'

$(ESPEAK_COMPAT_HEADER):
	@mkdir -p '$(CROSS_SHIM_DIR)'
	@printf '%s\n' \
		'#include <io.h>' \
		'#include <fcntl.h>' \
		'#ifdef _WIN32' \
		'#pragma comment(lib, "advapi32")' \
		'#endif' > '$@'

$(TAURI_NSIS_TEMPLATE_SOURCE):
	@command -v curl >/dev/null || { printf '%s\n' 'Missing curl. Install curl before preparing the NSIS template.' >&2; exit 1; }
	@mkdir -p '$(@D)'
	@curl --retry 5 --retry-delay 2 --retry-all-errors -fsSL '$(TAURI_NSIS_TEMPLATE_URL)' -o '$@'
	@printf '%s  %s\n' '$(TAURI_NSIS_TEMPLATE_SHA256)' '$@' | sha256sum -c -

$(WINDOWS_NSIS_TEMPLATE): $(TAURI_NSIS_TEMPLATE_SOURCE) scripts/customize-windows-nsis-template.py
	@printf '%s  %s\n' '$(TAURI_NSIS_TEMPLATE_SHA256)' '$(TAURI_NSIS_TEMPLATE_SOURCE)' | sha256sum -c -
	@python3 scripts/customize-windows-nsis-template.py '$(TAURI_NSIS_TEMPLATE_SOURCE)' '$@'

$(STDCXX_IMPORT_LIB):
	@mkdir -p '$(IMPORT_LIB_DIR)'
	llvm-lib /llvmlibempty /out:'$@'

$(DIRECTML_IMPORT_LIB):
	@mkdir -p '$(IMPORT_LIB_DIR)'
	@printf '%s\n' \
		'LIBRARY DirectML.dll' \
		'EXPORTS' \
		'DMLCreateDevice' \
		'DMLCreateDevice1' > '$(IMPORT_LIB_DIR)/DirectML.def'
	llvm-dlltool -m i386:x86-64 -d '$(IMPORT_LIB_DIR)/DirectML.def' -l '$@'

$(PATHCCH_IMPORT_LIB):
	@mkdir -p '$(IMPORT_LIB_DIR)'
	@printf '%s\n' \
		'LIBRARY PathCch.dll' \
		'EXPORTS' \
		'PathAllocCanonicalize' \
		'PathAllocCombine' \
		'PathCchAddBackslash' \
		'PathCchAddBackslashEx' \
		'PathCchAddExtension' \
		'PathCchAppend' \
		'PathCchAppendEx' \
		'PathCchCanonicalize' \
		'PathCchCanonicalizeEx' \
		'PathCchCombine' \
		'PathCchCombineEx' \
		'PathCchFindExtension' \
		'PathCchIsRoot' \
		'PathCchRemoveBackslash' \
		'PathCchRemoveBackslashEx' \
		'PathCchRemoveExtension' \
		'PathCchRemoveFileSpec' \
		'PathCchRenameExtension' \
		'PathCchSkipRoot' \
		'PathCchStripPrefix' \
		'PathCchStripToRoot' \
		'PathIsUNCEx' > '$(IMPORT_LIB_DIR)/PathCch.def'
	llvm-dlltool -m i386:x86-64 -d '$(IMPORT_LIB_DIR)/PathCch.def' -l '$@'

$(VC_RUNTIME_DIR)/.complete:
	@command -v curl >/dev/null || { printf '%s\n' 'Missing curl. Install curl before staging the VC runtime.' >&2; exit 1; }
	@command -v cabextract >/dev/null || { printf '%s\n' 'Missing cabextract. Install cabextract before staging the VC runtime.' >&2; exit 1; }
	@mkdir -p '$(CURDIR)/.deps/downloads' '$(VC_RUNTIME_ROOT)'
	@if [ ! -f '$(VC_REDIST_ARCHIVE)' ]; then \
		curl --retry 5 --retry-delay 2 --retry-all-errors -fsSL '$(VC_REDIST_URL)' -o '$(VC_REDIST_ARCHIVE)'; \
	fi
	@printf '%s  %s\n' '$(VC_REDIST_SHA256)' '$(VC_REDIST_ARCHIVE)' | sha256sum -c -
	@rm -rf '$(VC_RUNTIME_ROOT)/cabs' '$(VC_RUNTIME_ROOT)/extract' '$(VC_RUNTIME_DIR)'
	@mkdir -p '$(VC_RUNTIME_ROOT)/cabs' '$(VC_RUNTIME_ROOT)/extract' '$(VC_RUNTIME_DIR)'
	@cabextract -q -d '$(VC_RUNTIME_ROOT)/cabs' -F a4 '$(VC_REDIST_ARCHIVE)'
	@cabextract -q -d '$(VC_RUNTIME_ROOT)/extract' '$(VC_RUNTIME_ROOT)/cabs/a4'
	@install -m 0644 '$(VC_RUNTIME_ROOT)/extract/msvcp140.dll_amd64' '$(VC_RUNTIME_DIR)/MSVCP140.dll'
	@install -m 0644 '$(VC_RUNTIME_ROOT)/extract/msvcp140_1.dll_amd64' '$(VC_RUNTIME_DIR)/MSVCP140_1.dll'
	@install -m 0644 '$(VC_RUNTIME_ROOT)/extract/vcruntime140.dll_amd64' '$(VC_RUNTIME_DIR)/VCRUNTIME140.dll'
	@install -m 0644 '$(VC_RUNTIME_ROOT)/extract/vcruntime140_1.dll_amd64' '$(VC_RUNTIME_DIR)/VCRUNTIME140_1.dll'
	@printf '%s  %s\n' '7c26614e1d733892c2deac7e245ce115504b1d80592dd0a01b08e3e5a55f89ca' '$(VC_RUNTIME_DIR)/MSVCP140.dll' | sha256sum -c -
	@printf '%s  %s\n' '206c931bf90fdad8816de3b5e2ef80b2bcaa9406c89ecc05fe6fddffe251e982' '$(VC_RUNTIME_DIR)/MSVCP140_1.dll' | sha256sum -c -
	@printf '%s  %s\n' 'd1f4225df2cd877dbf130d5668a021dce3f94118455ff5ec952061c30afc9ce7' '$(VC_RUNTIME_DIR)/VCRUNTIME140.dll' | sha256sum -c -
	@printf '%s  %s\n' 'a7146c08f89fe5b04541ab507cdb59ff7b44534d4ba3c668a426c6450a03434e' '$(VC_RUNTIME_DIR)/VCRUNTIME140_1.dll' | sha256sum -c -
	@touch '$@'
