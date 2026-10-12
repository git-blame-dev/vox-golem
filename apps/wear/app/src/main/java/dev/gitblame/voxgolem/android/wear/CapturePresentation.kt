package dev.gitblame.voxgolem.android.wear

import android.os.Bundle
import android.os.SystemClock
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import dev.gitblame.voxgolem.android.wear.capture.CaptureRepository
import dev.gitblame.voxgolem.android.wear.capture.CaptureSnapshot
import dev.gitblame.voxgolem.capture.CaptureRecord
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import dev.gitblame.voxgolem.capture.CaptureState
import dev.gitblame.voxgolem.capture.LiveTranscriptionState

private val quietCaptureRecognitionCodes = setOf("speech-recognizer-7", "no-speech-detected")
private val captureRecognitionMessagePrefixes = listOf("Audio retained · ", "Audio saved · transcription pending · ")

internal fun captureHasQuietRecognitionWarning(message: String): Boolean =
    message in quietCaptureRecognitionCodes || captureRecognitionMessagePrefixes.any { prefix ->
        message.startsWith(prefix) && message.removePrefix(prefix) in quietCaptureRecognitionCodes
    }

internal fun captureNoteErrorText(code: String?): String? =
    code?.takeUnless { it in quietCaptureRecognitionCodes }?.let { "Audio retained · $it" }

internal fun captureNoteFeedbackText(message: String): String =
    if (captureHasQuietRecognitionWarning(message)) "Transcription ended · audio retained" else message

internal fun captureLiveStatusText(record: CaptureRecord): String {
    val status = when (record.liveTranscriptionState) {
        LiveTranscriptionState.NOT_REQUESTED -> "Transcription pending on phone"
        LiveTranscriptionState.CONNECTING -> "Connecting phone for transcription…"
        LiveTranscriptionState.PENDING -> "Phone transcription pending"
        LiveTranscriptionState.ATTEMPTING -> "Starting phone recognizer…"
        LiveTranscriptionState.PROVIDER_READY -> "Phone recognizer ready · waiting for words"
        LiveTranscriptionState.TRANSCRIBING -> "Transcribing on phone"
        LiveTranscriptionState.UNAVAILABLE -> "Live transcription unavailable · audio retained"
        LiveTranscriptionState.COMPLETED -> "Transcription saved"
    }
    val reason = record.liveErrorCode?.let { dev.gitblame.voxgolem.capture.CaptureErrorCodes.safe(it) }
    return listOfNotNull("Recording", status, reason).joinToString(" · ")
}

internal data class CapturePreparationReceipt(val launchGeneration: Long)

