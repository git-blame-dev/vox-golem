package dev.gitblame.voxgolem.capture

import app.tauri.plugin.JSArray
import app.tauri.plugin.JSObject
import org.json.JSONObject
import java.time.Duration
import java.time.Instant

internal data class NativeProjectionStatus(
    val consentEnabled: Boolean,
    val foregroundReady: Boolean,
    val runtimeError: String?,
    val permissionError: String?,
) {
    fun onboarding(): PhoneCaptureCoordinator.EnableResult = when {
        runtimeError != null -> PhoneCaptureCoordinator.EnableResult("failed", CaptureErrorCodes.safe(runtimeError))
        !consentEnabled -> PhoneCaptureCoordinator.EnableResult("setup-required")
        permissionError != null -> PhoneCaptureCoordinator.EnableResult("permission-required", permissionError)
        foregroundReady -> PhoneCaptureCoordinator.EnableResult("enabled")
        else -> PhoneCaptureCoordinator.EnableResult("starting")
    }
}

/** Public bridge allowlist, deliberately separate from canonical persistence JSON. */
internal object NativeCaptureProjection {
    fun snapshot(destination: NotesDestination?, records: List<CaptureRecord>, status: NativeProjectionStatus,
                 enableResult: PhoneCaptureCoordinator.EnableResult? = null): JSObject = JSObject().apply {
        val observed = status.onboarding()
        val onboarding = if (enableResult == null || enableResult.state == "starting") observed else enableResult
        put("available", true)
        put("notesFolderUri", destination?.treeUri?.toString() ?: JSONObject.NULL)
        put("captures", JSArray(records.map(::record)))
        put("error", when (status.runtimeError) {
            "wear-capability-unavailable" -> "Wear OS connection is unavailable. Connect the watch in its companion app, then retry setup."
            null -> JSONObject.NULL
            else -> CaptureErrorCodes.safe(status.runtimeError)
        })
        put("setupRequired", onboarding.state != "enabled")
        put("onboardingState", onboarding.state)
        put("onboardingError", onboarding.error ?: JSONObject.NULL)
    }

    fun record(record: CaptureRecord): JSObject = JSObject().apply {
        val started = record.startedAtUtc?.let { runCatching { Instant.parse(it) }.getOrNull() }
        val finished = record.finishedAtUtc?.let { runCatching { Instant.parse(it) }.getOrNull() }
        val duration = if (started != null && finished != null) Duration.between(started, finished).toMillis().coerceAtLeast(0)
            else record.durableFrames * 1000 / record.format.sampleRate
        put("captureId", record.captureId)
        put("purpose", record.purpose.name)
        put("requestedLanguage", record.requestedLanguage ?: JSONObject.NULL)
        put("speechNetworkFallback", record.speechNetworkFallback)
        put("liveTranscriptionState", record.liveTranscriptionState.name)
        put("liveStatusRevision", record.liveStatusRevision)
        put("canTranscribeLive", record.canTranscribeLive)
        put("liveErrorCode", record.liveErrorCode?.let { CaptureErrorCodes.safe(it) } ?: JSONObject.NULL)
        put("createdAtUtc", record.createdAtUtc)
        put("finishedAtUtc", record.finishedAtUtc ?: JSONObject.NULL)
        put("durationMs", duration)
        put("captureState", record.captureState.name)
        put("recognitionState", record.recognitionState.name)
        put("transferState", record.transferState.name)
        put("archiveState", record.archiveState.name)
        put("orgState", record.orgState.name)
        put("previewText", record.previewText ?: JSONObject.NULL)
        put("finalText", record.finalText ?: JSONObject.NULL)
        put("archivedAudioUri", record.archivedAudioUri ?: JSONObject.NULL)
        put("error", if (record.lastErrorCode != null || record.lastErrorStage != null) CaptureErrorCodes.safe(record.lastErrorCode) else JSONObject.NULL)
        put("transcriptVersion", record.transcriptVersion)
    }
}
