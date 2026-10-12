package dev.gitblame.voxgolem.assistant.recognition

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.os.Handler
import android.os.Looper
import dev.gitblame.voxgolem.assistant.AssistantTiming
import dev.gitblame.voxgolem.assistant.AssistantTiming.Operation
import dev.gitblame.voxgolem.assistant.AssistantTiming.Stage
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.atomic.AtomicLong
import dev.gitblame.voxgolem.assistant.speech.SpeechAdmission
import dev.gitblame.voxgolem.assistant.speech.SpeechOptions

interface LocalRecognizerEvents {
    fun ready() {}
    fun partial(text: String)
    fun final(text: String)
    fun error(code: String)
}
interface LocalRecognizerSession {
    val route: RecognitionRoute? get() = null
    val archiveExpected: Boolean get() = false
    val recordedAudio: RecordedAudioResult? get() = null
    fun checkLanguage(language: String, result: (Boolean) -> Unit)
    fun checkSelectedLanguage(language: String, explicit: Boolean, result: (Boolean) -> Unit) = checkLanguage(language, result)
    fun start(language: String, events: LocalRecognizerEvents)
    fun stop()
    fun cancel()
    fun destroy()
}
interface LocalRecognizerFactory {
    fun available(): Boolean
    fun localOnlyAvailable(): Boolean = false
    fun availableForPolicy(networkFallback: Boolean): Boolean = available()
    fun create(): LocalRecognizerSession
    fun create(timing: AssistantTiming): LocalRecognizerSession = create()
    fun createForPolicy(networkFallback: Boolean): LocalRecognizerSession = create()
    fun createForPolicy(networkFallback: Boolean, timing: AssistantTiming): LocalRecognizerSession = createForPolicy(networkFallback)
    fun recordingEnabled(): Boolean = false
    fun recordingAvailable(language: String, networkFallback: Boolean): Boolean = false
    fun availableForEntry(language: String, networkFallback: Boolean, recording: Boolean = recordingEnabled()): Boolean = availableForPolicy(networkFallback)
    fun createForEntry(purpose: VoiceCapturePurpose, language: String, networkFallback: Boolean,
        timing: AssistantTiming, recording: Boolean = recordingEnabled()): LocalRecognizerSession = createForPolicy(networkFallback, timing)
}

enum class DictationPhase { IDLE, CHECKING, LISTENING, FINISHING, FINAL, ERROR }
data class DictationState(val phase: DictationPhase = DictationPhase.IDLE, val draft: String = "", val error: String? = null,
    val utterance: Long = 0, val route: RecognitionRoute? = null, val endedWithoutSpeech: Boolean = false,
    val readyForSpeech: Boolean = false, val diagnosticError: String? = error,
    val recordedAudio: RecordedAudioResult? = null, val archiveExpected: Boolean = false)

