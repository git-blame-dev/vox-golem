.SUFFIXES:

SHELL := /bin/bash
.SHELLFLAGS := -eu -o pipefail -c

WINDOWS_TARGET := x86_64-pc-windows-msvc
TARGET_RELEASE_DIR := $(CURDIR)/target/$(WINDOWS_TARGET)/release
STAGED_RELEASE_DIR := $(CURDIR)/dist/VoxGolem
IMPORT_LIB_DIR := $(CURDIR)/target/import-libs
CROSS_SHIM_DIR := $(CURDIR)/target/cross-shims
ESPEAK_COMPAT_HEADER := $(CROSS_SHIM_DIR)/espeak_windows_compat.h
STDCXX_IMPORT_LIB := $(IMPORT_LIB_DIR)/stdc++.lib
DIRECTML_IMPORT_LIB := $(IMPORT_LIB_DIR)/DirectML.lib
PATHCCH_IMPORT_LIB := $(IMPORT_LIB_DIR)/PathCch.lib
CUDA_RUNTIME_FLAVOR ?= onnxruntime-1.24.2-cu12-cudnn9
CUDA_RUNTIME_DIR := $(CURDIR)/.deps/cuda-runtime/$(CUDA_RUNTIME_FLAVOR)/bin

ORT_RUNTIME_DLLS := \
	DirectML.dll \
	onnxruntime_providers_cuda.dll \
	onnxruntime_providers_nv_tensorrt_rtx.dll \
	onnxruntime_providers_shared.dll \
	onnxruntime_providers_tensorrt.dll

CUDA_RUNTIME_DLLS := \
	cublas64_12.dll \
	cublasLt64_12.dll \
	cudart64_12.dll \
	cudnn64_9.dll \
	cudnn_adv64_9.dll \
	cudnn_cnn64_9.dll \
	cudnn_engines_precompiled64_9.dll \
	cudnn_engines_runtime_compiled64_9.dll \
	cudnn_graph64_9.dll \
	cudnn_heuristic64_9.dll \
	cudnn_ops64_9.dll \
	cufft64_11.dll

REQUIRED_DIST_FILES := \
	config.toml \
	vox-golem.exe \
	onnxruntime_providers_cuda.dll \
	onnxruntime_providers_shared.dll \
	cublas64_12.dll \
	cublasLt64_12.dll \
	cudart64_12.dll \
	cudnn64_9.dll \
	cudnn_adv64_9.dll \
	cudnn_cnn64_9.dll \
	cudnn_engines_precompiled64_9.dll \
	cudnn_engines_runtime_compiled64_9.dll \
	cudnn_graph64_9.dll \
	cudnn_heuristic64_9.dll \
	cudnn_ops64_9.dll \
	cufft64_11.dll

.DEFAULT_GOAL := help

.PHONY: help test check-pc-tools pc pc-dist dist verify-pc-dist app-dev clean

help:
	@printf '%s\n' 'Targets:'
	@printf '%s\n' '  make test     Run deterministic Linux frontend and Rust checks'
	@printf '%s\n' '  make pc       Cross-build the Windows Tauri app from Linux'
	@printf '%s\n' '  make pc-dist  Build and stage Windows files under dist/VoxGolem'
	@printf '%s\n' '  make dist     Build and stage all release files under dist/'
	@printf '%s\n' '  make app-dev  Run the frontend development server'
	@printf '%s\n' '  make clean    Remove generated build and staging output'

test:
	bun install --frozen-lockfile
	bun run typecheck
	bun run lint
	bun run test
	bun run build
	cargo fmt --check
	cargo clippy -p voxgolem-audio -p voxgolem-core -p voxgolem-model -p voxgolem-platform --all-targets --all-features -- -D warnings
	cargo test -p voxgolem-audio -p voxgolem-core -p voxgolem-model -p voxgolem-platform

