fn main() {
    tauri_plugin::Builder::new(&[
        "snapshot",
        "enable_capture",
        "list_captures",
        "select_notes_folder",
        "play_capture",
        "stop_playback",
        "retry_capture",
        "finalize_capture",
        "update_transcript",
    ])
    .android_path("android")
    .build();
}
