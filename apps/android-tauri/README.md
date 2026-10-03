# Android Tauri Shell

This Android-only Tauri shell owns process-local conversation state, canonical IPC commands, provider orchestration, lifecycle cancellation, and app-private credential path resolution. The reusable Custom transport remains in `voxgolem-platform`; audio, local models, persistence, and desktop plugins stay outside this crate. A zero-asset install still starts and reports only the affected provider capability as unavailable.

`src-tauri/gen/android` is generated output and must never be hand-authored. Regenerate it only with the root `make android-init` wrapper, which pins and verifies Tauri CLI 2.11.1, refreshes the tracked `src-tauri/icons/icon.png` source from the desktop product icon, and regenerates Android launcher icons. Build the arm64 debug APK with `make android-build`.

The generated baseline is intentionally kept separate from authored behavior. The only manually authored files under `src-tauri/gen/android` are `app/src/main/AndroidManifest.xml`, `app/src/main/res/xml/backup_rules.xml`, and `app/src/main/res/xml/data_extraction_rules.xml`; launcher, theme, string, drawable, and mipmap resources are generator-owned outputs. Other cross-platform files emitted beside `src-tauri/icons/icon.png` are ignored. Machine-local SDK files, Gradle state, and build products remain ignored.

Prerequisites are Bun, Rust/Cargo, Android SDK/NDK tooling supported by Tauri, and the pinned `tauri-cli 2.11.1`. Run `make android-init` from the repository root after those prerequisites are installed.

The app performs no login or token refresh. For a connected, authorized debug device, provision an existing compatible OpenCode auth record directly into app-private storage with `make android-provision-auth DEVICE=<adb-serial> AUTH_PATH=<file>`. The command uses a fixed remote path, streams through stdin, atomically replaces the file with mode `0600`, verifies the transfer length, and never prints credential content.