check-pc-tools:
	@command -v cargo-xwin >/dev/null || { printf '%s\n' 'Missing cargo-xwin. Install with: cargo install cargo-xwin --locked' >&2; exit 1; }
	@command -v cargo-tauri >/dev/null || { printf '%s\n' 'Missing Tauri CLI. Install with: cargo install tauri-cli --version 2.11.1 --locked' >&2; exit 1; }
	@command -v cmake >/dev/null || { printf '%s\n' 'Missing cmake. Install CMake before running make pc.' >&2; exit 1; }
	@command -v ninja >/dev/null || { printf '%s\n' 'Missing ninja. cargo-xwin requires Ninja for CMake crates.' >&2; exit 1; }
	@command -v clang-cl >/dev/null || { printf '%s\n' 'Missing clang-cl. Install LLVM/Clang 19 or newer before running make pc.' >&2; exit 1; }
	@clang_version="$$(clang-cl --version | { read -r first_line; printf '%s\n' "$$first_line"; })"; \
		clang_major="$${clang_version#* version }"; \
		clang_major="$${clang_major%%.*}"; \
		case "$$clang_major" in ''|*[!0-9]*) clang_major=0 ;; esac; \
		if [ "$$clang_major" -lt 19 ]; then \
			printf 'clang-cl 19 or newer is required for the Windows STL used by cargo-xwin; found: %s\n' "$$clang_version" >&2; \
			exit 1; \
		fi
	@command -v llvm-rc >/dev/null || { printf '%s\n' 'Missing llvm-rc. Install LLVM tools before running make pc.' >&2; exit 1; }
	@command -v llvm-mt >/dev/null || { printf '%s\n' 'Missing llvm-mt. Install LLVM tools before running make pc.' >&2; exit 1; }
	@command -v llvm-lib >/dev/null || { printf '%s\n' 'Missing llvm-lib. Install LLVM tools before running make pc.' >&2; exit 1; }
	@command -v llvm-dlltool >/dev/null || { printf '%s\n' 'Missing llvm-dlltool. Install LLVM tools before running make pc.' >&2; exit 1; }
	@command -v lld-link >/dev/null || { printf '%s\n' 'Missing lld-link. Install lld before running make pc.' >&2; exit 1; }

pc: check-pc-tools $(ESPEAK_COMPAT_HEADER) $(STDCXX_IMPORT_LIB) $(DIRECTML_IMPORT_LIB) $(PATHCCH_IMPORT_LIB)
	bun install --frozen-lockfile
	CMAKE_GENERATOR=Ninja \
	TARGET_CFLAGS="$${TARGET_CFLAGS:+$${TARGET_CFLAGS} }/FI$(ESPEAK_COMPAT_HEADER)" \
	TARGET_CXXFLAGS="$${TARGET_CXXFLAGS:+$${TARGET_CXXFLAGS} }/FI$(ESPEAK_COMPAT_HEADER)" \
	RUSTFLAGS="$${RUSTFLAGS:+$${RUSTFLAGS} }-L native=$(IMPORT_LIB_DIR)" \
	cargo tauri build --runner cargo-xwin --target $(WINDOWS_TARGET) --no-bundle
	@printf 'Windows app: %s\n' '$(TARGET_RELEASE_DIR)/vox-golem.exe'

pc-dist: pc
	@rm -rf '$(STAGED_RELEASE_DIR)'
	@mkdir -p '$(STAGED_RELEASE_DIR)'
	cp '$(CURDIR)/config.example.toml' '$(STAGED_RELEASE_DIR)/config.toml'
	cp '$(TARGET_RELEASE_DIR)/vox-golem.exe' '$(STAGED_RELEASE_DIR)/vox-golem.exe'
	@for file in $(ORT_RUNTIME_DLLS); do \
		if [ -f '$(TARGET_RELEASE_DIR)'/$$file ]; then \
			cp '$(TARGET_RELEASE_DIR)'/$$file '$(STAGED_RELEASE_DIR)'/$$file; \
		fi; \
	done
	@$(MAKE) --no-print-directory $(CUDA_RUNTIME_DIR)/.complete
	@for file in $(CUDA_RUNTIME_DLLS); do \
		cp '$(CUDA_RUNTIME_DIR)'/$$file '$(STAGED_RELEASE_DIR)'/$$file; \
	done
	@$(MAKE) --no-print-directory verify-pc-dist
	@printf 'Staged Windows release files: %s\n' '$(STAGED_RELEASE_DIR)'

dist: pc-dist

verify-pc-dist:
	@missing=0; \
	for file in $(REQUIRED_DIST_FILES); do \
		if [ ! -f '$(STAGED_RELEASE_DIR)'/$$file ]; then \
			printf 'Missing staged release file: %s\n' "$$file" >&2; \
			missing=1; \
		fi; \
	done; \
	if [ "$$missing" -ne 0 ]; then exit 1; fi
	@find '$(STAGED_RELEASE_DIR)' -maxdepth 1 -type f -printf '%f\t%s bytes\n' | sort

