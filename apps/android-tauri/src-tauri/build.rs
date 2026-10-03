fn main() {
    let attributes =
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(&[
            "get_app_snapshot",
            "submit_prompt",
            "cancel_prompt",
            "reset_session",
            "set_preferences",
        ]));
    tauri_build::try_build(attributes).expect("failed to build Tauri application");
}
