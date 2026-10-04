package dev.gitblame.voxgolem.capture

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.Looper
import com.google.android.gms.tasks.Tasks
import com.google.android.gms.wearable.Wearable
import org.json.JSONObject
import java.io.File
import java.nio.charset.StandardCharsets
import java.time.Instant
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.ScheduledThreadPoolExecutor
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean

/** Native owner for incoming Watch dictation; recognition never enters assistant APIs. */
class PhoneCaptureCoordinator internal constructor(
    context: Context,
    private val speech: SpeechRecognizerAdapter,
    private val sendMessage: (String, String, ByteArray) -> Unit,
    private val archiveWav: (CaptureRecord) -> Uri,
    private val exportOrg: (String) -> OrgExportResult,
    private val replayPacingMillis: Long = 100,
    private val recognitionTimeoutSeconds: Long = 30,
    private val runtimeReady: () -> Boolean = { PhoneCaptureRuntimeService.isForegroundReady() },
) {
    constructor(context: Context) : this(
        context.applicationContext,
        SuppliedAudioSpeechRecognizer(context.applicationContext),
        { nodeId, path, bytes -> Tasks.await(Wearable.getMessageClient(context).sendMessage(nodeId, path, bytes)) },
        { record ->
            val destination = SafNotesStore(context).selected() ?: throw IllegalStateException("notes-folder-not-selected")
            val file = record.localAudioPath?.let(::File) ?: throw IllegalStateException("staged-recording-missing")
            SafNotesStore(context).archiveAudio(destination, record.captureId, file, record.sha256!!, record.finalizedByteLength!!)
        },
        { captureId -> OrgExportCoordinator(context).process(captureId) },
    )

    private val appContext = context.applicationContext
    private val repository = CaptureRepository(appContext)
    private val prefs = appContext.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
    private val capability by lazy { Wearable.getCapabilityClient(appContext) }
    private val io: ExecutorService = Executors.newSingleThreadExecutor()
    private val timeoutExecutor = ScheduledThreadPoolExecutor(1)
    private val sessions = ConcurrentHashMap<String, RecognitionSession>()
    private val recovered = AtomicBoolean(false)

    private data class RecognitionSession(
        val captureId: String,
        val sessionId: String,
        val sourceNodeId: String,
        val reducer: TranscriptReducer,
        var revision: Long = 0,
        var accepted: Boolean = false,
        var final: Boolean = false,
        var timeout: ScheduledFuture<*>? = null,
    )

    data class EnableResult(val state: String, val error: String? = null)

    /** Called only from the visible native enable flow or a visible app startup after prior consent. */
    fun start(): EnableResult {
        if (isWatch()) return EnableResult("unsupported", "phone-capture-not-available-on-wear")
        if (!prefs.getBoolean(KEY_OPTED_IN, false)) return EnableResult("setup-required", "phone-capture-not-enabled")
        val missing = missingPermissions()
        if (missing.isNotEmpty()) return EnableResult("permission-required", missing.joinToString(","))
        if (!recovered.getAndSet(true)) io.execute { repository.recoverInterrupted() }
        capability.addLocalCapability(CaptureWire.PHONE_CAPABILITY).addOnFailureListener {
            prefs.edit().putString(KEY_RUNTIME_ERROR, "wear-capability-unavailable").apply()
            capability.removeLocalCapability(CaptureWire.PHONE_CAPABILITY)
        }
        prefs.edit().remove(KEY_RUNTIME_ERROR).apply()
        val intent = Intent(appContext, PhoneCaptureRuntimeService::class.java).setAction(PhoneCaptureRuntimeService.ACTION_START)
        try {
            if (Build.VERSION.SDK_INT >= 26) appContext.startForegroundService(intent) else appContext.startService(intent)
        } catch (_: RuntimeException) {
            prefs.edit().putString(KEY_RUNTIME_ERROR, "connected-device-service-start-denied").apply()
            runCatching { Tasks.await(capability.removeLocalCapability(CaptureWire.PHONE_CAPABILITY)) }
            return EnableResult("failed", "connected-device-service-start-denied")
        }
        io.execute {
            repository.list().filter { it.captureState == CaptureState.SAVED && it.recognitionState != RecognitionState.RUNNING }
                .forEach { retryCapture(it.captureId) }
        }
        return EnableResult("starting")
    }

    /** Activity is visibly foregrounded when the plugin calls this after runtime permissions succeed. */
    fun enableFromVisibleActivity(): EnableResult {
        if (isWatch()) return EnableResult("unsupported", "phone-capture-not-available-on-wear")
        val missing = missingPermissions()
        if (missing.isNotEmpty()) return EnableResult("permission-required", missing.joinToString(","))
        if (!prefs.edit().putBoolean(KEY_OPTED_IN, true).remove(KEY_RUNTIME_ERROR).commit()) {
            return EnableResult("failed", "cannot-persist-phone-capture-setting")
        }
        return start()
    }

    fun isEnabled(): Boolean = prefs.getBoolean(KEY_OPTED_IN, false) && missingPermissions().isEmpty()

    fun retryPendingCaptures() {
        io.execute {
            var offset = 0
            do {
                val page = repository.list(offset, 100)
                page.filter { it.captureState == CaptureState.SAVED }.forEach { retryCapture(it.captureId) }
                offset += page.size
            } while (page.size == 100)
        }
    }

    fun finalizeCapture(captureId: String) {
        require(CaptureProtocol.isValidUuid(captureId)) { "Invalid capture ID" }
        val finalized = repository.update(captureId) { current ->
            if (current.captureState == CaptureState.SAVED) current
            else {
                require(current.captureState == CaptureState.INTERRUPTED) { "Capture is not interrupted" }
                require(current.sha256 != null && current.finalizedByteLength != null &&
                    current.localAudioPath?.let { File(it).isFile } == true) { "Recovered audio is unavailable" }
                current.copy(captureState = CaptureState.SAVED, stopReason = "user-finalized")
            }
        }
        require(finalized != null) { "Capture was not found" }
        retryCapture(captureId)
    }

    /** Retry a finalized capture by streaming its PCM data chunk through the supplied-audio API. */
    fun retryCapture(captureId: String) {
        if (!CaptureProtocol.isValidUuid(captureId)) return
        io.execute {
            var record = repository.get(captureId) ?: return@execute
            if (record.captureState != CaptureState.SAVED) return@execute
            if (record.recognitionState == RecognitionState.RUNNING) return@execute

            val alreadyArchived = record.archiveState == ArchiveState.VERIFIED
            if (!alreadyArchived) {
                archiveAndExport(record.captureId, "")
                record = repository.get(captureId) ?: return@execute
            }
            if (record.recognitionState == RecognitionState.FINAL || record.recognitionState == RecognitionState.NO_SPEECH) {
                if (alreadyArchived) maybeExport(record)
                return@execute
            }
            val wav = record.localAudioPath?.let(::File)
            if (wav == null || !wav.isFile) {
                failSavedRecognition(record, "saved-wav-unavailable")
                return@execute
            }
            if (!runtimeReady()) {
                repository.update(record.captureId) { current -> current.copy(
                    recognitionState = RecognitionState.QUEUED,
                    lastErrorStage = "execution",
                    lastErrorCode = "connected-device-service-not-ready",
                ) }
                return@execute
            }
            val capabilities = speech.capabilities()
            if (!capabilities.externalAudio || !capabilities.onDevice || record.format !in capabilities.formats) {
                repository.update(record.captureId) {
                    it.copy(
                        recognitionState = RecognitionState.UNAVAILABLE,
                        lastErrorStage = "recognition",
                        lastErrorCode = "external-audio-provider-unavailable",
                    )
                }
                return@execute
            }
            val sessionId = UUID.randomUUID().toString()
            val session = newSession(record, sessionId, "")
            if (!markRunning(record, session)) return@execute
            try {
                speech.start(sessionId, record.format, record.language, eventsFor(session))
                WavPcmReader(wav, record.format).use { source ->
                    val expectedFrames = record.durableFrames.takeIf { it > 0 } ?: source.frameCount
                    check(source.frameCount == expectedFrames) { "saved-wav-frame-count-mismatch" }
                    while (true) {
                        val chunk = source.read(3_200) ?: break
                        speech.append(sessionId, chunk)
                        armRecognitionTimeout(session)
                        if (replayPacingMillis > 0) Thread.sleep(replayPacingMillis)
                    }
                }
                speech.finish(sessionId)
                armRecognitionTimeout(session)
            } catch (failure: Exception) {
                speech.cancel(sessionId)
                failSavedRecognition(record, failure.message ?: "saved-audio-transcription-failed", sessionId)
            }
        }
    }

    internal fun beginLive(captureId: String, sessionId: String, sourceNodeId: String, format: CaptureFormat, startedAtUtc: String) {
        if (!CaptureProtocol.isValidUuid(captureId) || !CaptureProtocol.isValidUuid(sessionId) || sourceNodeId.isBlank() ||
            !CaptureProtocol.validateFormat(format) || startedAtUtc.length !in 1..64) return
        val existing = repository.get(captureId)
        if (existing != null && existing.captureState != CaptureState.RECORDING) {
            sendReady(sourceNodeId, captureId, sessionId, "unavailable", false)
            return
        }
        val active = sessions[sessionId]
        if (active != null) {
            if (active.captureId == captureId && active.sourceNodeId == sourceNodeId) {
                sendReady(sourceNodeId, captureId, sessionId, if (active.accepted) "accepted" else "attempting", active.accepted)
            } else {
                sendReady(sourceNodeId, captureId, sessionId, "unavailable", false)
            }
            return
        }
        val record = existing ?: runCatching {
            repository.save(
                CaptureRecord(
                    captureId = captureId,
                    createdAtUtc = startedAtUtc,
                    startedAtUtc = startedAtUtc,
                    captureState = CaptureState.RECORDING,
                    format = format,
                    recognitionState = RecognitionState.QUEUED,
                    recognitionSessionId = sessionId,
                ),
            )
        }.getOrElse {
            sendReady(sourceNodeId, captureId, sessionId, "unavailable", false)
            return
        }
        if (record.format != format) {
            updateRecognitionFailure(record, sessionId, "capture-format-conflict")
            sendReady(sourceNodeId, captureId, sessionId, "unavailable", false)
            return
        }
        if (!runtimeReady()) {
            repository.update(captureId) { current ->
                if (current.recognitionSessionId != sessionId) current
                else current.copy(
                    recognitionState = RecognitionState.QUEUED,
                    lastErrorStage = "execution",
                    lastErrorCode = "connected-device-service-not-ready",
                )
            }
            sendReady(sourceNodeId, captureId, sessionId, "pending", false)
            return
        }
        if (appContext.checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
            updateRecognitionFailure(record, sessionId, "record-audio-permission-required")
            sendReady(sourceNodeId, captureId, sessionId, "unavailable", false)
            return
        }
        val caps = speech.capabilities()
        if (!caps.externalAudio || !caps.onDevice || format !in caps.formats) {
            updateRecognitionFailure(record, sessionId, "external-audio-provider-unavailable")
            sendReady(sourceNodeId, captureId, sessionId, "unavailable", false)
            return
        }
        val session = newSession(record, sessionId, sourceNodeId)
        if (!markRunning(record, session)) return
        sendReady(sourceNodeId, captureId, sessionId, "attempting", false)
        try {
            speech.start(sessionId, format, record.language, eventsFor(session))
        } catch (failure: Exception) {
            sessions.remove(sessionId)
            updateRecognitionFailure(record, sessionId, failure.message ?: "external-audio-provider-rejected")
            sendReady(sourceNodeId, captureId, sessionId, "unavailable", false)
        }
    }

    internal fun appendLive(sessionId: String, pcm: ByteArray) {
        val session = sessions[sessionId] ?: return
        try {
            speech.append(sessionId, pcm)
            armRecognitionTimeout(session)
        } catch (failure: Exception) {
            failSession(session, failure.message ?: "external-audio-write-failed")
        }
    }

    internal fun finishLive(captureId: String, sessionId: String, sourceNodeId: String, frameCount: Long) {
        if (frameCount < 0) return
        repository.update(captureId) { current ->
            if (current.recognitionSessionId == sessionId) current.copy(observedFrames = frameCount) else current
        }
        val session = sessions[sessionId] ?: return
        if (session.captureId != captureId || session.sourceNodeId != sourceNodeId || frameCount < 0) return
        speech.finish(sessionId)
        armRecognitionTimeout(session)
    }

    internal fun noteLiveStop(captureId: String, sessionId: String, sourceNodeId: String, frameCount: Long) {
        val session = sessions[sessionId] ?: return
        val record = repository.get(captureId) ?: return
        val maximumFrames = record.format.sampleRate.toLong() * MAX_CAPTURE_MILLIS / 1_000
        if (frameCount < 0 || frameCount > maximumFrames || session.captureId != captureId ||
            session.sourceNodeId != sourceNodeId || record.recognitionSessionId != sessionId) return
    }

    /** A live placeholder has no committed content hash; a verified archive finalizes that same row. */
    internal fun onArchiveReceived(record: CaptureRecord, sourceNodeId: String) {
        if (!CaptureProtocol.isValidUuid(record.captureId) || record.sha256?.matches(Regex("(?i)^[0-9a-f]{64}$")) != true ||
            record.finalizedByteLength == null || record.finalizedByteLength < 44 ||
            record.captureState !in setOf(CaptureState.SAVED, CaptureState.INTERRUPTED)) {
            throw IllegalArgumentException("invalid-finalized-capture-record")
        }
        val stored = repository.get(record.captureId)
        val incompleteLiveAudio = stored?.sha256 == null && stored?.recognitionSessionId != null &&
            stored.observedFrames != record.durableFrames
        if (incompleteLiveAudio) {
            stored?.recognitionSessionId?.let { previousId ->
                sessions.remove(previousId)?.timeout?.cancel(false)
                speech.cancel(previousId)
            }
        }
        if (stored?.sha256 != null &&
            (!stored.sha256.equals(record.sha256, ignoreCase = true) || stored.finalizedByteLength != record.finalizedByteLength)) {
            throw IllegalStateException("capture-id-content-conflict")
        }
        if (stored != null && stored.sha256 == null && stored.localAudioPath != null) {
            throw IllegalStateException("capture-id-has-nonprovisional-record")
        }
        if (stored == null) {
            repository.save(record.copy(transferState = TransferState.PHONE_STAGED, archiveState = ArchiveState.STAGED))
        } else {
            repository.update(record.captureId) { current ->
                current.copy(
                    sourceKind = record.sourceKind,
                    startedAtUtc = current.startedAtUtc ?: record.startedAtUtc,
                    finishedAtUtc = record.finishedAtUtc,
                    finishedLocalDate = record.finishedLocalDate,
                    timezoneId = record.timezoneId,
                    captureState = record.captureState,
                    stopReason = record.stopReason ?: current.stopReason,
                    observedFrames = maxOf(current.observedFrames, record.observedFrames),
                    durableFrames = record.durableFrames,
                    format = record.format,
                    localAudioPath = record.localAudioPath,
                    finalizedByteLength = record.finalizedByteLength,
                    sha256 = record.sha256,
                    transferState = if (current.transferState == TransferState.PHONE_ARCHIVED) current.transferState else TransferState.PHONE_STAGED,
                    archiveState = if (current.archiveState == ArchiveState.VERIFIED) current.archiveState else ArchiveState.STAGED,
                    recognitionState = if (incompleteLiveAudio) RecognitionState.QUEUED else current.recognitionState,
                    recognitionSessionId = if (incompleteLiveAudio) null else current.recognitionSessionId,
                    previewText = current.finalText ?: current.previewText,
                    finalText = if (incompleteLiveAudio) null else current.finalText,
                )
            }
        }
        val merged = repository.get(record.captureId) ?: return
        if (merged.transferState == TransferState.PHONE_ARCHIVED && merged.archivedAudioUri != null) {
            sendReceipt(sourceNodeId, merged, "PHONE_ARCHIVED", merged.exportDestinationId)
            maybeExport(merged)
            return
        }
        sendReceipt(sourceNodeId, merged, "PHONE_STAGED")
        io.execute { archiveAndExport(record.captureId, sourceNodeId) }
        if (merged.captureState == CaptureState.SAVED &&
            merged.recognitionState in setOf(RecognitionState.QUEUED, RecognitionState.UNAVAILABLE, RecognitionState.FAILED) &&
            sessions.values.none { it.captureId == merged.captureId }) {
            retryCapture(merged.captureId)
        }
    }

    internal fun markArchiveFailure(captureId: String, code: String) {
        repository.update(captureId) { current ->
            if (current.transferState == TransferState.PHONE_ARCHIVED) current
            else current.copy(archiveState = ArchiveState.RETRYABLE_ERROR, lastErrorStage = "archive", lastErrorCode = code)
        }
    }

    internal fun awaitIdle(timeout: Long = 5, unit: TimeUnit = TimeUnit.SECONDS): Boolean {
        val done = java.util.concurrent.CountDownLatch(1)
        io.execute { done.countDown() }
        return done.await(timeout, unit)
    }

    internal fun shutdownForTests() {
        sessions.values.toList().forEach { speech.cancel(it.sessionId) }
        sessions.clear()
        io.shutdownNow()
        timeoutExecutor.shutdownNow()
    }

    private fun newSession(record: CaptureRecord, sessionId: String, sourceNodeId: String): RecognitionSession {
        val reducer = TranscriptReducer(record.captureId)
        reducer.beginSession(sessionId)
        return RecognitionSession(record.captureId, sessionId, sourceNodeId, reducer)
    }

    private fun markRunning(record: CaptureRecord, session: RecognitionSession): Boolean {
        val changed = repository.update(record.captureId) { current ->
            if (current.captureState != CaptureState.SAVED && current.captureState != CaptureState.RECORDING) current
            else current.copy(
                recognitionState = RecognitionState.RUNNING,
                recognitionSessionId = session.sessionId,
                engineId = "android-speech-external-audio",
                lastErrorStage = null,
                lastErrorCode = null,
            )
        }
        if (changed == null || changed.recognitionSessionId != session.sessionId || changed.recognitionState != RecognitionState.RUNNING) return false
        sessions[session.sessionId] = session
        armRecognitionTimeout(session)
        return true
    }

    private fun armRecognitionTimeout(session: RecognitionSession) {
        synchronized(session) {
            session.timeout?.cancel(false)
            session.timeout = timeoutExecutor.schedule(
                { failSession(session, "speech-recognizer-timeout") },
                recognitionTimeoutSeconds,
                TimeUnit.SECONDS,
            )
        }
    }

    private fun eventsFor(session: RecognitionSession) = object : SpeechRecognizerAdapter.Events {
        override fun onAttemptState(state: String) {
            if (state == "provider-ready" && session.sourceNodeId.isNotBlank()) {
                sendReady(session.sourceNodeId, session.captureId, session.sessionId, "provider-ready", false)
            }
        }

        override fun onHypothesis(text: String, isFinal: Boolean) {
            if (sessions[session.sessionId] !== session || session.final) return
            session.revision += 1
            val snapshot = session.reducer.apply(
                TranscriptUpdate(session.captureId, session.sessionId, session.revision, text, isFinal),
            )
            session.accepted = true
            if (isFinal) session.final = true
            io.execute {
                if (repository.get(session.captureId)?.recognitionSessionId != session.sessionId) return@execute
                val updated = repository.update(session.captureId) { current ->
                    if (current.recognitionSessionId != session.sessionId) current
                    else current.copy(
                        recognitionState = if (isFinal) if (text.isBlank()) RecognitionState.NO_SPEECH else RecognitionState.FINAL else RecognitionState.RUNNING,
                        previewText = snapshot.preview,
                        finalText = snapshot.finalText,
                        lastRevision = snapshot.revision,
                        transcriptVersion = if (isFinal && !text.isBlank()) current.transcriptVersion + 1 else current.transcriptVersion,
                        lastErrorStage = null,
                        lastErrorCode = null,
                    )
                }
                if (session.sourceNodeId.isNotBlank()) {
                    sendReady(session.sourceNodeId, session.captureId, session.sessionId, "accepted", true)
                    sendTranscript(session, text, isFinal)
                }
                if (isFinal) {
                    sessions.remove(session.sessionId, session)
                    session.timeout?.cancel(false)
                    updated?.let(::maybeExport)
                }
            }
        }

        override fun onFailure(code: String) = failSession(session, code)
    }

    private fun failSession(session: RecognitionSession, code: String) {
        if (sessions.remove(session.sessionId, session)) {
            session.timeout?.cancel(false)
            runCatching { speech.cancel(session.sessionId) }
            io.execute {
                val updated = repository.update(session.captureId) { current ->
                    if (current.recognitionSessionId != session.sessionId || current.recognitionState == RecognitionState.FINAL) current
                    else current.copy(
                        recognitionState = RecognitionState.FAILED,
                        lastErrorStage = "recognition",
                        lastErrorCode = code.take(160),
                    )
                }
                if (session.sourceNodeId.isNotBlank()) sendReady(session.sourceNodeId, session.captureId, session.sessionId, "unavailable", false)
                updated?.let(::maybeExport)
            }
        }
    }

    private fun updateRecognitionFailure(record: CaptureRecord, sessionId: String, reason: String) {
        repository.update(record.captureId) { current ->
            if (current.recognitionSessionId != sessionId) current
            else current.copy(
                recognitionState = RecognitionState.UNAVAILABLE,
                lastErrorStage = "recognition",
                lastErrorCode = reason.take(160),
            )
        }
    }

    private fun failSavedRecognition(record: CaptureRecord, reason: String, sessionId: String? = null) {
        repository.update(record.captureId) { current ->
            if (sessionId != null && current.recognitionSessionId != sessionId) current
            else current.copy(
                recognitionState = RecognitionState.FAILED,
                lastErrorStage = "recognition",
                lastErrorCode = reason.take(160),
            )
        }
    }

    private fun archiveAndExport(captureId: String, sourceNodeId: String) {
        val record = repository.get(captureId) ?: return
        if (record.archiveState == ArchiveState.VERIFIED && record.archivedAudioUri != null) {
            maybeExport(record)
            return
        }
        try {
            val uri = archiveWav(record)
            val archived = repository.update(captureId) {
                if (it.sha256 != record.sha256 || it.finalizedByteLength != record.finalizedByteLength) it
                else it.copy(
                    archivedAudioUri = uri.toString(),
                    archiveState = ArchiveState.VERIFIED,
                    transferState = TransferState.PHONE_ARCHIVED,
                    lastErrorStage = null,
                    lastErrorCode = null,
                )
            } ?: return
            if (archived.archiveState == ArchiveState.VERIFIED) {
                if (sourceNodeId.isNotBlank()) sendReceipt(sourceNodeId, archived, "PHONE_ARCHIVED", archived.exportDestinationId)
                maybeExport(archived)
            }
        } catch (failure: Exception) {
            val code = failure.message ?: failure.javaClass.simpleName.lowercase()
            repository.update(captureId) { current ->
                if (current.archiveState == ArchiveState.VERIFIED) current
                else current.copy(
                    archiveState = if (code == "notes-folder-not-selected") ArchiveState.WAITING_FOR_FOLDER else ArchiveState.RETRYABLE_ERROR,
                    transferState = TransferState.PHONE_STAGED,
                    lastErrorStage = "archive",
                    lastErrorCode = code.take(160),
                )
            }
        }
    }

    private fun maybeExport(record: CaptureRecord) {
        if (record.orgState == OrgState.EXPORTED || record.captureState != CaptureState.SAVED || record.recognitionState != RecognitionState.FINAL ||
            record.finalText.isNullOrBlank() || record.archiveState != ArchiveState.VERIFIED || record.archivedAudioUri == null) return
        runCatching { exportOrg(record.captureId) }.onFailure { failure ->
            repository.update(record.captureId) {
                it.copy(orgState = OrgState.RETRYABLE_ERROR, lastErrorStage = "org", lastErrorCode = failure.javaClass.simpleName.lowercase())
            }
        }
    }

    private fun sendReady(nodeId: String, captureId: String, sessionId: String, state: String, canTranscribe: Boolean) {
        if (nodeId.isBlank() || !CaptureProtocol.isValidUuid(captureId) || !CaptureProtocol.isValidUuid(sessionId)) return
        val bytes = JSONObject()
            .put("protocol", CaptureWire.PROTOCOL)
            .put("captureId", captureId)
            .put("sessionId", sessionId)
            .put("canTranscribeLive", canTranscribe)
            .put("recognitionStatus", state)
            .toString().toByteArray(StandardCharsets.UTF_8)
        enqueueMessage(nodeId, CaptureWire.READY, bytes)
    }

    private fun sendTranscript(session: RecognitionSession, text: String, final: Boolean) {
        val path = if (final) CaptureWire.FINAL else CaptureWire.PARTIAL
        val body = JSONObject()
            .put("protocol", CaptureWire.PROTOCOL)
            .put("captureId", session.captureId)
            .put("sessionId", session.sessionId)
            .put("revision", session.revision)
            .put("transcriptVersion", repository.get(session.captureId)?.transcriptVersion ?: 0)
            .put("fullText", text)
            .toString()
        val bytes = body.toByteArray(StandardCharsets.UTF_8)
        if (CaptureProtocol.validatePayloadSize(bytes.size)) {
            enqueueMessage(session.sourceNodeId, path, bytes)
        } else {
            repository.update(session.captureId) { current ->
                if (current.recognitionSessionId != session.sessionId) current
                else current.copy(lastErrorStage = "delivery", lastErrorCode = "transcript-not-sent-exceeds-message-limit")
            }
            sendReady(session.sourceNodeId, session.captureId, session.sessionId, "transcript-too-large", true)
        }
    }

    private fun sendReceipt(nodeId: String, record: CaptureRecord, stage: String, destinationId: String? = null) {
        if (nodeId.isBlank() || record.sha256.isNullOrBlank()) return
        val bytes = JSONObject().put("protocol", CaptureWire.PROTOCOL).put("captureId", record.captureId)
            .put("archiveHash", record.sha256).put("stage", stage)
            .apply { if (destinationId != null) put("destinationId", destinationId) }
            .toString().toByteArray(StandardCharsets.UTF_8)
        enqueueMessage(nodeId, CaptureWire.RECEIPT, bytes)
    }

    private fun enqueueMessage(nodeId: String, path: String, bytes: ByteArray) {
        if (bytes.size > CaptureProtocol.MAX_MESSAGE_BYTES) return
        io.execute { runCatching { sendMessage(nodeId, path, bytes) } }
    }

    private fun missingPermissions(): List<String> = buildList {
        if (appContext.checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) add(Manifest.permission.RECORD_AUDIO)
        if (Build.VERSION.SDK_INT >= 31 && appContext.checkSelfPermission(Manifest.permission.BLUETOOTH_CONNECT) != PackageManager.PERMISSION_GRANTED) add(Manifest.permission.BLUETOOTH_CONNECT)
        if (Build.VERSION.SDK_INT >= 33 && appContext.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) add(Manifest.permission.POST_NOTIFICATIONS)
    }

    private fun isWatch(): Boolean = appContext.packageManager.hasSystemFeature("android.hardware.type.watch")

    internal companion object {
        const val PREFERENCES = "capture_phone_runtime"
        const val KEY_OPTED_IN = "phone_capture_opted_in"
        const val KEY_RUNTIME_ERROR = "phone_capture_runtime_error"
        private const val MAX_CAPTURE_MILLIS = 10 * 60 * 1_000L

        @Volatile private var instance: PhoneCaptureCoordinator? = null

        fun shared(context: Context): PhoneCaptureCoordinator = instance ?: synchronized(this) {
            instance ?: PhoneCaptureCoordinator(context.applicationContext).also { instance = it }
        }

        internal fun resetSharedForTests() = synchronized(this) { instance = null }
    }
}
