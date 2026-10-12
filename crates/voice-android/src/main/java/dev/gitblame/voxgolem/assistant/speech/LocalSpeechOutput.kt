package dev.gitblame.voxgolem.assistant.speech

import android.content.Context
import android.os.Handler
import android.os.Looper
import dev.gitblame.voxgolem.assistant.AssistantTiming
import dev.gitblame.voxgolem.assistant.AssistantTiming.Operation
import dev.gitblame.voxgolem.assistant.AssistantTiming.Stage
import dev.gitblame.voxgolem.assistant.recognition.RecognitionResourceLease
import dev.gitblame.voxgolem.assistant.recognition.SpeechLocales
import dev.gitblame.voxgolem.assistant.recognition.retryRetirementOnMain
import java.util.Locale
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.atomic.AtomicLong

data class LocalVoice(val name: String, val locale: Locale, val networkRequired: Boolean, val installed: Boolean)
interface LocalTtsSession {
    val provider: String? get() = null
    val defaultVoiceName: String? get() = null
    val providerIsPreference: Boolean get() = false
    fun voices(): List<LocalVoice>
    fun speak(text: String, voice: LocalVoice, onTerminal: (String?) -> Unit): Boolean
    fun speak(text: String, voice: LocalVoice, timing: AssistantTiming?, onTerminal: (String?) -> Unit): Boolean =
        speak(text, voice, onTerminal)
    fun stop()
    fun shutdown()
}
interface LocalTtsFactory {
    fun create(onReady: (Boolean) -> Unit): LocalTtsSession
    fun providers(): List<String?> = listOf(null)
    fun create(provider: String?, onReady: (Boolean) -> Unit): LocalTtsSession = create(onReady)
    fun create(provider: String?, timing: AssistantTiming?, onReady: (Boolean) -> Unit): LocalTtsSession = create(provider, onReady)
}
interface SpeechAudioFocus {
    fun acquire(onLoss: () -> Unit): Boolean
    fun release()
}
enum class SpeechPhase { IDLE, PREPARING, CHECKING, SPEAKING, ERROR }
data class TtsRoute(val provider: String?, val requestedLanguage: String, val selectedLanguage: String,
    val networkRequired: Boolean, val fallbackReason: String? = null, val providerIsPreference: Boolean = false,
    val selectedVoice: String? = null, val installedLanguages: List<String>? = null)
data class SpeechState(val phase: SpeechPhase = SpeechPhase.IDLE, val enabled: Boolean = false,
    val localVoiceAvailable: Boolean = false, val error: String? = null, val route: TtsRoute? = null,
    val voiceAvailable: Boolean = localVoiceAvailable, val options: SpeechOptions = SpeechOptions())