internal class CapturePresentation(
    private val scope: CoroutineScope,
    private val readActive: () -> CaptureRecord?,
    private val readRecord: (String) -> CaptureRecord?,
    private val readRecent: () -> List<CaptureRecord>,
    private val onChanged: () -> Unit,
    private val onSavedExit: () -> Unit,
    private val io: CoroutineDispatcher = Dispatchers.IO,
) {
    constructor(scope: CoroutineScope, repository: CaptureRepository, onChanged: () -> Unit, onSavedExit: () -> Unit) :
        this(scope, repository::active, repository::get, { repository.records(limit = 12) }, onChanged, onSavedExit)

    var snapshot by mutableStateOf(CaptureSnapshot(null, emptyList()))
        private set
    var feedback by mutableStateOf("")
    var preparationReceipt by mutableStateOf<CapturePreparationReceipt?>(null)
        private set
    var observationError by mutableStateOf<String?>(null)
        private set
    var resumed by mutableStateOf(false)
        private set
    var pendingCaptureStart = false
    var observedCaptureId: String? = null
        private set
    var microphoneReadyCaptureId by mutableStateOf<String?>(null)
        private set
    var stopRequestedCaptureId by mutableStateOf<String?>(null)
        private set
    var savedFeedbackCaptureId by mutableStateOf<String?>(null)
        private set
    var savedFeedbackText by mutableStateOf("Audio saved · transcription pending on phone")
        private set
    var savedExitDeadlineMs by mutableStateOf<Long?>(null)
        private set
    var terminalSaveFailureCaptureId by mutableStateOf<String?>(null)
        private set
    private var savedFeedbackDeadlineMs = 0L
    private var savedFeedbackJob: Job? = null
    private var refreshJob: Job? = null
    private var generation = 0L
    private var dirty = false
    private var reloadRecent = false
    private var closed = false
    private var readsEnabled = true
    private var calibrationActive = false
    private var readySignalId: String? = null
    private var savedSignalId: String? = null
    private var failedCaptureId: String? = null
    private var stopBeforeIdentity = false
    internal val refreshing: Boolean get() = refreshJob?.isActive == true

    val backgroundListening: Boolean get() = snapshot.active?.let {
        resumed && it.captureState == CaptureState.RECORDING && microphoneReadyCaptureId == it.captureId &&
            stopRequestedCaptureId != it.captureId
    } == true
    val stopEnabled: Boolean get() = snapshot.active?.let {
        it.captureState in setOf(CaptureState.PREPARING, CaptureState.RECORDING) && stopRequestedCaptureId != it.captureId
    } == true
    val terminalSaveFailure: Boolean get() = terminalSaveFailureCaptureId != null && terminalSaveFailureCaptureId == snapshot.active?.captureId

    fun restore(state: Bundle?) {
        observedCaptureId = state?.getString("observedCaptureId")
        microphoneReadyCaptureId = state?.getString("microphoneReadyCaptureId")
        stopRequestedCaptureId = state?.getString("stopRequestedCaptureId")
        stopBeforeIdentity = state?.getBoolean("stopBeforeIdentity") ?: false
        savedFeedbackCaptureId = state?.getString("savedFeedbackCaptureId")
        savedFeedbackDeadlineMs = state?.getLong("savedFeedbackDeadlineMs") ?: 0L
        preparationReceipt = if (state?.getBoolean("captureStartIssued") == true &&
            state.containsKey("capturePreparationGeneration") && state.containsKey("launchGeneration") &&
            state.getLong("capturePreparationGeneration") == state.getLong("launchGeneration") &&
            !stopBeforeIdentity && stopRequestedCaptureId == null && savedFeedbackCaptureId == null &&
            observedCaptureId == null) CapturePreparationReceipt(state.getLong("capturePreparationGeneration")) else null
        if (preparationReceipt != null) feedback = "Preparing microphone…"
    }
    fun save(state: Bundle) {
        state.putString("observedCaptureId", observedCaptureId)
        state.putString("microphoneReadyCaptureId", microphoneReadyCaptureId)
        state.putString("stopRequestedCaptureId", stopRequestedCaptureId)
        state.putBoolean("stopBeforeIdentity", stopBeforeIdentity)
        state.putString("savedFeedbackCaptureId", savedFeedbackCaptureId)
        state.putLong("savedFeedbackDeadlineMs", savedFeedbackDeadlineMs)
        state.remove("capturePreparationGeneration")
        preparationReceipt?.let { state.putLong("capturePreparationGeneration", it.launchGeneration) }
    }
    fun beginPreparation(launchGeneration: Long) {
        preparationReceipt = CapturePreparationReceipt(launchGeneration)
        observationError = null
        feedback = "Preparing microphone…"
    }
    fun retirePreparation() { preparationReceipt = null }
    fun resetCapture() {
        retirePreparation()
        observationError = null
        invalidate()
        savedFeedbackJob?.cancel()
        savedFeedbackJob = null
        savedExitDeadlineMs = null
        savedFeedbackCaptureId = null
        savedFeedbackDeadlineMs = 0L
        observedCaptureId = null
        microphoneReadyCaptureId = null
        stopRequestedCaptureId = null
        stopBeforeIdentity = false
        terminalSaveFailureCaptureId = null
        readySignalId = null
        savedSignalId = null
        failedCaptureId = null
    }
    fun requestStop() {
        retirePreparation()
        stopRequestedCaptureId = snapshot.active?.captureId
        stopBeforeIdentity = stopRequestedCaptureId == null
        feedback = "Saving recording…"
        onChanged()
    }
    fun calibrationChanged(active: Boolean) {
        calibrationActive = active
        if (active) observedCaptureId = null
        invalidate()
        refresh()
    }
    fun resume() { resumed = true; readsEnabled = true; invalidate(); refresh() }
    fun pause() {
        resumed = false
        readsEnabled = false
        invalidate()
        savedFeedbackJob?.cancel()
        savedFeedbackJob = null
        savedExitDeadlineMs = null
    }
    fun close() { closed = true; pause(); refreshJob?.cancel() }
    private fun invalidate() { generation++; dirty = true }

    fun signal(state: String?, captureId: String?, ready: Boolean, message: String?) {
        if (closed) return
        if (captureId != null && observedCaptureId != null && captureId != observedCaptureId) { invalidate(); refresh(); return }
        if (state == null) { refresh(); return }
        if (state in setOf("failed", "saved", "completed")) retirePreparation()
        if (captureId != null && (state == "ready" || state == "attached" && ready)) readySignalId = captureId
        if (state == "saved") savedSignalId = captureId
        if (state == "failed") failedCaptureId = captureId ?: observedCaptureId
        if (state == "saved" || state == "completed") { terminalSaveFailureCaptureId = null; failedCaptureId = null }
        feedback = when (state) {
            "ready" -> "Microphone ready"
            "attached" -> if (ready) "Recording · microphone ready" else "Preparing microphone…"
            "saved" -> "Audio saved · transcription pending"
            "completed" -> "Audio and transcription saved"
            "recovered" -> message ?: "Previous capture recovered"
            "failed" -> message ?: "Capture needs attention"
            else -> "Capture needs attention"
        }
        generation++
        refresh()
    }

    fun refresh(reload: Boolean = true) {
        if (closed) return
        dirty = true
        reloadRecent = reloadRecent || reload
        if (!readsEnabled) return
        if (refreshJob?.isActive == true) return
        refreshJob = scope.launch {
            while (dirty && !closed && readsEnabled) {
                dirty = false
                val ticket = generation
                val observed = observedCaptureId ?: savedSignalId
                val previous = snapshot
                val reloadHistory = reloadRecent
                reloadRecent = false
                val pendingStart = pendingCaptureStart
                val allowSaved = resumed && !calibrationActive && !pendingStart
                val result = try { withContext(io) {
                    val active = readActive()
                    val completed = if (active == null) observed?.let(readRecord) else null
                    val saved = completed?.takeIf { allowSaved && it.captureState == CaptureState.SAVED }
                    val recent = if (saved == null && active == null && !pendingStart && (reloadHistory || previous.active != null)) readRecent() else previous.recent
                    Triple(active, saved, recent)
                } } catch (failure: Exception) {
                    if (failure is kotlinx.coroutines.CancellationException) throw failure
                    if (ticket == generation && readsEnabled && !closed) {
                        observationError = "Capture status unavailable · audio retained"
                        onChanged()
                    }
                    continue
                }
                if (closed || !readsEnabled || ticket != generation) { reloadRecent = reloadRecent || reloadHistory; continue }
                observationError = null
                snapshot = CaptureSnapshot(result.first, result.third)
                val active = result.first
                if (active != null) {
                    retirePreparation()
                    if (observedCaptureId != active.captureId) {
                        microphoneReadyCaptureId = null
                        stopRequestedCaptureId = null
                        terminalSaveFailureCaptureId = null
                    }
                    observedCaptureId = active.captureId
                    if (stopBeforeIdentity) { stopRequestedCaptureId = active.captureId; stopBeforeIdentity = false }
                    if (active.captureState == CaptureState.RECORDING && (active.observedFrames > 0 || readySignalId == active.captureId)) microphoneReadyCaptureId = active.captureId
                    if (failedCaptureId == active.captureId && active.captureState == CaptureState.STOPPING) terminalSaveFailureCaptureId = active.captureId
                }
                result.second?.let {
                    observedCaptureId = it.captureId
                    savedFeedbackText = if (!it.finalText.isNullOrBlank()) "Audio and transcription saved"
                        else if (it.liveTranscriptionState == LiveTranscriptionState.UNAVAILABLE)
                            "Audio saved · transcription pending on phone" + (it.liveErrorCode?.let { code -> " · ${dev.gitblame.voxgolem.capture.CaptureErrorCodes.safe(code)}" } ?: "")
                        else "Audio saved · transcription pending on phone"
                    showSavedFeedback(it.captureId)
                }
                onChanged()
            }
        }
    }
    private fun showSavedFeedback(captureId: String) {
        retirePreparation()
        if (savedFeedbackCaptureId != captureId) {
            savedFeedbackJob?.cancel()
            savedExitDeadlineMs = null
            savedFeedbackCaptureId = captureId
            savedFeedbackDeadlineMs = SystemClock.elapsedRealtime() + CAPTURE_SAVED_FEEDBACK_MILLIS
        }
        if (savedFeedbackJob?.isActive == true) return
        savedFeedbackJob = scope.launch {
            delay((savedFeedbackDeadlineMs - CAPTURE_SAVED_EXIT_MILLIS - SystemClock.elapsedRealtime()).coerceAtLeast(0))
            if (!resumed || closed || savedFeedbackCaptureId != captureId) return@launch
            if (SystemClock.elapsedRealtime() < savedFeedbackDeadlineMs) savedExitDeadlineMs = savedFeedbackDeadlineMs
            delay((savedFeedbackDeadlineMs - SystemClock.elapsedRealtime()).coerceAtLeast(0))
            if (resumed && !closed && savedFeedbackCaptureId == captureId) onSavedExit()
        }
    }
}
