fn main() {
    let attributes =
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(&[
            "get_app_snapshot",
            "report_startup_stage",
            "submit_prompt",
            "cancel_prompt",
            "reset_session",
            "set_preferences",
            "set_search_mode",
            "select_conversation",
            "select_conversation_head",
            "save_assistant_draft",
            "list_assistant_conversations",
            "list_assistant_heads",
            "assistant_voice_status",
            "open_speech_services_store",
            "start_assistant_voice",
            "finish_assistant_dictation",
            "stop_assistant_voice",
            "assistant_pairing_status",
            "new_voice_shortcut_status",
            "pin_new_voice_shortcut",
            "consume_new_voice_shortcut",
            "stop_send_new_voice_shortcut",
            "release_new_voice_shortcut",
            "prepare_new_voice_shortcut",
        ]));
    tauri_build::try_build(attributes).expect("failed to build Tauri application");
}