/** Final-text playback with a foreground ready-session cache and explicit network-voice policy. */
class LocalSpeechOutput(
    private val factory: LocalTtsFactory,
    private val focus: SpeechAudioFocus,
    private val main: Handler = Handler(Looper.getMainLooper()),
    private val timeoutMillis: Long = 120_000,
    private val leases: RecognitionResourceLease = RecognitionResourceLease.process,
    private val allowNetworkFallback: Boolean = false,
    private val networkFallbackPolicy: () -> Boolean = { allowNetworkFallback },
    private val configuration: (() -> SpeechAdmission)? = null,
    private val beginTiming: (Operation) -> AssistantTiming = { AssistantTiming.begin(null, it) },
) : AutoCloseable {
    constructor(context: Context, networkFallback: () -> Boolean = { true }) : this(AndroidLocalTtsFactory(context.applicationContext), AndroidSpeechAudioFocus(context.applicationContext),
        allowNetworkFallback = true, networkFallbackPolicy = networkFallback,
        beginTiming = { AssistantTiming.begin(context.applicationContext, it) })
    private data class Request(val ticket: Long, val language: Locale, val text: String?, val selection: SpeechSelection)
    private data class ProviderAttempt(val provider: String?, val allowNetwork: Boolean)
    private val epoch = AtomicLong()
    private val observers = CopyOnWriteArrayList<(SpeechState) -> Unit>()
    @Volatile private var state = SpeechState()
    private var foreground = false
    private var closed = false
    private var sdk: LocalTtsSession? = null
    private var sessionGeneration = 0L
    private var sessionReady = false
    private var sessionVoices: List<LocalVoice> = emptyList()
    private var sessionProvider: String? = null
    private var sessionDefaultVoice: String? = null
    private var sessionProviderPreference = false
    private val knownVoices = LinkedHashMap<String, List<SpeechVoiceOption>>(8, 0.75f, true)
    private var knownProviders: List<String> = emptyList()
    private var preparedSelection = SpeechSelection()
    private var pendingRequest: Request? = null
    private var providerAttempts: List<ProviderAttempt> = emptyList()
    private var providerAttempt = 0
    private var activeProvider: String? = null
    private var providerTimeout: Runnable? = null
    private var retired: LocalTtsSession? = null
    private var focused = false
    private var lease: RecognitionResourceLease.Token? = null
    private var terminal: ((String?) -> Unit)? = null
    private var timeout: Runnable? = null
    private var requestTiming: AssistantTiming? = null
    fun snapshot(): SpeechState = state
    fun options(): SpeechOptions = state.options
    private fun collectedOptions(): SpeechOptions = SpeechOptions(knownProviders, knownVoices.values.flatten()).bounded()
    fun refreshProviders() { main.post {
        if (closed) return@post
        knownProviders = runCatching { factory.providers().filterNotNull().filter(::selectableSpeechId).distinct().take(8) }.getOrDefault(emptyList())
        publish(state)
    } }
    fun subscribe(observer: (SpeechState) -> Unit): AutoCloseable {
        observers += observer; main.post { if (observer in observers) observer(state) }
        return AutoCloseable { observers -= observer }
    }
    fun setEnabled(enabled: Boolean) {
        if (!enabled) epoch.incrementAndGet()
        main.post {
            val cancelled = if (!enabled) terminal else null
            val cleaned = enabled || dispose()
            if (!enabled) endTiming(if (cleaned) Stage.CANCELLED else Stage.RETIREMENT_FAILED)
            publish(state.copy(enabled = enabled, phase = if (!cleaned) SpeechPhase.ERROR else if (enabled) state.phase else SpeechPhase.IDLE,
                error = if (cleaned) null else "speech_cleanup_failed"))
            cancelled?.invoke(if (cleaned) "speech_disabled" else "speech_cleanup_failed")
        }
    }
    fun setForeground(visible: Boolean) {
        if (!visible) epoch.incrementAndGet()
        main.post { foreground = visible; if (!visible) retireProjection() }
    }
    /** Queries installed local voices while idle, without preempting queued or active playback. */
    fun prepare(language: String) {
        val observed = epoch.get()
        val timing = beginTiming(Operation.PREPARATION).also { it.mark(Stage.REQUESTED) }
        main.post {
            if (closed || retired != null || state.phase in setOf(SpeechPhase.SPEAKING, SpeechPhase.CHECKING) ||
                (state.phase == SpeechPhase.PREPARING && preparedSelection == (configuration?.invoke()?.selection ?: SpeechSelection())) ||
                (sdk != null && preparedSelection == (configuration?.invoke()?.selection ?: SpeechSelection())) ||
                !epoch.compareAndSet(observed, observed + 1)) {
                timing.finish(Stage.CANCELLED); return@post
            }
            initialize(language, null, null, observed + 1, timing)
        }
    }
    fun speak(finalText: String, language: String, onTerminal: (String?) -> Unit) =
        initialize(language, finalText, onTerminal, epoch.incrementAndGet(),
            beginTiming(Operation.SPEECH).also { it.mark(Stage.REQUESTED) })
    private fun initialize(language: String, text: String?, callback: ((String?) -> Unit)?, ticket: Long, timing: AssistantTiming) {
        main.post {
            if (ticket != epoch.get() || closed) { timing.finish(Stage.CANCELLED); return@post }
            val replaced = terminal
            if ((state.phase == SpeechPhase.SPEAKING || retired != null) && !dispose()) {
                endTiming(Stage.RETIREMENT_FAILED); timing.finish(Stage.RETIREMENT_FAILED)
                publish(state.copy(phase = SpeechPhase.ERROR, error = "speech_cleanup_failed"))
                replaced?.invoke("speech_cleanup_failed")
                callback?.invoke("speech_cleanup_failed"); return@post
            }
            timeout?.let(main::removeCallbacks); timeout = null
            terminal = null
            endTiming(Stage.REPLACED)
            replaced?.invoke("speech_replaced")
            if (ticket != epoch.get() || closed) { timing.finish(Stage.CANCELLED); return@post }
            requestTiming = timing
            terminal = callback
            if (!foreground) { finish(ticket, "visible_activity_required"); return@post }
            if (text != null && !state.enabled) { finish(ticket, "speech_disabled"); return@post }
            val admission = configuration?.invoke() ?: SpeechAdmission(networkFallback = networkFallbackPolicy())
            val selection = admission.selection
            try { selection.validateShape() } catch (_: IllegalArgumentException) { finish(ticket, "invalid_speech_selection"); return@post }
            val requested = SpeechLocales.parse(selection.language.ifEmpty { language })
            if (requested == null || text?.isBlank() == true) { finish(ticket, "speech_text_or_language_missing"); return@post }
            knownProviders = factory.providers().filterNotNull().filter(::selectableSpeechId).distinct().take(8)
            if (selection.ttsProvider.isNotEmpty() && selection.ttsProvider !in knownProviders) { finish(ticket, "unsupported_tts_provider"); return@post }
            if (sdk != null && (selection != preparedSelection)) {
                if (!retireSession()) { finish(ticket, "speech_cleanup_failed"); return@post }
            }
            preparedSelection = selection
            timing.mark(Stage.ADMITTED)
            pendingRequest = Request(ticket, requested, text, selection)
            val candidates = if (selection.ttsProvider.isNotEmpty() || selection.ttsVoice.isNotEmpty()) listOf(selection.ttsProvider.ifEmpty { null })
                else (listOf(activeProvider) + factory.providers()).filter { it == null || selectableSpeechId(it) }.distinct().take(8)
            val allowNetwork = admission.networkFallback
            val reuseNetwork = allowNetwork && sessionReady && state.route?.networkRequired == true &&
                state.route?.requestedLanguage == requested.toLanguageTag()
            providerAttempts = candidates.map { ProviderAttempt(it, reuseNetwork) } +
                if (allowNetwork && !reuseNetwork) candidates.map { ProviderAttempt(it, true) } else emptyList()
            providerAttempt = 0
            publish(state.copy(phase = if (text == null) SpeechPhase.PREPARING else SpeechPhase.CHECKING,
                localVoiceAvailable = false, voiceAvailable = false, error = null))
            if (ticket != epoch.get() || closed) return@post
            val deadline = Runnable { finish(ticket, "speech_timeout") }; timeout = deadline
            main.postDelayed(deadline, timeoutMillis)
            if (sessionReady) { timing.mark(Stage.READY_REUSED); speakPrepared(); return@post }
            if (sdk != null) { timing.mark(Stage.PENDING_REUSED); return@post }
            openProvider()
        }
    }
    private fun openProvider() {
        val request = pendingRequest ?: return
        val attempt = providerAttempts.getOrNull(providerAttempt) ?: return
        activeProvider = attempt.provider
        val generation = ++sessionGeneration
        val deadline = Runnable {
            if (generation == sessionGeneration && !sessionReady && pendingRequest != null)
                nextProvider("speech_initialization_failed", providerFailed = true)
        }
        providerTimeout = deadline; main.postDelayed(deadline, 10_000)
        try {
            sdk = factory.create(attempt.provider, requestTiming) { ready -> main.post ready@{
                if (generation != sessionGeneration || sdk == null || closed || sessionReady) return@ready
                if (pendingRequest == null) return@ready
                providerTimeout?.let(main::removeCallbacks); providerTimeout = null
                if (!ready) { nextProvider("speech_initialization_failed", providerFailed = true); return@ready }
                try {
                    sessionProvider = sdk!!.provider
                    if (activeProvider != null && sessionProvider != null && activeProvider != sessionProvider) {
                        finish(request.ticket, "tts_provider_mismatch"); return@ready
                    }
                    sessionVoices = sdk!!.voices()
                    val key = activeProvider ?: ""
                    knownVoices[key] = sessionVoices.filter { it.installed && selectableSpeechId(it.name) }.sortedBy { it.name }.take(128)
                        .map { SpeechVoiceOption(key, it.name, it.locale.toLanguageTag(), it.networkRequired) }
                    while (knownVoices.size > 8) knownVoices.remove(knownVoices.keys.first())
                    sessionDefaultVoice = sdk!!.defaultVoiceName
                    sessionProviderPreference = sdk!!.providerIsPreference
                    sessionReady = true
                    requestTiming?.mark(Stage.VOICES_READY)
                    if (activeProvider == null && sessionProvider != null) {
                        providerAttempts = providerAttempts.filterIndexed { index, candidate ->
                            index <= providerAttempt || candidate.provider != sessionProvider
                        }
                    }
                    speakPrepared()
                } catch (_: RuntimeException) { finish(pendingRequest?.ticket ?: request.ticket, "speech_failed") }
            } }
        } catch (_: RuntimeException) { nextProvider("speech_initialization_failed", providerFailed = true) }
    }
    private fun nextProvider(error: String, providerFailed: Boolean = false) {
        val request = pendingRequest ?: return
        var next = providerAttempt + 1
        if (providerFailed) {
            while (next < providerAttempts.size && providerAttempts[next].provider == activeProvider) next++
            providerAttempts = providerAttempts.filterIndexed { index, candidate ->
                index < next || candidate.provider != activeProvider
            }
        }
        val attempt = providerAttempts.getOrNull(next)
        if (attempt == null) { finish(request.ticket, error); return }
        providerAttempt = next
        if (sessionReady && attempt.provider == activeProvider) { speakPrepared(); return }
        if (!retireSession()) { finish(request.ticket, "speech_cleanup_failed"); return }
        openProvider()
    }
    private fun speakPrepared() {
        val request = pendingRequest ?: return
        val ticket = request.ticket
        if (ticket != epoch.get() || closed || !sessionReady) return
        val session = sdk ?: return
        try {
            val requested = request.language
            val networkAllowed = providerAttempts.getOrNull(providerAttempt)?.allowNetwork == true
            val voice = sessionVoices.filter { it.installed && (!it.networkRequired || networkAllowed) &&
                (request.selection.ttsVoice.isEmpty() || it.name == request.selection.ttsVoice) &&
                it.locale.language == requested.language &&
                (requested.script.isEmpty() || it.locale.script.isEmpty() || it.locale.script == requested.script) }
                .sortedWith(compareBy<LocalVoice> { it.networkRequired }
                    .thenBy { it.locale.country != requested.country }
                    .thenBy { it.name != sessionDefaultVoice }.thenBy { it.name }).firstOrNull()
            if (voice == null) { nextProvider(if (request.selection.ttsVoice.isNotEmpty()) "unsupported_tts_voice" else "local_voice_unavailable"); return }
            pendingRequest = null
            val fallback = when {
                voice.networkRequired && request.selection.ttsVoice.isEmpty() -> "no_installed_local_voice"
                activeProvider != null && request.selection.ttsProvider.isEmpty() -> "native_engine_fallback"
                voice.locale.country != requested.country -> "configured_variant_unavailable"
                else -> null
            }
            val selectedLanguage = Locale.Builder().setLanguage(voice.locale.language).setRegion(voice.locale.country)
                .setScript(voice.locale.script).build().toLanguageTag()
            requestTiming?.mark(if (voice.networkRequired) Stage.NETWORK_ROUTE_SELECTED else Stage.LOCAL_ROUTE_SELECTED)
            publish(state.copy(localVoiceAvailable = !voice.networkRequired, voiceAvailable = true,
                route = TtsRoute(sessionProvider, requested.toLanguageTag(), selectedLanguage, voice.networkRequired, fallback,
                     providerIsPreference = sessionProviderPreference, selectedVoice = voice.name.takeIf(::selectableSpeechId),
                     installedLanguages = sessionVoices.filter { it.installed }.map { it.locale.toLanguageTag() }.filter { it.length <= 64 }.distinct().take(128))))
            if (ticket != epoch.get() || closed) return
            val text = request.text
            if (text == null) { finish(ticket, null); return }
            if (!foreground || !state.enabled) { finish(ticket, "speech_cancelled"); return }
            leases.retryFailedRetirement()
            if (ticket != epoch.get() || closed) return
            lease = leases.tryAcquire(RecognitionResourceLease.Kind.CHAT)
            requestTiming?.mark(if (lease == null) Stage.LEASE_DENIED else Stage.LEASE_ACQUIRED)
            if (lease == null) { finish(ticket, "recognition_busy"); return }
            focused = focus.acquire { main.post { finish(ticket, "audio_focus_lost") } }
            requestTiming?.mark(if (focused) Stage.FOCUS_ACQUIRED else Stage.FOCUS_DENIED)
            if (!focused) { finish(ticket, "audio_focus_denied"); return }
            publish(state.copy(phase = SpeechPhase.SPEAKING))
            if (ticket != epoch.get() || closed) return
            if (!session.speak(text, voice, requestTiming) { error -> main.post { finish(ticket, error) } }) finish(ticket, "speech_failed")
        } catch (_: RuntimeException) { finish(ticket, "speech_failed") }
    }
    fun stop() {
        epoch.incrementAndGet()
        main.post { retireProjection(keepWarm = foreground && state.enabled && sessionReady && state.phase == SpeechPhase.IDLE) }
    }
    private fun finish(ticket: Long, error: String?) {
        if (ticket != epoch.get()) return
        // Terminal callbacks (including repeated SDK onDone/onStop) can settle only this epoch once.
        val settlement = epoch.incrementAndGet()
        val callback = terminal; terminal = null
        val keepWarm = error == null && foreground && state.enabled && sessionReady
        val result = if (keepWarm) {
            pendingRequest = null
            timeout?.let(main::removeCallbacks); timeout = null
            releasePlayback()
            null
        } else if (dispose()) error else "speech_cleanup_failed"
        endTiming(if (result == null) Stage.SETTLED else Stage.FAILED)
        publish(state.copy(phase = if (result == null) SpeechPhase.IDLE else SpeechPhase.ERROR, error = result))
        if (epoch.get() == settlement && !closed) callback?.invoke(result)
    }
    private fun retireProjection(keepWarm: Boolean = false) {
        val cleaned = keepWarm || dispose()
        endTiming(if (cleaned) Stage.CANCELLED else Stage.RETIREMENT_FAILED)
        publish(state.copy(phase = if (cleaned) SpeechPhase.IDLE else SpeechPhase.ERROR,
            error = if (cleaned) null else "speech_cleanup_failed"))
    }
    private fun dispose(): Boolean {
        timeout?.let(main::removeCallbacks); timeout = null; terminal = null
        pendingRequest = null; providerAttempts = emptyList(); providerAttempt = 0
        if (!retireSession()) return false
        activeProvider = null
        releasePlayback()
        return true
    }
    private fun retireSession(): Boolean {
        sessionGeneration++
        providerTimeout?.let(main::removeCallbacks); providerTimeout = null
        sessionReady = false; sessionVoices = emptyList(); sessionProvider = null; sessionDefaultVoice = null; sessionProviderPreference = false
        val session = sdk ?: retired; sdk = null; retired = null
        if (session != null) {
            runCatching { session.stop() }
            if (runCatching { session.shutdown() }.isFailure) {
                retired = session
                requestTiming?.mark(Stage.RETIREMENT_FAILED)
                val owned = lease
                owned?.onRetirementRetry {
                    retryRetirementOnMain(main) {
                        if (retired !== session || lease !== owned || sdk != null) false else dispose()
                    }
                }
                return false
            }
            requestTiming?.mark(Stage.SESSION_RETIRED)
        }
        return true
    }
    private fun releasePlayback() {
        if (focused) { focused = false; runCatching { focus.release() } }
        lease?.close(); lease = null
    }
    private fun endTiming(stage: Stage) { requestTiming?.finish(stage); requestTiming = null }
    private fun publish(value: SpeechState) { state = value.copy(options = collectedOptions()); observers.forEach { it(state) } }
    override fun close() {
        epoch.incrementAndGet()
        main.post { closed = true; retireProjection(); observers.clear() }
    }
    companion object {
        fun configured(context: Context, admission: () -> SpeechAdmission) = LocalSpeechOutput(
            AndroidLocalTtsFactory(context.applicationContext), AndroidSpeechAudioFocus(context.applicationContext), configuration = admission,
            beginTiming = { AssistantTiming.begin(context.applicationContext, it) })
    }
}
