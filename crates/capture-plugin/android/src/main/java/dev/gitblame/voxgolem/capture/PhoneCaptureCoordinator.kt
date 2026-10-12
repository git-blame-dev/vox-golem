package dev.gitblame.voxgolem.capture

import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import dev.gitblame.voxgolem.assistant.recognition.RecognitionResourceLease
import com.google.android.gms.tasks.Tasks
import com.google.android.gms.wearable.Wearable
import java.io.File
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
    sendMessage: (String, String, ByteArray) -> Unit,
    archiveWav: (CaptureRecord) -> Uri,
    exportOrg: (String) -> OrgExportResult,
    private val replayPacingMillis: Long = 100,
    private val recognitionTimeoutSeconds: Long = 30,
    private val runtimeReady: () -> Boolean = { PhoneCaptureRuntimeService.isForegroundReady() },
    sendOversizedReceipt: (String, String, ByteArray) -> Unit = PhoneCaptureDelivery.wearReceiptSender(context),
    private val recognitionLeases: RecognitionResourceLease = RecognitionResourceLease.process,
) {
    constructor(context: Context) : this(
        context.applicationContext,
        SuppliedAudioSpeechRecognizer(context.applicationContext),
        { nodeId, path, bytes -> Tasks.await(Wearable.getMessageClient(context).sendMessage(nodeId, path, bytes), 5, TimeUnit.SECONDS) },
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
    private val io: ExecutorService = Executors.newSingleThreadExecutor()
    private val outgoing = PhoneCaptureDelivery(repository, sendMessage, sendOversizedReceipt)
    private val background: ExecutorService = Executors.newSingleThreadExecutor()
    private val archive = PhoneCaptureArchive(repository, archiveWav, exportOrg, outgoing)
    private val timeoutExecutor = ScheduledThreadPoolExecutor(1)
    private val sessions = ConcurrentHashMap<String, RecognitionSession>()
    private val pendingLiveArchives = ConcurrentHashMap<String, PendingArchive>()
    private val terminalLiveSessions = ConcurrentHashMap.newKeySet<String>()
    private val liveInputs = ConcurrentHashMap<String, LiveInput>()
    private var probeSessionId: String? = null
    private var cancelProbe: (() -> Unit)? = null
    private var probeLease: RecognitionResourceLease.Token? = null
    private var probeRetirementObserver: ProbeRetirementObserver? = null
    private var probeSdkOwner: Any? = null
    private var failedProbeRetirement: FailedProbeRetirement? = null
    private var admissionInProgress = false
    private val failedRetirements = ConcurrentHashMap<String, RecognitionSession>()
    @Volatile private var runtimeStopped = false
    internal var beforeDuplicateReady: () -> Unit = {}
    private val recovered = AtomicBoolean(false)
    private val leaseSubscription = recognitionLeases.subscribeAvailable { kind ->
        if (kind == RecognitionResourceLease.Kind.CHAT && !runtimeStopped && !io.isShutdown) retryPendingCaptures()
    }

    private data class PendingArchive(val record: CaptureRecord, val sourceNodeId: String)
    private data class LiveInput(val captureId: String, val sourceNodeId: String, val close: () -> Unit)

    internal interface ProbeRetirementObserver {
        fun allocated(sdkOwner: Any): Boolean
        fun failed(sdkOwner: Any, retry: () -> Boolean)
        fun retired(sdkOwner: Any)
    }
    private data class FailedProbeRetirement(
        val sdkOwner: Any,
        val retry: () -> Boolean,
        val inProgress: AtomicBoolean = AtomicBoolean(false),
    )

    private data class RecognitionSession(
        val captureId: String,
        val sessionId: String,
        val sourceNodeId: String,
        val reducer: TranscriptReducer,
        val live: Boolean,
        val timing: CaptureTiming,
        @Volatile var revision: Long = 0,
        val transcriptDelivery: PhoneCaptureDelivery.Transcript = PhoneCaptureDelivery.Transcript(),
        var accepted: Boolean = false,
        val terminalClaimed: AtomicBoolean = AtomicBoolean(false),
        @Volatile var inputEnded: Boolean = false,
        val adapterInitialized: java.util.concurrent.CountDownLatch = java.util.concurrent.CountDownLatch(1),
        @Volatile var adapterReady: Boolean = false,
        var inputOwner: (() -> Unit)? = null,
        @Volatile var adapterEvents: SpeechRecognizerAdapter.Events? = null,
        var timeout: ScheduledFuture<*>? = null,
        var lease: RecognitionResourceLease.Token? = null,
        val retirementClaimed: AtomicBoolean = AtomicBoolean(false),
        val nativeRetired: AtomicBoolean = AtomicBoolean(false),
    )

    data class EnableResult(val state: String, val error: String? = null)

    /** Called only from the visible native enable flow or a visible app startup after prior consent. */
    fun start(): EnableResult {
        if (isWatch()) return EnableResult("unsupported", "phone-capture-not-available-on-wear")
        if (!prefs.getBoolean(KEY_OPTED_IN, false)) return EnableResult("setup-required", "phone-capture-not-enabled")
        val missing = missingPermissions()
        if (missing.isNotEmpty()) return EnableResult("permission-required", missing.joinToString(","))
        retryFailedRetirements()
        if (!recovered.getAndSet(true)) io.execute { repository.recoverInterrupted(CapturePurpose.NOTE) }
        prefs.edit().remove(KEY_RUNTIME_ERROR).apply()
        val intent = Intent(appContext, PhoneCaptureRuntimeService::class.java).setAction(PhoneCaptureRuntimeService.ACTION_START)
        try {
            if (Build.VERSION.SDK_INT >= 26) appContext.startForegroundService(intent) else appContext.startService(intent)
        } catch (_: RuntimeException) {
            prefs.edit().putString(KEY_RUNTIME_ERROR, "connected-device-service-start-denied").apply()
            return EnableResult("failed", "connected-device-service-start-denied")
        }
        retryPendingCaptures()
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

    fun hasConsent(): Boolean = prefs.getBoolean(KEY_OPTED_IN, false)
    fun isEnabled(): Boolean = hasConsent() && missingPermissions().isEmpty()

    fun retryPendingCaptures() {
        io.execute {
            var offset = 0
            do {
                val page = repository.list(offset, 100)
                page.filter { it.purpose == CapturePurpose.NOTE && it.captureState == CaptureState.SAVED }.forEach { retryAutomatically(it.captureId) }
                offset += page.size
            } while (page.size == 100)
        }
    }

    fun finalizeCapture(captureId: String) {
        require(CaptureProtocol.isValidUuid(captureId)) { "Invalid capture ID" }
        val finalized = repository.update(captureId) { current ->
            require(current.purpose == CapturePurpose.NOTE) { "non-note-finalization-not-allowed" }
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
    fun retryCapture(captureId: String, sourceNodeId: String = "") {
        if (sourceNodeId.isNotBlank() && (!CaptureProtocol.isValidUuid(captureId) || !repository.isPhoneOrigin(captureId, sourceNodeId))) {
            outgoing.sendAuthorizationFailure(sourceNodeId, captureId); return
        }
        drainPendingArchive(captureId)
        scheduleReplay(captureId, sourceNodeId, automatic = false)
    }

    private fun retryAutomatically(captureId: String, sourceNodeId: String = "") = scheduleReplay(captureId, sourceNodeId, automatic = true)

    private fun scheduleReplay(captureId: String, sourceNodeId: String, automatic: Boolean) {
        if (!CaptureProtocol.isValidUuid(captureId)) return
        background.execute {
            if (sourceNodeId.isNotBlank() && !repository.isPhoneOrigin(captureId, sourceNodeId)) {
                outgoing.sendAuthorizationFailure(sourceNodeId, captureId); return@execute
            }
            if (!automatic) retryFailedRetirements()
            val record = recoverStaleRecognition(captureId) ?: return@execute
            if (record.purpose != CapturePurpose.NOTE || record.captureState != CaptureState.SAVED) return@execute

            val alreadyArchived = record.archiveState == ArchiveState.VERIFIED
            if (!alreadyArchived) archive.queue(record.captureId, sourceNodeId)
            if (record.recognitionState == RecognitionState.FINAL || record.recognitionState == RecognitionState.NO_SPEECH) {
                if (alreadyArchived) archive.maybeExport(record)
                return@execute
            }
            if (hasBusyRecognizer() || (automatic && !automaticRetryDue(record, Instant.now()))) return@execute
            val wav = record.localAudioPath?.let(::File)
            if (wav == null || !wav.isFile) {
                failSavedRecognition(record, "saved-wav-unavailable")
                return@execute
            }
            if (!runtimeReady()) {
                repository.update(record.captureId) { current -> current.copy(
                    recognitionState = RecognitionState.QUEUED,
                    lastErrorStage = "execution",
                    lastErrorCode = NativePhoneRequirements.runtimeUnavailableCode(appContext),
                ) }
                return@execute
            }
            val capabilities = speech.capabilities()
            if (!capabilities.externalAudio || !runCatching { speech.supportsPolicy(record.speechNetworkFallback, capabilities) }.getOrDefault(false) || record.format !in capabilities.formats) {
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
            val session = newSession(record, sessionId, sourceNodeId)
            if (!markRunning(record, session, automatic)) { session.timing.finish("attempt-not-admitted"); return@execute }
            try {
                if (!startOwnedRecognition(record, session)) return@execute
                val events = session.adapterEvents ?: return@execute
                WavPcmReader(wav, record.format).use { source ->
                    val expectedFrames = record.durableFrames.takeIf { it > 0 } ?: source.frameCount
                    check(source.frameCount == expectedFrames) { "saved-wav-frame-count-mismatch" }
                    while (true) {
                        if (sessions[sessionId] !== session) return@execute
                        val chunk = source.read(3_200) ?: break
                        if (!speech.appendOwned(sessionId, events, chunk)) return@execute
                        armRecognitionTimeout(session)
                        val delayMillis = replayChunkDelayMillis(chunk.size, record.format, replayPacingMillis)
                        if (delayMillis > 0) Thread.sleep(delayMillis)
                    }
                }
                if (sessions[sessionId] !== session) return@execute
                if (!speech.finishOwned(sessionId, events)) return@execute
                armRecognitionTimeout(session)
            } catch (_: Exception) {
                failSession(session, "saved-audio-transcription-failed")
            }
        }
    }

    internal fun beginLive(captureId: String, sessionId: String, sourceNodeId: String, format: CaptureFormat, startedAtUtc: String,
                           inputOwner: (() -> Unit)? = null, requestedLanguage: String? = null,
                           speechNetworkFallback: Boolean = false, purpose: CapturePurpose = CapturePurpose.NOTE): Boolean {
        if (!CaptureProtocol.isValidUuid(captureId) || !CaptureProtocol.isValidUuid(sessionId) || sourceNodeId.isBlank() ||
            purpose != CapturePurpose.NOTE || !CaptureProtocol.validateFormat(format) || startedAtUtc.length !in 1..64 ||
            !CaptureProtocol.validateRequestedLanguage(requestedLanguage)) return false
        val tupleValid = synchronized(sessions) {
            val input = liveInputs[sessionId]
            val active = sessions[sessionId]
            (input == null || (input.captureId.equals(captureId, true) && input.sourceNodeId == sourceNodeId)) &&
                (inputOwner == null || input?.close === inputOwner) &&
                (active == null || (active.live && active.captureId.equals(captureId, true) && active.sourceNodeId == sourceNodeId &&
                    (inputOwner == null || active.inputOwner === inputOwner)))
        }
        if (!tupleValid) { rejectLiveInput(captureId, sessionId, sourceNodeId); return false }
        val existing = repository.get(captureId)
        if (existing != null && (existing.purpose != purpose || existing.requestedLanguage != requestedLanguage ||
                existing.speechNetworkFallback != speechNetworkFallback)) {
            outgoing.sendReady(sourceNodeId, captureId, sessionId, "unavailable", false, "capture-policy-conflict")
            return false
        }
        if (existing != null && !repository.isPhoneOrigin(captureId, sourceNodeId)) {
            outgoing.sendAuthorizationFailure(sourceNodeId, captureId); return false
        }
        if (sessionId in terminalLiveSessions || (existing?.recognitionSessionId == sessionId &&
                existing.recognitionState in setOf(RecognitionState.FAILED, RecognitionState.UNAVAILABLE, RecognitionState.FINAL, RecognitionState.NO_SPEECH))) return false
        if (existing != null && existing.captureState != CaptureState.RECORDING) {
            outgoing.sendReady(sourceNodeId, captureId, sessionId, "unavailable", false)
            return false
        }
        val active = sessions[sessionId]
        if (active != null) {
            beforeDuplicateReady()
            if (active.captureId.equals(captureId, true) && active.sourceNodeId == sourceNodeId) {
                outgoing.sendReady(sourceNodeId, captureId, sessionId, if (active.accepted) "accepted" else "attempting", active.accepted)
            } else {
                outgoing.sendReady(sourceNodeId, captureId, sessionId, "unavailable", false)
            }
            return active.live && active.captureId.equals(captureId, true) && active.sourceNodeId == sourceNodeId && !active.terminalClaimed.get() && !active.inputEnded &&
                (inputOwner == null || ownedLiveSession(captureId, sessionId, sourceNodeId, inputOwner) === active)
        }
        val record = runCatching {
            repository.admitPhoneCapture(
                CaptureRecord(
                    captureId = captureId,
                    createdAtUtc = startedAtUtc,
                    startedAtUtc = startedAtUtc,
                    captureState = CaptureState.RECORDING,
                    format = format,
                    recognitionState = RecognitionState.QUEUED,
                    recognitionSessionId = sessionId,
                    purpose = purpose, requestedLanguage = requestedLanguage, speechNetworkFallback = speechNetworkFallback,
                ), sourceNodeId,
            )
        }.getOrElse {
            outgoing.sendReady(sourceNodeId, captureId, sessionId, "unavailable", false)
            return false
        } ?: run { outgoing.sendAuthorizationFailure(sourceNodeId, captureId); return false }
        if (record.format != format) {
            updateRecognitionFailure(record, sessionId, "capture-format-conflict")
            outgoing.sendReady(sourceNodeId, captureId, sessionId, "unavailable", false)
            return false
        }
        if (!runtimeReady()) {
            val code = NativePhoneRequirements.runtimeUnavailableCode(appContext)
            repository.update(captureId) { current ->
                if (current.recognitionSessionId != sessionId) current
                else current.copy(
                    recognitionState = RecognitionState.QUEUED,
                    lastErrorStage = "execution",
                    lastErrorCode = code,
                )
            }
            outgoing.sendReady(sourceNodeId, captureId, sessionId, "pending", false, code)
            return false
        }
        if (appContext.checkSelfPermission(NativePhoneRequirements.AUDIO_PERMISSION) != PackageManager.PERMISSION_GRANTED) {
            updateRecognitionFailure(record, sessionId, NativePhoneRequirements.AUDIO_ERROR)
            outgoing.sendReady(sourceNodeId, captureId, sessionId, "unavailable", false, NativePhoneRequirements.AUDIO_ERROR)
            return false
        }
        val caps = speech.capabilities()
        if (!caps.externalAudio || !runCatching { speech.supportsPolicy(record.speechNetworkFallback, caps) }.getOrDefault(false) || format !in caps.formats) {
            updateRecognitionFailure(record, sessionId, "external-audio-provider-unavailable")
            outgoing.sendReady(sourceNodeId, captureId, sessionId, "unavailable", false, "external-audio-provider-unavailable")
            return false
        }
        val session = newSession(record, sessionId, sourceNodeId)
        session.inputOwner = inputOwner
        if (!markRunning(record, session)) {
            outgoing.sendReady(sourceNodeId, captureId, sessionId, "pending", false, "recognition-busy")
            session.timing.finish("attempt-not-admitted"); return false
        }
        outgoing.sendReady(sourceNodeId, captureId, sessionId, "attempting", false)
        try {
            return startOwnedRecognition(record, session)
        } catch (_: Exception) {
            failSession(session, "external-audio-provider-rejected")
            return false
        }
    }

    internal fun appendLive(captureId: String, sessionId: String, sourceNodeId: String, inputOwner: () -> Unit, pcm: ByteArray) {
        val session = ownedLiveSession(captureId, sessionId, sourceNodeId, inputOwner) ?: throw CaptureLiveInputOwnershipException()
        if (!awaitLiveAdapter(session)) throw CaptureLiveInputOwnershipException()
        if (ownedLiveSession(captureId, sessionId, sourceNodeId, inputOwner) !== session) throw CaptureLiveInputOwnershipException()
        try {
            val events = session.adapterEvents ?: throw CaptureLiveInputOwnershipException()
            if (!speech.appendOwned(sessionId, events, pcm)) throw CaptureLiveInputOwnershipException()
            session.timing.mark("first-pcm-accepted")
            armRecognitionTimeout(session)
        } catch (failure: CaptureLiveInputOwnershipException) { throw failure }
        catch (_: Exception) {
            failSession(session, "external-audio-write-failed")
        }
    }

    internal fun registerLiveInput(captureId: String, sessionId: String, sourceNodeId: String, close: () -> Unit): Boolean =
        synchronized(sessions) {
            val active = sessions[sessionId]
            if (!CaptureProtocol.isValidUuid(sessionId) || sessionId in terminalLiveSessions || !canReceiveArchive(captureId, sourceNodeId)) false
            else if (active != null && (!active.live || !active.captureId.equals(captureId, true) || active.sourceNodeId != sourceNodeId ||
                    active.inputOwner != null || active.inputEnded || active.terminalClaimed.get())) false
            else if (liveInputs.putIfAbsent(sessionId, LiveInput(captureId.lowercase(), sourceNodeId, close)) != null) false
            else { active?.inputOwner = close; true }
        }

    private fun ownedLiveSession(captureId: String, sessionId: String, sourceNodeId: String, inputOwner: () -> Unit): RecognitionSession? =
        synchronized(sessions) {
            val input = liveInputs[sessionId]
            sessions[sessionId]?.takeIf { current ->
                current.live && current.captureId.equals(captureId, true) && current.sourceNodeId == sourceNodeId &&
                    current.inputOwner === inputOwner && input != null && input.captureId.equals(captureId, true) && input.sourceNodeId == sourceNodeId &&
                    input.close === inputOwner && !current.terminalClaimed.get() && !current.inputEnded
            }
        }

    internal fun rejectLiveInput(captureId: String, sessionId: String, sourceNodeId: String) =
        outgoing.sendReady(sourceNodeId, captureId, sessionId, "live-input-ownership-rejected", false)

    internal fun tryBeginRecognitionProbe(sessionId: String, cancel: () -> Unit): Boolean {
        retryFailedRetirements()
        return synchronized(sessions) {
        if (runtimeStopped || !runtimeReady() || admissionInProgress || failedRetirements.isNotEmpty() || failedProbeRetirement != null ||
            probeSessionId != null || sessions.isNotEmpty() || liveInputs.isNotEmpty()) false
        else {
            val token = recognitionLeases.tryAcquire(RecognitionResourceLease.Kind.CAPTURE_PROBE)
            if (token == null) false
            else {
                val observer = object : ProbeRetirementObserver {
                    override fun allocated(sdkOwner: Any): Boolean = synchronized(sessions) {
                        if (probeSessionId != sessionId || probeLease !== token || probeSdkOwner != null) false
                        else { probeSdkOwner = sdkOwner; true }
                    }
                    override fun failed(sdkOwner: Any, retry: () -> Boolean) {
                        synchronized(sessions) {
                            if (probeSessionId != sessionId || probeLease !== token || probeSdkOwner !== sdkOwner) return
                            if (failedProbeRetirement?.sdkOwner !== sdkOwner) failedProbeRetirement = FailedProbeRetirement(sdkOwner, retry)
                        }
                        token.onRetirementRetry { retryFailedRetirements() }
                    }
                    override fun retired(sdkOwner: Any) { releaseProbe(sessionId, token, sdkOwner) }
                }
                if (probeRetirementOwners.putIfAbsent(sessionId, observer) != null) { token.close(); false }
                else {
                    probeSessionId = sessionId; cancelProbe = cancel; probeLease = token
                    probeRetirementObserver = observer; true
                }
            }
        }
        }
    }

    internal fun endRecognitionProbe(sessionId: String) {
        releaseProbe(sessionId)
    }

    private fun releaseProbe(sessionId: String, expected: RecognitionResourceLease.Token? = null, sdkOwner: Any? = null) {
        val released = synchronized(sessions) {
            if (probeSessionId != sessionId || (expected != null && probeLease !== expected) ||
                (sdkOwner != null && probeSdkOwner !== sdkOwner)) null
            else {
                val token = probeLease
                probeRetirementObserver?.let { probeRetirementOwners.remove(sessionId, it) }
                probeSessionId = null; cancelProbe = null; probeLease = null
                probeRetirementObserver = null; probeSdkOwner = null; failedProbeRetirement = null
                token
            }
        }
        if (released != null) {
            released.close()
            if (!io.isShutdown) runCatching { retryPendingCaptures() }
        }
    }

    internal fun onRuntimeStarted() {
        synchronized(sessions) { runtimeStopped = false }
        retryFailedRetirements()
    }

    internal fun onRuntimeStopped() {
        retryFailedRetirements()
        val (owned, probe) = synchronized(sessions) {
            runtimeStopped = true
            sessions.values.toList() to cancelProbe
        }
        probe?.invoke()
        owned.forEach { failSession(it, "phone-runtime-stopped") }
    }

    internal fun releaseLiveInput(captureId: String, sessionId: String, sourceNodeId: String, close: () -> Unit): Boolean = synchronized(sessions) {
        val input = liveInputs[sessionId] ?: return@synchronized false
        if (!input.captureId.equals(captureId, true) || input.sourceNodeId != sourceNodeId || input.close !== close) false
        else liveInputs.remove(sessionId, input)
    }

    internal fun failLiveInput(captureId: String, sessionId: String, sourceNodeId: String, inputOwner: () -> Unit, code: String) {
        val session = synchronized(sessions) {
            val current = ownedLiveSession(captureId, sessionId, sourceNodeId, inputOwner)
            if (current != null) {
                terminalLiveSessions.add(sessionId)
                current
            } else null
        }
        if (session != null) failSession(session, code)
    }

    private fun closeLiveInput(session: RecognitionSession) {
        val input = synchronized(sessions) {
            liveInputs[session.sessionId]?.takeIf { it.captureId == session.captureId && it.sourceNodeId == session.sourceNodeId &&
                it.close === session.inputOwner && liveInputs.remove(session.sessionId, it) }
        }
        input?.let { runCatching { it.close() } }
    }

    internal fun finishLive(captureId: String, sessionId: String, sourceNodeId: String, inputOwner: () -> Unit, frameCount: Long) {
        if (frameCount < 0) return
        val session = ownedLiveSession(captureId, sessionId, sourceNodeId, inputOwner) ?: throw CaptureLiveInputOwnershipException()
        if (!awaitLiveAdapter(session)) throw CaptureLiveInputOwnershipException()
        synchronized(sessions) {
            if (ownedLiveSession(captureId, sessionId, sourceNodeId, inputOwner) !== session) throw CaptureLiveInputOwnershipException()
            repository.update(captureId) { current ->
                if (current.recognitionSessionId == sessionId) current.copy(observedFrames = frameCount) else current
            }
            session.inputEnded = true
        }
        val events = session.adapterEvents ?: throw CaptureLiveInputOwnershipException()
        if (!speech.finishOwned(sessionId, events)) throw CaptureLiveInputOwnershipException()
        armRecognitionTimeout(session)
        drainPendingArchive(captureId)
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
        val finalizedByteLength = record.finalizedByteLength
        if (record.purpose != CapturePurpose.NOTE || !CaptureProtocol.isValidUuid(record.captureId) || record.sha256?.matches(Regex("(?i)^[0-9a-f]{64}$")) != true ||
             finalizedByteLength == null || finalizedByteLength < 44 ||
            record.captureState !in setOf(CaptureState.SAVED, CaptureState.INTERRUPTED)) {
            throw IllegalArgumentException("invalid-finalized-capture-record")
        }
        if (!canReceiveArchive(record.captureId, sourceNodeId)) throw CaptureOriginAuthorizationException()
        if (repository.admitPhoneCapture(record.copy(transferState = TransferState.PHONE_STAGED, archiveState = ArchiveState.STAGED),
                sourceNodeId, verifiedNewArchive = true) == null) throw CaptureOriginAuthorizationException()
        val deferUntilLiveEof = synchronized(sessions) {
            val live = sessions.values.firstOrNull {
                it.captureId == record.captureId && it.live && !it.inputEnded && !it.terminalClaimed.get()
            }
            if (live == null) false else {
                val previous = pendingLiveArchives[record.captureId]
                check(previous == null || (previous.sourceNodeId == sourceNodeId &&
                    previous.record.sha256 == record.sha256 && previous.record.finalizedByteLength == record.finalizedByteLength)) {
                    "capture-id-content-conflict"
                }
                pendingLiveArchives[record.captureId] = PendingArchive(record, sourceNodeId)
                true
            }
        }
        if (deferUntilLiveEof) return
        val stored = repository.get(record.captureId)
        require(stored == null || (stored.purpose == record.purpose && stored.requestedLanguage == record.requestedLanguage &&
            stored.speechNetworkFallback == record.speechNetworkFallback)) { "capture-policy-conflict" }
        val incompleteLiveAudio = stored?.sha256 == null && stored?.recognitionSessionId != null &&
            stored.observedFrames != record.durableFrames
        if (incompleteLiveAudio) {
            stored?.recognitionSessionId?.let { previousId ->
                sessions.remove(previousId)?.let { previous ->
                    previous.terminalClaimed.set(true); previous.timeout?.cancel(false); retireNative(previous)
                }
            }
        }
        if (stored?.sha256 != null &&
            (!stored.sha256.equals(record.sha256, ignoreCase = true) || stored.finalizedByteLength != record.finalizedByteLength)) {
            throw IllegalStateException("capture-id-content-conflict")
        }
        if (stored != null && stored.sha256 == null && stored.localAudioPath != null) {
            throw IllegalStateException("capture-id-has-nonprovisional-record")
        }
        repository.update(record.captureId) { current ->
            val locallyFinalized = current.sha256 != null && current.captureState == CaptureState.SAVED &&
                current.sha256.equals(record.sha256, ignoreCase = true) && current.finalizedByteLength == record.finalizedByteLength
            check(current.sha256 == null || (current.sha256.equals(record.sha256, ignoreCase = true) &&
                current.finalizedByteLength == record.finalizedByteLength)) { "capture-id-content-conflict" }
            check(current.sha256 != null || current.localAudioPath == null) { "capture-id-has-nonprovisional-record" }
            current.copy(
                sourceKind = record.sourceKind,
                startedAtUtc = current.startedAtUtc ?: record.startedAtUtc,
                finishedAtUtc = record.finishedAtUtc,
                finishedLocalDate = record.finishedLocalDate,
                timezoneId = record.timezoneId,
                captureState = if (locallyFinalized) current.captureState else record.captureState,
                stopReason = if (locallyFinalized) current.stopReason else record.stopReason ?: current.stopReason,
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
        val merged = repository.get(record.captureId) ?: return
        if (merged.transferState == TransferState.PHONE_ARCHIVED && merged.archivedAudioUri != null) {
            outgoing.sendReceipt(sourceNodeId, merged, "PHONE_ARCHIVED", merged.exportDestinationId)
            archive.maybeExport(merged)
        } else {
            outgoing.sendReceipt(sourceNodeId, merged, "PHONE_STAGED")
            archive.queue(record.captureId, sourceNodeId)
        }
        if (merged.captureState == CaptureState.SAVED &&
            merged.recognitionState in RETRYABLE_RECOGNITION_STATES && !hasActiveSession(merged.captureId)) {
            retryAutomatically(merged.captureId, sourceNodeId)
        }
        stored?.recognitionSessionId?.let { terminalLiveSessions.remove(it) }
    }

    internal fun respondStatus(captureId: String, sourceNodeId: String) {
        io.execute {
            if (!CaptureProtocol.isValidUuid(captureId)) return@execute
            if (!repository.isPhoneOrigin(captureId, sourceNodeId)) {
                outgoing.sendAuthorizationFailure(sourceNodeId, captureId); return@execute
            }
            val record = repository.get(captureId) ?: return@execute
            if (record.purpose != CapturePurpose.NOTE) return@execute
            if (record.sha256 == null) return@execute
            outgoing.sendReceipt(sourceNodeId, record, if (record.transferState == TransferState.PHONE_ARCHIVED) "PHONE_ARCHIVED" else "PHONE_STAGED")
            if (record.captureState == CaptureState.SAVED && record.recognitionState in RETRYABLE_RECOGNITION_STATES &&
                !hasBusyRecognizer() && runtimeReady()) {
                retryAutomatically(captureId, sourceNodeId)
            }
        }
    }

    internal fun canReceiveArchive(captureId: String, sourceNodeId: String): Boolean =
        CaptureProtocol.isValidUuid(captureId) && sourceNodeId.isNotBlank() && sourceNodeId.length <= 256 &&
            (repository.get(captureId)?.let { it.purpose == CapturePurpose.NOTE && repository.isPhoneOrigin(captureId, sourceNodeId) } ?: true)

    internal fun canReuseStagedArchive(captureId: String, sourceNodeId: String): Boolean =
        CaptureProtocol.isValidUuid(captureId) && repository.get(captureId)?.purpose == CapturePurpose.NOTE && repository.isPhoneOrigin(captureId, sourceNodeId)

    internal fun rejectOrigin(captureId: String, sourceNodeId: String) = outgoing.sendAuthorizationFailure(sourceNodeId, captureId)

    private fun drainPendingArchive(captureId: String) {
        val pending = synchronized(sessions) {
            if (sessions.values.any { it.captureId == captureId && it.live && !it.inputEnded && !it.terminalClaimed.get() }) null
            else pendingLiveArchives.remove(captureId)
        } ?: return
        fun retainForRetry() {
            synchronized(sessions) { pendingLiveArchives.putIfAbsent(captureId, pending) }
        }
        try {
            io.execute {
                try { onArchiveReceived(pending.record, pending.sourceNodeId) }
                catch (_: Exception) {
                    retainForRetry()
                    runCatching { markArchiveFailure(captureId, "pending-archive-processing-failed") }
                }
            }
        } catch (_: java.util.concurrent.RejectedExecutionException) {
            retainForRetry()
        }
    }

    private fun awaitLiveAdapter(session: RecognitionSession): Boolean {
        if (!session.adapterInitialized.await(10, TimeUnit.SECONDS)) {
            failSession(session, "speech-adapter-initialization-timeout")
            return false
        }
        return session.adapterReady && sessions[session.sessionId] === session && !session.terminalClaimed.get()
    }

    internal fun markArchiveFailure(captureId: String, code: String) {
        repository.update(captureId) { current ->
            if (current.transferState == TransferState.PHONE_ARCHIVED) current
            else current.copy(archiveState = ArchiveState.RETRYABLE_ERROR, lastErrorStage = "archive", lastErrorCode = CaptureErrorCodes.safe(code, "archive-failed"))
        }
    }

    internal fun awaitIdle(timeout: Long = 5, unit: TimeUnit = TimeUnit.SECONDS): Boolean {
        val deadline = System.nanoTime() + unit.toNanos(timeout)
        repeat(2) {
            val done = java.util.concurrent.CountDownLatch(1)
            io.execute { background.execute { archive.barrier { io.execute { outgoing.barrier { done.countDown() } } } } }
            if (!done.await((deadline - System.nanoTime()).coerceAtLeast(0), TimeUnit.NANOSECONDS)) return false
        }
        return true
    }

    internal fun shutdownForTests() {
        leaseSubscription.close()
        sessions.values.toList().forEach { retireNative(it) }
        sessions.clear()
        retryFailedRetirements()
        if (failedProbeRetirement == null) probeSessionId?.let { releaseProbe(it) }
        liveInputs.values.forEach { runCatching { it.close() } }
        liveInputs.clear()
        terminalLiveSessions.clear()
        pendingLiveArchives.clear()
        background.shutdownNow()
        archive.shutdown()
        outgoing.shutdown()
        io.shutdownNow()
        timeoutExecutor.shutdownNow()
    }

    private fun newSession(record: CaptureRecord, sessionId: String, sourceNodeId: String): RecognitionSession {
        val reducer = TranscriptReducer(record.captureId)
        reducer.beginSession(sessionId)
        val timing = CaptureTiming(appContext, "phone-recognition").also { it.mark("attempt-created") }
        timing.mark(if (record.captureState == CaptureState.RECORDING) "mode-live" else "mode-replay")
        return RecognitionSession(record.captureId, sessionId, sourceNodeId, reducer, record.captureState == CaptureState.RECORDING, timing)
    }

    private fun markRunning(record: CaptureRecord, session: RecognitionSession, automatic: Boolean = false): Boolean {
        if (!automatic) retryFailedRetirements()
        val retired = mutableListOf<RecognitionSession>()
        val reserved = synchronized(sessions) {
            if (session.live && !validInputAdmission(session)) return@synchronized false
            if (runtimeStopped || !runtimeReady() || probeSessionId != null || admissionInProgress || failedRetirements.isNotEmpty() || failedProbeRetirement != null) return@synchronized false
            if (session.live && session.sessionId in terminalLiveSessions) return@synchronized false
            if (sessions.values.any { it.captureId == record.captureId }) return@synchronized false
            if (!session.live && sessions.isNotEmpty()) return@synchronized false
            if (recognitionLeases.current()?.kind in setOf(RecognitionResourceLease.Kind.CHAT,
                    RecognitionResourceLease.Kind.WATCH_NOTE_AUDIO, RecognitionResourceLease.Kind.CAPTURE_PROBE)) return@synchronized false
            if (session.live) {
                if (sessions.values.any { it.live && !it.inputEnded && !it.terminalClaimed.get() }) return@synchronized false
                sessions.values.filter { !it.live || it.inputEnded }.forEach { replay ->
                    if (!replay.terminalClaimed.compareAndSet(false, true)) return@forEach
                    sessions.remove(replay.sessionId, replay)
                    if (replay.live) terminalLiveSessions.add(replay.sessionId)
                    replay.timeout?.cancel(false)
                    replay.timing.finish("recognition-preempted")
                    retired += replay
                }
            }
            admissionInProgress = true
            true
        }
        if (!reserved) return false
        var admitted = false
        try {
            // Keep old admission through cancellation. SDK callbacks never run under either monitor.
            var retiredCleanly = true
            retired.forEach { previous ->
                closeLiveInput(previous)
                if (!retireNative(previous, release = false)) retiredCleanly = false
                repository.update(previous.captureId) { current ->
                    if (current.recognitionSessionId != previous.sessionId ||
                        current.recognitionState in setOf(RecognitionState.FINAL, RecognitionState.NO_SPEECH)) current
                    else current.copy(recognitionState = RecognitionState.QUEUED, recognitionSessionId = null)
                }
            }
            if (!retiredCleanly) return false
            if (runtimeStopped || !runtimeReady()) return false
            session.lease = retired.firstNotNullOfOrNull { it.lease?.handoff(RecognitionResourceLease.Kind.CAPTURE_LIVE) }
                ?: recognitionLeases.tryAcquire(if (session.live) RecognitionResourceLease.Kind.CAPTURE_LIVE else RecognitionResourceLease.Kind.CAPTURE_REPLAY)
            if (session.lease == null) return false
            admitted = synchronized(sessions) {
            if (session.live && !validInputAdmission(session)) return@synchronized false
            if (runtimeStopped || !runtimeReady() || probeSessionId != null ||
                (session.live && session.sessionId in terminalLiveSessions)) return@synchronized false
            val changed = repository.update(record.captureId) { current ->
                val at = Instant.now()
                if (current.captureState != if (session.live) CaptureState.RECORDING else CaptureState.SAVED) current
                else if (!session.live && (current.recognitionState !in RETRYABLE_RECOGNITION_STATES ||
                        (automatic && !automaticRetryDue(current, at)))) current
                else current.copy(
                    recognitionState = RecognitionState.RUNNING,
                    recognitionSessionId = session.sessionId,
                    engineId = "android-speech-external-audio",
                    lastErrorStage = null,
                    lastErrorCode = null,
                    retryCount = if (automatic) current.retryCount + 1 else current.retryCount,
                    nextRetryAtUtc = if (automatic) at.plusSeconds(recognitionRetryDelaySeconds(current.retryCount)).toString() else current.nextRetryAtUtc,
                )
            }
            if (changed == null || changed.captureState != (if (session.live) CaptureState.RECORDING else CaptureState.SAVED) ||
                changed.recognitionSessionId != session.sessionId || changed.recognitionState != RecognitionState.RUNNING) return@synchronized false
            if (session.live && session.inputOwner == null) session.inputOwner = liveInputs[session.sessionId]?.close
            sessions[session.sessionId] = session
            armRecognitionTimeout(session)
            true
            }
            return admitted
        } finally {
            synchronized(sessions) { admissionInProgress = false }
            retired.filter { it.nativeRetired.get() }.forEach { it.lease?.close() }
            if (!admitted) { session.lease?.close(); session.lease = null }
        }
    }

    private fun startOwnedRecognition(record: CaptureRecord, session: RecognitionSession): Boolean {
        session.timing.mark("adapter-start-requested")
        try {
            if (sessions[session.sessionId] !== session || session.terminalClaimed.get()) return false
            val events = eventsFor(session)
            session.adapterEvents = events
            speech.startWithPolicy(session.sessionId, record.format, record.requestedLanguage ?: record.language,
                record.speechNetworkFallback, events)
            session.timing.mark("adapter-start-returned")
            session.adapterReady = sessions[session.sessionId] === session && !session.terminalClaimed.get()
        } finally { session.adapterInitialized.countDown() }
        if (sessions[session.sessionId] === session) return true
        retireNative(session)
        return false
    }

    private fun validInputAdmission(session: RecognitionSession): Boolean {
        val input = liveInputs[session.sessionId]
        return (input == null || (input.captureId == session.captureId && input.sourceNodeId == session.sourceNodeId)) &&
            (session.inputOwner == null || input?.close === session.inputOwner)
    }

    private fun automaticRetryDue(record: CaptureRecord, at: Instant): Boolean {
        val due = record.nextRetryAtUtc?.let { runCatching { Instant.parse(it) }.getOrNull() }
        return record.retryCount < MAX_RECOGNITION_RETRIES && (due == null || !due.isAfter(at))
    }

    /** Recover a persisted RUNNING state only when this coordinator has no live session for it. */
    private fun recoverStaleRecognition(captureId: String): CaptureRecord? = synchronized(sessions) {
        val current = repository.get(captureId) ?: return@synchronized null
        if (current.purpose != CapturePurpose.NOTE) return@synchronized null
        if (current.recognitionState != RecognitionState.RUNNING) return@synchronized current
        if (sessions.values.any { it.captureId == captureId }) return@synchronized null
        repository.update(captureId) { latest ->
            if (latest.recognitionState != RecognitionState.RUNNING || sessions.values.any { it.captureId == captureId }) latest
            else latest.copy(recognitionState = RecognitionState.QUEUED, recognitionSessionId = null)
        }?.takeUnless { it.recognitionState == RecognitionState.RUNNING }
    }

    private fun hasActiveSession(captureId: String): Boolean = synchronized(sessions) {
        sessions.values.any { it.captureId == captureId }
    }

    private fun hasBusyRecognizer(): Boolean = synchronized(sessions) {
        sessions.isNotEmpty() || probeSessionId != null || runtimeStopped || admissionInProgress ||
            failedRetirements.isNotEmpty() || recognitionLeases.current() != null
    }

    private fun armRecognitionTimeout(session: RecognitionSession) {
        synchronized(session) {
            if (session.terminalClaimed.get()) return
            session.timeout?.cancel(false)
            session.timeout = timeoutExecutor.schedule(
                { failSession(session, "speech-recognizer-timeout") },
                recognitionTimeoutSeconds,
                TimeUnit.SECONDS,
            )
        }
    }

    private fun eventsFor(session: RecognitionSession) = object : SpeechRecognizerAdapter.Events {
        override fun isCurrent(): Boolean = !runtimeStopped && sessions[session.sessionId] === session && !session.terminalClaimed.get()

        override fun onAttemptState(state: String) {
            if (!isCurrent()) return
            if (state in setOf("provider-ready", "provider-speech-begin", "provider-first-partial", "provider-first-segment",
                    "provider-final", "provider-segmented-end", "provider-pipe-first-write", "provider-partial-callback",
                    "provider-empty-partial", "provider-segment-callback", "provider-empty-segment", "provider-speech-end",
                    "provider-input-backlog-over-1s", "provider-write-blocked-250ms")) session.timing.mark(state)
            if (state == "provider-ready" && session.sourceNodeId.isNotBlank()) {
                outgoing.sendReady(session.sourceNodeId, session.captureId, session.sessionId, "provider-ready", false)
            }
        }

        override fun onHypothesis(text: String, isFinal: Boolean) {
            if (sessions[session.sessionId] !== session || session.terminalClaimed.get()) return
            if (isFinal && !session.terminalClaimed.compareAndSet(false, true)) return
            if (isFinal) retireNative(session)
            session.revision += 1
            val snapshot = session.reducer.apply(
                TranscriptUpdate(session.captureId, session.sessionId, session.revision, text, isFinal),
            )
            if (text.isNotBlank()) session.timing.mark("first-hypothesis")
            if (isFinal) session.timing.mark("final-received")
            val becameAccepted = !session.accepted && text.isNotBlank()
            session.accepted = session.accepted || text.isNotBlank()
            io.execute {
                var persisted = false
                try {
                    if (!isFinal && !isCurrent()) return@execute
                    if (text.isNotBlank()) session.timing.mark("first-hypothesis-io-started")
                    val updated = try {
                        if (repository.get(session.captureId)?.recognitionSessionId != session.sessionId) return@execute
                        repository.update(session.captureId) { current ->
                            if (current.recognitionSessionId != session.sessionId || (!isFinal && !isCurrent())) current
                            else current.copy(
                                recognitionState = if (isFinal) if (text.isBlank()) RecognitionState.NO_SPEECH else RecognitionState.FINAL else RecognitionState.RUNNING,
                                previewText = snapshot.preview?.takeIf { it.isNotBlank() } ?: current.previewText,
                                finalText = snapshot.finalText?.takeIf { it.isNotBlank() },
                                lastRevision = snapshot.revision,
                                transcriptVersion = if (isFinal && !text.isBlank()) current.transcriptVersion + 1 else current.transcriptVersion,
                                retryCount = if (isFinal) 0 else current.retryCount,
                                nextRetryAtUtc = if (isFinal) null else current.nextRetryAtUtc,
                                liveTranscriptionState = if (isFinal) {
                                    if (text.isBlank()) LiveTranscriptionState.UNAVAILABLE else LiveTranscriptionState.COMPLETED
                                } else if (text.isNotBlank()) LiveTranscriptionState.TRANSCRIBING else current.liveTranscriptionState,
                                canTranscribeLive = !isFinal && (text.isNotBlank() || current.canTranscribeLive),
                                liveErrorCode = if (isFinal && text.isBlank()) "no-speech-detected" else null,
                                lastErrorStage = null,
                                lastErrorCode = null,
                            )
                        }
                    } catch (failure: Exception) {
                        if (!isFinal) throw failure
                        markFinalPersistenceFailure(session)
                        return@execute
                    }
                    if (updated?.recognitionSessionId != session.sessionId || updated.lastRevision != snapshot.revision) return@execute
                    persisted = true
                    if (text.isNotBlank()) session.timing.mark("first-hypothesis-persisted")
                    if (session.sourceNodeId.isNotBlank()) {
                        if (becameAccepted) outgoing.sendReady(session.sourceNodeId, session.captureId, session.sessionId, "accepted", true)
                        sendTranscript(session, text, isFinal, snapshot.revision)
                    }
                    if (isFinal) {
                        if (updated.sha256 != null && session.sourceNodeId.isNotBlank()) {
                            outgoing.sendReceipt(session.sourceNodeId, updated, if (updated.transferState == TransferState.PHONE_ARCHIVED) "PHONE_ARCHIVED" else "PHONE_STAGED")
                        }
                        archive.maybeExport(updated)
                    }
                } finally {
                    if (isFinal) {
                        settleFinalSession(session)
                        session.timing.finish(if (persisted) "final-persisted" else "final-not-persisted")
                        if (persisted) retryPendingCaptures()
                    }
                }
            }
        }

        override fun onFailure(code: String) = failSession(session, code)
    }

    private fun settleFinalSession(session: RecognitionSession) {
        synchronized(session) { session.timeout?.cancel(false) }
        val removed = synchronized(sessions) {
            sessions.remove(session.sessionId, session).also { removed ->
                if (removed && session.live) terminalLiveSessions.add(session.sessionId)
            }
        }
        if (removed) {
            closeLiveInput(session)
            drainPendingArchive(session.captureId)
        }
        // SDK retirement/quarantine is independent of logical persistence settlement.
    }

    private fun markFinalPersistenceFailure(session: RecognitionSession) {
        runCatching {
            val updated = repository.update(session.captureId) { current ->
                if (current.recognitionSessionId != session.sessionId ||
                    current.recognitionState in setOf(RecognitionState.FINAL, RecognitionState.NO_SPEECH)) current
                else current.copy(recognitionState = RecognitionState.FAILED, lastErrorStage = "recognition",
                    lastErrorCode = "final-transcript-persistence-failed")
            }
            if (updated?.recognitionSessionId == session.sessionId && updated.recognitionState == RecognitionState.FAILED &&
                session.sourceNodeId.isNotBlank()) {
                outgoing.sendReady(session.sourceNodeId, session.captureId, session.sessionId, "unavailable", false, "final-transcript-persistence-failed")
                if (updated.sha256 != null) {
                    outgoing.sendReceipt(session.sourceNodeId, updated,
                        if (updated.transferState == TransferState.PHONE_ARCHIVED) "PHONE_ARCHIVED" else "PHONE_STAGED")
                }
            }
        }
    }

    private fun failSession(session: RecognitionSession, code: String) {
        if (!session.terminalClaimed.compareAndSet(false, true)) return
        if (session.live) terminalLiveSessions.add(session.sessionId)
        closeLiveInput(session)
        session.timing.finish("recognition-failed")
        if (sessions.remove(session.sessionId, session)) {
            session.timeout?.cancel(false)
            retireNative(session)
            io.execute {
                val updated = repository.update(session.captureId) { current ->
                    if (current.recognitionSessionId != session.sessionId || current.recognitionState == RecognitionState.FINAL) current
                    else current.copy(
                        recognitionState = RecognitionState.FAILED,
                        lastErrorStage = "recognition",
                        lastErrorCode = CaptureErrorCodes.safe(code, "transcription-unavailable"),
                    )
                }
                if (session.sourceNodeId.isNotBlank()) outgoing.sendReady(session.sourceNodeId, session.captureId, session.sessionId, "unavailable", false, code)
                if (updated?.sha256 != null && session.sourceNodeId.isNotBlank()) {
                    outgoing.sendReceipt(session.sourceNodeId, updated, if (updated.transferState == TransferState.PHONE_ARCHIVED) "PHONE_ARCHIVED" else "PHONE_STAGED")
                }
                updated?.let(archive::maybeExport)
                drainPendingArchive(session.captureId)
            }
        }
    }

    private fun retireNative(session: RecognitionSession, release: Boolean = true): Boolean {
        if (session.nativeRetired.get()) return true
        if (!session.retirementClaimed.compareAndSet(false, true)) return false
        try {
            val cleaned = runCatching {
                val owner = session.adapterEvents
                if (owner == null) speech.cancel(session.sessionId) else speech.cancelOwned(session.sessionId, owner)
            }.isSuccess
            session.nativeRetired.set(cleaned)
            if (cleaned) {
                failedRetirements.remove(session.sessionId, session)
                if (release) session.lease?.close()
            } else {
                failedRetirements[session.sessionId] = session
                session.lease?.onRetirementRetry { retryFailedRetirements() }
            }
            return cleaned
        } finally { session.retirementClaimed.set(false) }
    }

    /** Explicit bounded retry; never holds session/lease admission locks across SDK teardown. */
    fun retryFailedRetirements(): Boolean {
        var recovered = false
        failedRetirements.values.toList().forEach { session ->
            if (retireNative(session)) recovered = true
        }
        val probe = synchronized(sessions) { failedProbeRetirement }
        if (probe != null && probe.inProgress.compareAndSet(false, true)) {
            try { if (runCatching(probe.retry).getOrDefault(false)) recovered = true }
            finally { probe.inProgress.set(false) }
        }
        if (recovered && !runtimeStopped && !io.isShutdown) retryPendingCaptures()
        return failedRetirements.isEmpty() && synchronized(sessions) { failedProbeRetirement == null }
    }

    private fun updateRecognitionFailure(record: CaptureRecord, sessionId: String, reason: String) {
        repository.update(record.captureId) { current ->
            if (current.recognitionSessionId != sessionId) current
            else current.copy(
                recognitionState = RecognitionState.UNAVAILABLE,
                lastErrorStage = "recognition",
                lastErrorCode = CaptureErrorCodes.safe(reason, "transcription-unavailable"),
            )
        }
    }

    private fun failSavedRecognition(record: CaptureRecord, reason: String, sessionId: String? = null) {
        repository.update(record.captureId) { current ->
            if (sessionId != null && current.recognitionSessionId != sessionId) current
            else current.copy(
                recognitionState = RecognitionState.FAILED,
                lastErrorStage = "recognition",
                lastErrorCode = CaptureErrorCodes.safe(reason, "transcription-unavailable"),
            )
        }
    }

    private fun sendTranscript(session: RecognitionSession, text: String, final: Boolean, revision: Long) {
        outgoing.sendTranscript(session.captureId, session.sessionId, session.sourceNodeId,
            session.transcriptDelivery, session.timing, text, final, revision)
    }

    private fun missingPermissions(): List<String> = NativePhoneRequirements.missing(appContext).map { it.permission }

    private fun isWatch(): Boolean = NativePhoneRequirements.isWatch(appContext)

    internal companion object {
        const val PREFERENCES = "capture_phone_runtime"
        const val KEY_OPTED_IN = "phone_capture_opted_in"
        const val KEY_RUNTIME_ERROR = "phone_capture_runtime_error"
        private const val MAX_CAPTURE_MILLIS = 10 * 60 * 1_000L
        private const val MAX_RECOGNITION_RETRIES = 5
        internal fun replayChunkDelayMillis(bytes: Int, format: CaptureFormat, nominalChunkMillis: Long): Long =
            bytes.toLong() * nominalChunkMillis * 10 / (format.sampleRate.toLong() * format.channels * format.bitsPerSample / 8)
        private val RETRYABLE_RECOGNITION_STATES = setOf(RecognitionState.QUEUED, RecognitionState.FAILED, RecognitionState.UNAVAILABLE, RecognitionState.RUNNING)

        private fun recognitionRetryDelaySeconds(retryCount: Int): Long =
            (30L shl retryCount.coerceIn(0, 4)).coerceAtMost(300L)

        @Volatile private var instance: PhoneCaptureCoordinator? = null
        private val probeRetirementOwners = ConcurrentHashMap<String, ProbeRetirementObserver>()
        internal fun probeRetirementObserver(sessionId: String): ProbeRetirementObserver? = probeRetirementOwners[sessionId]

        fun shared(context: Context): PhoneCaptureCoordinator = instance ?: synchronized(this) {
            instance ?: PhoneCaptureCoordinator(context.applicationContext).also { instance = it }
        }

        internal fun resetSharedForTests() = synchronized(this) { instance = null }
    }
}