app-dev:
	bun run dev

clean:
	rm -rf '$(CURDIR)/dist' '$(CURDIR)/target' '$(CURDIR)/frontend/app/dist'

$(ESPEAK_COMPAT_HEADER):
	@mkdir -p '$(CROSS_SHIM_DIR)'
	@printf '%s\n' \
		'#include <io.h>' \
		'#include <fcntl.h>' \
		'#ifdef _WIN32' \
		'#pragma comment(lib, "advapi32")' \
		'#endif' > '$@'

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

$(CUDA_RUNTIME_DIR)/.complete:
	@command -v curl >/dev/null || { printf '%s\n' 'Missing curl. Install curl before staging runtime DLLs.' >&2; exit 1; }
	@command -v unzip >/dev/null || { printf '%s\n' 'Missing unzip. Install unzip before staging runtime DLLs.' >&2; exit 1; }
	@mkdir -p '$(CUDA_RUNTIME_DIR)' '$(CURDIR)/.deps/downloads' '$(CURDIR)/.deps/extract'
	@complete=1; \
	for file in $(CUDA_RUNTIME_DLLS); do \
		if [ ! -f '$(CUDA_RUNTIME_DIR)'/$$file ]; then complete=0; fi; \
	done; \
	if [ "$$complete" -eq 1 ]; then touch '$@'; exit 0; fi; \
	rm -rf '$(CURDIR)/.deps/extract'/* '$(CUDA_RUNTIME_DIR)'/*; \
	download_and_extract() { \
		local url="$$1"; \
		local sha256="$$2"; \
		shift 2; \
		local archive='$(CURDIR)/.deps/downloads/'"$${url##*/}"; \
		local extract_dir='$(CURDIR)/.deps/extract/'"$${url##*/}"; \
		curl --retry 5 --retry-delay 2 --retry-all-errors -fsSL "$$url" -o "$$archive"; \
		printf '%s  %s\n' "$$sha256" "$$archive" | sha256sum -c -; \
		rm -rf "$$extract_dir"; \
		mkdir -p "$$extract_dir"; \
		unzip -q "$$archive" -d "$$extract_dir"; \
		for pattern in "$$@"; do \
			mapfile -t matches < <(find "$$extract_dir" -type f -name "$$pattern" | sort); \
			if [ "$${#matches[@]}" -eq 0 ]; then \
				printf 'No DLLs matched %s in %s\n' "$$pattern" "$$archive" >&2; \
				exit 1; \
			fi; \
			for match in "$${matches[@]}"; do \
				cp "$$match" '$(CUDA_RUNTIME_DIR)'/; \
			done; \
		done; \
	}; \
	download_and_extract 'https://developer.download.nvidia.com/compute/cuda/redist/cuda_cudart/windows-x86_64/cuda_cudart-windows-x86_64-12.9.79-archive.zip' '179e9c43b0735ffe67207b3da556eb5a0c50f3047961882b7657d3b822d34ef8' 'cudart64_12.dll'; \
	download_and_extract 'https://developer.download.nvidia.com/compute/cuda/redist/libcublas/windows-x86_64/libcublas-windows-x86_64-12.9.1.4-archive.zip' 'd534d98b0b453a98914dbf3adf47d7e84b55037abf02f87466439e1dcef581ed' 'cublas64_12.dll' 'cublasLt64_12.dll'; \
	download_and_extract 'https://developer.download.nvidia.com/compute/cuda/redist/libcufft/windows-x86_64/libcufft-windows-x86_64-11.4.1.4-archive.zip' 'f26f80bb9abff3269c548e1559e8c2b4ba58ccb8acc6095bbc6404fc962d4b80' 'cufft64_11.dll'; \
	download_and_extract 'https://developer.download.nvidia.com/compute/cudnn/redist/cudnn/windows-x86_64/cudnn-windows-x86_64-9.10.0.56_cuda12-archive.zip' '214f21e4a6fdc7121b2463f55937ebf097ab7c96232c3d575b39b9b030503fca' 'cudnn*.dll'; \
	missing=0; \
	for file in $(CUDA_RUNTIME_DLLS); do \
		if [ ! -f '$(CUDA_RUNTIME_DIR)'/$$file ]; then \
			printf 'Missing CUDA runtime DLL after extraction: %s\n' "$$file" >&2; \
			missing=1; \
		fi; \
	done; \
	if [ "$$missing" -ne 0 ]; then exit 1; fi; \
	touch '$@'
