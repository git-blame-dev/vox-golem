# Android Tauri Shell

This is the first minimal Android-only Tauri slice. It owns only process-local conversation state and the canonical IPC commands; providers, models, audio, persistence, and desktop plugins are deliberately outside this crate. A zero-asset install starts and reports unavailable capabilities.

`src-tauri/gen/android` is generated output and must never be hand-authored. Regenerate it only with the root `make android-init` wrapper, which pins and verifies Tauri CLI 2.11.1, refreshes the tracked `src-tauri/icons/icon.png` source from the desktop product icon, and regenerates Android launcher icons. Build the arm64 debug APK with `make android-build`.

The generated baseline is intentionally kept separate from authored behavior. The only manually authored files under `src-tauri/gen/android` are `app/src/main/AndroidManifest.xml`, `app/src/main/res/xml/backup_rules.xml`, and `app/src/main/res/xml/data_extraction_rules.xml`; launcher, theme, string, drawable, and mipmap resources are generator-owned outputs. Other cross-platform files emitted beside `src-tauri/icons/icon.png` are ignored. Machine-local SDK files, Gradle state, and build products remain ignored.

Prerequisites are Bun, Rust/Cargo, Android SDK/NDK tooling supported by Tauri, and the pinned `tauri-cli 2.11.1`. Run `make android-init` from the repository root after those prerequisites are installed.