/** One utterance per start. Ordinary dictation never sends a prompt and never restarts itself. */
class SharedDictationOwner(
    private val factory: LocalRecognizerFactory,
    private val microphoneGranted: () -> Boolean,
    private val leases: RecognitionResourceLease = RecognitionResourceLease.process,
    private val main: Handler = Handler(Looper.getMainLooper()),
    private val timeoutMillis: Long = 30_000,
    private val configuration: (() -> SpeechAdmission)? = null,
    private val beginTiming: () -> AssistantTiming = { AssistantTiming.begin(null, Operation.RECOGNITION) },
) : AutoCloseable {
    constructor(context: Context, networkFallback: () -> Boolean = { true }) : this(AndroidLocalRecognizerFactory(context.applicationContext,
        fallbackPolicy = networkFallback), {
        context.checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED
    }, beginTiming = { AssistantTiming.begin(context.applicationContext, Operation.RECOGNITION) })
    private val epoch = AtomicLong()
    private val observers = CopyOnWriteArrayList<(DictationState) -> Unit>()
    @Volatile private var state = DictationState()
    private var foreground = false
    private var closed = false
    private var session: LocalRecognizerSession? = null
    private var retired: LocalRecognizerSession? = null
    private var lease: RecognitionResourceLease.Token? = null
    private var timeout: Runnable? = null
    private var prefix = ""
    private var emptyNoMatchIsIdle = false
    private var recognizedWords = false
    private var admittedConfiguration: SpeechAdmission? = null
    private var recognitionTiming: AssistantTiming? = null
    private var retirementOutcome: Stage? = null
    private var purpose = VoiceCapturePurpose.DRAFT
    private var admittedRecording = false
    fun snapshot(): DictationState = state
    fun serviceAvailable(): Boolean = configuration?.invoke()?.let {
        factory.availableForEntry(it.selection.language.ifEmpty { java.util.Locale.getDefault().toLanguageTag() }, it.networkFallback)
    } ?: factory.available()
    fun recordingEnabled() = factory.recordingEnabled()
    fun recordingAvailable(): Boolean = configuration?.invoke()?.let {
        factory.recordingAvailable(it.selection.language.ifEmpty { java.util.Locale.getDefault().toLanguageTag() }, it.networkFallback)
    } ?: false
    fun localOnlyServiceAvailable(): Boolean = factory.localOnlyAvailable()
    fun options() = SpeechOptions(recognitionInstalledLanguages = state.route?.installedLanguages,
        recognitionSupportedLanguages = state.route?.supportedLanguages,
        localOnlyRecognitionAvailable = runCatching { factory.localOnlyAvailable() }.getOrDefault(false)).bounded()
    fun subscribe(observer: (DictationState) -> Unit): AutoCloseable {
        observers += observer
        main.post { if (observer in observers) observer(state) }
        return AutoCloseable { observers -= observer }
    }
    fun setForeground(visible: Boolean) {
        if (!visible) epoch.incrementAndGet()
        main.post { foreground = visible; if (!visible) retireProjection() }
    }
    fun start(language: String, draft: String = state.draft, emptyNoMatchIsIdle: Boolean = false,
        purpose: VoiceCapturePurpose = VoiceCapturePurpose.DRAFT): Long {
        val ticket = epoch.incrementAndGet()
        val timing = beginTiming().also { it.mark(Stage.REQUESTED) }
        main.post {
            if (ticket != epoch.get() || closed) { timing.finish(Stage.CANCELLED); return@post }
            timing.mark(Stage.WORKER_STARTED)
            if (!dispose(Stage.REPLACED)) { timing.finish(Stage.RETIREMENT_FAILED); publish(DictationState(phase = DictationPhase.ERROR, draft = draft, error = "recognition_cleanup_failed", utterance = ticket)); return@post }
            recognitionTiming = timing
            prefix = draft.trim()
            this.emptyNoMatchIsIdle = emptyNoMatchIsIdle && prefix.isEmpty()
            recognizedWords = false
            this.purpose = purpose
            publish(DictationState(draft = draft, utterance = ticket))
            if (ticket != epoch.get() || closed) return@post
            admittedConfiguration = configuration?.invoke()
            admittedRecording = factory.recordingEnabled()
            val locale = SpeechLocales.parse(admittedConfiguration?.selection?.language?.ifEmpty { language } ?: language)
            when {
                !foreground -> fail(ticket, "visible_activity_required")
                !microphoneGranted() -> fail(ticket, "microphone_permission_required")
                locale == null -> fail(ticket, "local_language_unavailable")
                else -> admit(ticket, locale.toLanguageTag(), timing)
            }
        }
        return ticket
    }
    private fun admit(ticket: Long, language: String, timing: AssistantTiming) {
        try {
            val admitted = admittedConfiguration
            if (!(admitted?.let { factory.availableForEntry(language, it.networkFallback, admittedRecording) } ?: factory.available())) {
                fail(ticket, if (admittedRecording) "recorded_input_provider_unproven" else "on_device_recognition_unavailable"); return
            }
            leases.retryFailedRetirement()
            if (ticket != epoch.get() || closed) return
            lease = leases.tryAcquire(RecognitionResourceLease.Kind.CHAT)
            if (lease == null) { timing.mark(Stage.LEASE_DENIED); fail(ticket, "recognition_busy"); return }
            timing.mark(Stage.LEASE_ACQUIRED); timing.mark(Stage.ADMITTED)
            timing.mark(Stage.CREATE_REQUESTED)
            val sdk = if (admitted != null) factory.createForEntry(purpose, language, admitted.networkFallback, timing, admittedRecording)
                else factory.create(timing)
            session = sdk
            if (admittedRecording && !sdk.archiveExpected) { fail(ticket, "recorded_input_provider_unproven"); return }
            publish(state.copy(phase = DictationPhase.CHECKING, route = sdk.route, archiveExpected = sdk.archiveExpected))
            if (!current(ticket, sdk)) return
            val deadline = Runnable { if (ticket == epoch.get()) timing.mark(Stage.TIMED_OUT); fail(ticket, "recognition_timeout") }
            timeout = deadline; main.postDelayed(deadline, timeoutMillis)
            sdk.checkSelectedLanguage(language, admitted?.selection?.language?.isNotEmpty() == true) { supported -> main.post {
                if (!current(ticket, sdk) || state.phase != DictationPhase.CHECKING) return@post
                state = state.copy(route = sdk.route)
                if (!supported) { fail(ticket, "local_language_unavailable"); return@post }
                if (!foreground || !microphoneGranted()) { fail(ticket, "microphone_permission_required"); return@post }
                publish(state.copy(phase = DictationPhase.LISTENING))
                if (!current(ticket, sdk)) return@post
                try { timing.mark(Stage.LISTENING_REQUESTED); sdk.start(language, events(ticket, sdk, timing)) }
                catch (_: RuntimeException) { fail(ticket, "recognition_start_failed") }
            } }
            if (current(ticket, sdk) && state.phase == DictationPhase.CHECKING) publish(state.copy(route = sdk.route))
        } catch (_: RuntimeException) { fail(ticket, "recognition_unavailable") }
    }
    private fun current(ticket: Long, sdk: LocalRecognizerSession) = ticket == epoch.get() && session === sdk && !closed
    private fun events(ticket: Long, sdk: LocalRecognizerSession, timing: AssistantTiming) = object : LocalRecognizerEvents {
        override fun ready() { main.post {
            if (current(ticket, sdk) && foreground && state.phase == DictationPhase.LISTENING && !state.readyForSpeech) {
                timing.mark(Stage.READY_MAIN_DELIVERED)
                publish(state.copy(readyForSpeech = true))
            }
        } }
        override fun partial(text: String) { main.post {
            if (current(ticket, sdk)) {
                if (text.isNotBlank()) recognizedWords = true
                publish(state.copy(draft = if (text.isNotBlank()) joined(text) else state.draft, route = sdk.route))
            }
        } }
        override fun final(text: String) { main.post {
            if (!current(ticket, sdk)) return@post
            timing.mark(Stage.FINAL_MAIN_DELIVERED)
            state = state.copy(route = sdk.route)
            if (text.isBlank()) { fail(ticket, "no_speech"); return@post }
            val draft = joined(text)
            val recordedAudio = sdk.recordedAudio
            if (sdk.archiveExpected && recordedAudio == null) { fail(ticket, "recording_save_failed"); return@post }
            val cleaned = dispose(Stage.SETTLED)
            if (ticket != epoch.get() || closed) return@post
            if (!cleaned) publish(state.copy(phase = DictationPhase.ERROR, draft = draft, error = "recognition_cleanup_failed", readyForSpeech = false))
            else publish(DictationState(DictationPhase.FINAL, draft, utterance = ticket, route = state.route,
                recordedAudio = recordedAudio, archiveExpected = sdk.archiveExpected))
        } }
        override fun error(code: String) { main.post {
            if (!current(ticket, sdk)) return@post
            if (code == "recognition_error_7" && emptyNoMatchIsIdle && !recognizedWords) {
                val cleaned = dispose(Stage.SETTLED)
                if (ticket != epoch.get() || closed) return@post
                publish(state.copy(phase = if (cleaned) DictationPhase.IDLE else DictationPhase.ERROR,
                    error = if (cleaned) null else "recognition_cleanup_failed", diagnosticError = code, endedWithoutSpeech = cleaned, readyForSpeech = false))
            } else fail(ticket, code)
        } }
    }
    private fun joined(text: String) = listOf(prefix, text.trim()).filter { it.isNotEmpty() }.joinToString(" ")
    /** Request a final result, unlike Stop which cancels every pending admission/callback. */
    fun endUtterance() { val ticket = epoch.get(); main.post {
        if (ticket == epoch.get() && session != null && state.phase == DictationPhase.LISTENING) {
            publish(state.copy(phase = DictationPhase.FINISHING, readyForSpeech = false))
            if (ticket != epoch.get()) return@post
            recognitionTiming?.mark(Stage.FINISH_REQUESTED)
            try { session?.stop() } catch (_: RuntimeException) { fail(ticket, "recognition_stop_failed") }
        }
    } }
    fun stop() {
        epoch.incrementAndGet()
        main.post { retireProjection() }
    }
    private fun fail(ticket: Long, code: String) {
        if (ticket != epoch.get()) return
        val cleaned = dispose(Stage.FAILED)
        if (ticket != epoch.get() || closed) return
        publish(state.copy(phase = DictationPhase.ERROR, error = if (cleaned) code else "recognition_cleanup_failed", diagnosticError = code, readyForSpeech = false))
    }
    private fun retireProjection() {
        val cleaned = dispose()
        publish(state.copy(phase = if (cleaned) DictationPhase.IDLE else DictationPhase.ERROR,
            error = if (cleaned) null else "recognition_cleanup_failed", endedWithoutSpeech = false, readyForSpeech = false))
    }
    private fun dispose(outcome: Stage = Stage.CANCELLED): Boolean {
        val terminalOutcome = retirementOutcome ?: outcome
        val timing = recognitionTiming; recognitionTiming = null
        timeout?.let(main::removeCallbacks); timeout = null
        session?.let { state = state.copy(route = it.route, recordedAudio = it.recordedAudio ?: state.recordedAudio) }
        val sdk = session ?: retired; session = null; retired = null
        if (sdk != null) {
            runCatching { sdk.cancel() }
            if (runCatching { sdk.destroy() }.isFailure) {
                timing?.mark(Stage.RETIREMENT_FAILED)
                // The existing retained SDK/lease owns this span until retirement really succeeds.
                recognitionTiming = timing
                retirementOutcome = if (terminalOutcome == Stage.SETTLED) Stage.FAILED else terminalOutcome
                retired = sdk
                val owned = lease
                owned?.onRetirementRetry {
                    retryRetirementOnMain(main) {
                        if (retired !== sdk || lease !== owned || session != null) false else dispose()
                    }
                }
                return false
            }
        }
        lease?.close(); lease = null
        retirementOutcome = null
        if (sdk != null) timing?.mark(Stage.SESSION_RETIRED)
        timing?.finish(terminalOutcome)
        return true
    }
    private fun publish(value: DictationState) { state = value; observers.forEach { it(value) } }
    override fun close() {
        epoch.incrementAndGet()
        main.post { closed = true; retireProjection(); observers.clear() }
    }
    companion object {
        fun configured(context: Context, admission: () -> SpeechAdmission) = configuredWithAudio(context, admission)
        fun configuredWithAudio(context: Context, admission: () -> SpeechAdmission,
            audio: RecordedAudioFactory? = null,
            providerGate: dev.gitblame.voxgolem.assistant.inputtransport.SuppliedAudioProviderGate =
                dev.gitblame.voxgolem.assistant.inputtransport.SuppliedAudioProviderGate.UNPROVEN,
            recordingEnabled: () -> Boolean = { false }) = SharedDictationOwner(
            AndroidLocalRecognizerFactory(context.applicationContext, recordedAudio = audio,
                suppliedAudioGate = providerGate, enableRecording = recordingEnabled), {
                context.checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED
            }, configuration = admission,
            beginTiming = { AssistantTiming.begin(context.applicationContext, Operation.RECOGNITION) })
    }
}
