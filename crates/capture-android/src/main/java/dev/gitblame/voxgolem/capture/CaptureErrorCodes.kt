package dev.gitblame.voxgolem.capture

/** Explicit public diagnostic vocabulary. Old persisted or peer-supplied exception text is not a code. */
object CaptureErrorCodes {
    fun safe(code: String?, fallback: String = "capture-operation-failed"): String =
        code?.takeIf { it in fixed || it in speechErrors } ?: fallback

    private val speechErrors = (1..15).map { "speech-recognizer-$it" }.toSet()
    private val fixed = setOf(
        "capture-operation-failed", "capture-origin-not-authorized", "archive-failed", "org-export-failed", "wav-finalize-failed", "wav-finalization-failed",
        "notes-folder-not-selected", "staged-recording-missing", "saved-wav-unavailable", "invalid-spool-path", "interrupted",
        "insufficient-space", "spool-create-failed", "permission-revoked", "unavailable", "initialization-failed", "start-failed", "setup-failed",
        "limit-reached-review", "cannot-preserve-invalid-wav", "wav-finalization-retry-limit", "recovery-metadata-commit-failed",
        "capture-format-conflict", "saved-audio-transcription-failed", "external-audio-provider-rejected", "external-audio-write-failed",
        "external-audio-provider-unavailable", "external-audio-provider-backpressure", "external-audio-rejected",
        "speech-provider-ended-before-source", "speech-recognizer-retirement-failed", "speech-recognizer-timeout",
        "speech-adapter-initialization-timeout", "phone-runtime-stopped", "final-transcript-persistence-failed",
        "connected-device-service-not-ready", "transcript-not-sent-exceeds-message-limit", "final-transcript-exceeds-result-limit",
        "no-speech-detected", "transcription-unavailable", "phone-unavailable", "multiple-phone-receivers",
        "pending-archive-processing-failed", "archive-input-failed",
        "live-input-failed", "live-input-timeout", "archive-input-timeout", "live-reader-unavailable", "archive-reader-unavailable",
        "live-input-unavailable", "archive-input-unavailable", "live-runtime-destroyed", "archive-runtime-destroyed",
        "record-audio-permission-required", "microphone-permission-required", "bluetooth-connect-permission-required",
        "post-notifications-permission-required", "wear-capability-unavailable", "connected-device-service-start-denied",
        "connected-device-service-permission-denied",
        "phone-capture-not-enabled", "recognition-busy", "capture-policy-conflict", "capture-language-unavailable",
        "live-stream-failed", "live-queue-overflow", "live-finish-timeout", "live-input-ownership-rejected",
        "export-destination-changed-during-pending-write", "legacy-export-metadata-commit-failed",
        "export-journal-target-changed-requires-review", "transcript-changed-during-export", "inbox-creation-requires-review",
        "created-inbox-identity-is-ambiguous", "inbox-changed-after-export-preparation", "pending-inbox-creation-requires-review",
        "inbox-changed-before-write", "saf-permission-revoked-during-write", "saf-permission-revoked", "provider-failure-during-write",
        "provider-failure-before-write", "capture-disappeared", "capture-changed-before-inbox-write",
        "inbox-preimage-no-longer-matches", "inbox-readback-does-not-match-postimage", "transcript-changed-after-inbox-write",
        "transcript-version-changed", "multiple-inbox-documents-require-review", "ai-inbox.org-is-a-folder-not-a-file",
        "journaled-inbox-is-not-the-unique-canonical-target", "provider-cannot-name-inbox-exactly-rename-to-ai-inbox.org-or-choose-another-folder",
    )
}
