package dev.gitblame.voxgolem.assistant.inputtransport

import android.content.Context
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import dev.gitblame.voxgolem.assistant.recognition.RecognitionResourceLease
import java.util.concurrent.CompletableFuture

enum class AssistantInputRole { PHONE, WATCH }
enum class AssistantInputRoute { PHONE_SERVER, NATIVE_ON_DEVICE, WATCH_PHONE, TYPING }
enum class AssistantInputUnavailable {
    CLOSED, WINDOW_NOT_FOREGROUND, INVALID_LANGUAGE, NATIVE_CHECK_UNAVAILABLE,
    PEER_UNAVAILABLE, PROVIDER_UNAVAILABLE, RESOURCE_BUSY, SESSION_BUSY, TRANSPORT_UNAVAILABLE, PHONE_SERVER_ONLY,
}
data class AssistantInputAvailability(val route: AssistantInputRoute, val unavailable: AssistantInputUnavailable? = null)
data class AssistantInputStart(val route: AssistantInputRoute, val session: String? = null,
    val unavailable: AssistantInputUnavailable? = null)

data class NativeInputProviderStamp(val component: String, val packageVersion: Long, val osBuild: String)

/** Native-only physical calibration evidence; never construct from a WebView/transfer JSON flag. */
class NativeSuppliedAudioCalibration(
    private val language: String,
    private val calibratedProvider: NativeInputProviderStamp,
    private val currentProvider: () -> NativeInputProviderStamp?,
) : SuppliedAudioDeviceGate {
    init {
        AssistantInputWire.language(language)
        require(calibratedProvider.component.isNotBlank() && calibratedProvider.component.length <= 512)
        require(calibratedProvider.packageVersion >= 0 && calibratedProvider.osBuild.isNotBlank())
    }
    override fun permits(language: String) = language == this.language && calibratedProvider.osBuild == Build.FINGERPRINT &&
        runCatching { currentProvider() == calibratedProvider }.getOrDefault(false)
}

internal data class InputRuntimeHandlers(
    val control: (String, InputFrame) -> Unit,
    val authorizeAudio: (String, String) -> Boolean,
    val audio: (String, String, ByteArray) -> Boolean,
    val sourceEnded: (String, String) -> Unit,
    val failure: (String, String) -> Unit,
)
internal class InputRuntimeTransport(val audio: AssistantAudioTransport, private val dispose: () -> Unit) : AutoCloseable {
    override fun close() = dispose()
}

/** Native-window lifecycle owner. No store, completion engine, credentials transfer or service. */
class AssistantInputRuntime internal constructor(
    private val role: AssistantInputRole,
    private val window: CallerForegroundSignal,
    private val peers: AssistantInputPeers,
    private val transportFactory: (CallerForegroundSignal, SourcePeerBinding, InputRuntimeHandlers) -> InputRuntimeTransport,
    private val supplied: SuppliedInputFactory?,
    private val microphone: WatchMicrophoneFactory?,
    private val nativeAvailable: (String) -> Boolean,
    private val lease: RecognitionResourceLease,
    private val onDraft: (String) -> Unit,
    private val onOutcome: (InputOutcome) -> Unit,
    private val now: () -> Long,
) : AutoCloseable {
    private class Scope {
        var transport: InputRuntimeTransport? = null
        var phone: PhoneInputController? = null
        var watch: WatchInputController? = null
        var phoneSession: String? = null
        var watchSession: String? = null
        var watchLanguage: String? = null
        var failedPeer: AssistantInputPeerRoute? = null
        var remoteUnavailable: AssistantInputUnavailable? = null
    }
    private val main = Handler(Looper.getMainLooper())
    private var activeWindow = false
    private var closed = false
    private var current: Scope? = null
    private var ticker: Runnable? = null
    private val foreground = CallerForegroundSignal { activeWindow && !closed && window.isForeground() }
    init { requireInputMain() }

    fun onForeground(active: Boolean) {
        requireInputMain()
        if (closed) return
        if (!active || !window.isForeground()) { leaveWindow(); return }
        if (current != null) return
        activeWindow = true
        peers.onForeground(true)
        val scope = Scope(); current = scope
        try {
            val callbacks = InputRuntimeHandlers(
                { node, frame -> if (eligible(scope)) control(scope, node, frame) },
                { node, session -> eligible(scope) && scope.phone?.acceptsAudio(node, session) == true },
                { node, session, pcm -> eligible(scope) && scope.phone?.audio(node, session, pcm) == true },
                { node, session -> if (eligible(scope)) scope.phone?.sourceEnded(node, session) },
                { node, session -> if (owns(scope)) {
                    scope.phone?.transportFailure(node, session); scope.watch?.transportFailure(node, session)
                } },
            )
            scope.transport = transportFactory(foreground, peers, callbacks)
            if (role == AssistantInputRole.PHONE) {
                scope.phone = PhoneInputController(requireNotNull(supplied), foreground, peers, lease,
                    { node, frame ->
                        scope.transport?.audio?.control(node, frame)
                        if (owns(scope) && frame.kind == InputKind.PARTIAL) runCatching { onDraft(frame.text) }
                    }, now, { outcome ->
                        if (owns(scope)) {
                            if (scope.phoneSession == outcome.session) scope.phoneSession = null
                            runCatching { onOutcome(outcome) }
                        }
                    })
            } else {
                scope.watch = WatchInputController(requireNotNull(scope.transport).audio, requireNotNull(microphone),
                    foreground, peers, nativeAvailable, lease,
                    { text -> if (owns(scope)) runCatching { onDraft(text) } }, now,
                    { outcome -> if (owns(scope)) {
                        if (scope.watchSession == outcome.session) scope.watchSession = null
                        if (outcome.error != null) {
                            scope.failedPeer = peers.selected()
                            scope.remoteUnavailable = when (outcome.error) {
                                "supplied_audio_unavailable" -> AssistantInputUnavailable.PROVIDER_UNAVAILABLE
                                "input_busy" -> AssistantInputUnavailable.RESOURCE_BUSY
                                else -> AssistantInputUnavailable.TRANSPORT_UNAVAILABLE
                            }
                        }
                        runCatching { onOutcome(outcome) }
                    } })
            }
            startTicker(scope)
        } catch (_: Exception) { leaveWindow() }
    }

    fun availability(language: String): AssistantInputAvailability {
        requireInputMain()
        if (closed) return unavailable(AssistantInputUnavailable.CLOSED)
        if (runCatching { AssistantInputWire.language(language) }.isFailure) return unavailable(AssistantInputUnavailable.INVALID_LANGUAGE)
        if (!foreground.isForeground()) return unavailable(AssistantInputUnavailable.WINDOW_NOT_FOREGROUND)
        if (role == AssistantInputRole.WATCH) {
            val native = runCatching { nativeAvailable(language) }
            if (native.isFailure) return unavailable(AssistantInputUnavailable.NATIVE_CHECK_UNAVAILABLE)
            if (native.getOrDefault(false)) return AssistantInputAvailability(AssistantInputRoute.NATIVE_ON_DEVICE)
        }
        if (current == null) return unavailable(AssistantInputUnavailable.TRANSPORT_UNAVAILABLE)
        val selected = peers.selected() ?: return unavailable(AssistantInputUnavailable.PEER_UNAVAILABLE)
        val scope = current
        if (role == AssistantInputRole.WATCH && scope?.watchLanguage == language && scope.failedPeer == selected)
            scope.remoteUnavailable?.let { return unavailable(it) }
        if (role == AssistantInputRole.PHONE && !runCatching { supplied?.available(language) == true }.getOrDefault(false))
            return unavailable(AssistantInputUnavailable.PROVIDER_UNAVAILABLE)
        if (current?.watchSession != null || current?.phoneSession != null) return unavailable(AssistantInputUnavailable.SESSION_BUSY)
        if (lease.current() != null) return unavailable(AssistantInputUnavailable.RESOURCE_BUSY)
        return AssistantInputAvailability(if (role == AssistantInputRole.PHONE) AssistantInputRoute.PHONE_SERVER else AssistantInputRoute.WATCH_PHONE)
    }

    /** Chooses a route; NATIVE_ON_DEVICE means caller uses its existing exact-language native owner. */
    fun start(language: String): AssistantInputStart {
        requireInputMain()
        if (role == AssistantInputRole.PHONE) return AssistantInputStart(AssistantInputRoute.TYPING, unavailable = AssistantInputUnavailable.PHONE_SERVER_ONLY)
        val available = availability(language)
        if (available.route != AssistantInputRoute.WATCH_PHONE) return AssistantInputStart(available.route, unavailable = available.unavailable)
        val scope = current ?: return AssistantInputStart(AssistantInputRoute.TYPING, unavailable = AssistantInputUnavailable.TRANSPORT_UNAVAILABLE)
        val route = peers.selected() ?: return AssistantInputStart(AssistantInputRoute.TYPING, unavailable = AssistantInputUnavailable.PEER_UNAVAILABLE)
        scope.watchLanguage = language
        val session = scope.watch?.start(route.nodeId, route.localInstallation, route.peerInstallation, language)
            ?: return AssistantInputStart(AssistantInputRoute.TYPING, unavailable = AssistantInputUnavailable.TRANSPORT_UNAVAILABLE)
        scope.watchSession = session
        return AssistantInputStart(AssistantInputRoute.WATCH_PHONE, session)
    }

    fun stop(session: String): CompletableFuture<InputOutcome> {
        requireInputMain()
        return current?.watch?.stop(session) ?: CompletableFuture.completedFuture(
            InputOutcome(session, "", false, if (role == AssistantInputRole.PHONE) "phone_server_only" else "session_unavailable"))
    }
    fun interrupt() {
        requireInputMain(); current?.let { it.phone?.interrupt(); it.watch?.interrupt() }
    }

    private fun control(scope: Scope, node: String, frame: InputFrame) {
        if (runCatching { AssistantInputWire.validate(frame) }.isFailure || !peers.accepts(node, frame.source, frame.target)) return
        if (role == AssistantInputRole.WATCH) { scope.watch?.receive(node, frame); return }
        if (frame.kind == InputKind.START && frame.sequence != 0L) return
        val phone = scope.phone ?: return
        if (frame.kind != InputKind.START) { phone.receive(node, frame); return }
        if (scope.phoneSession == frame.session) return
        val busy = scope.phoneSession != null
        if (!busy) scope.phoneSession = frame.session
        if (!busy && phone.receive(node, frame)) return
        if (scope.phoneSession == frame.session) scope.phoneSession = null
        val error = if (!runCatching { supplied?.available(frame.language) == true }.getOrDefault(false))
            "supplied_audio_unavailable" else "input_busy"
        runCatching { scope.transport?.audio?.control(node, frame.copy(kind = InputKind.ERROR, source = frame.target,
            target = frame.source, sequence = 1, error = error)) }
        runCatching { onOutcome(InputOutcome(frame.session, "", false, error)) }
    }
    private fun startTicker(scope: Scope) {
        val next = object : Runnable {
            override fun run() {
                if (!owns(scope)) return
                if (!foreground.isForeground()) { leaveWindow(); return }
                peers.refresh(); scope.phone?.tick(); scope.watch?.tick()
                if (owns(scope)) main.postDelayed(this, TICK_MILLIS)
            }
        }
        ticker = next; main.postDelayed(next, TICK_MILLIS)
    }
    private fun leaveWindow() {
        activeWindow = false
        ticker?.let { main.removeCallbacks(it) }; ticker = null
        val scope = current
        try { scope?.phone?.interrupt(); scope?.watch?.interrupt() }
        finally {
            current = null
            runCatching { scope?.transport?.close() }
            peers.onForeground(false)
        }
    }
    private fun owns(scope: Scope) = !closed && current === scope
    private fun eligible(scope: Scope) = owns(scope) && foreground.isForeground()
    private fun unavailable(reason: AssistantInputUnavailable) = AssistantInputAvailability(AssistantInputRoute.TYPING, reason)
    override fun close() {
        requireInputMain(); if (closed) return
        try { leaveWindow() } finally { closed = true; peers.close() }
    }

    companion object {
        private const val TICK_MILLIS = 250L
        fun phone(context: Context, foreground: CallerForegroundSignal,
            peers: AssistantInputPeers = ApplicationAssistantInputPeers(context),
            calibration: NativeSuppliedAudioCalibration? = null,
            onDraft: (String) -> Unit = {}, onOutcome: (InputOutcome) -> Unit = {},
            lease: RecognitionResourceLease = RecognitionResourceLease.process,
        ): AssistantInputRuntime = AssistantInputRuntime(AssistantInputRole.PHONE, foreground, peers, transport(context),
            AndroidSuppliedAudioFactory(context, calibration ?: SuppliedAudioDeviceGate { false }), null, { false },
            lease, onDraft, onOutcome, SystemClock::elapsedRealtime)

        fun watch(context: Context, foreground: CallerForegroundSignal, nativeAvailable: (String) -> Boolean,
            peers: AssistantInputPeers = ApplicationAssistantInputPeers(context),
            onDraft: (String) -> Unit = {}, onOutcome: (InputOutcome) -> Unit = {},
            lease: RecognitionResourceLease = RecognitionResourceLease.process,
        ): AssistantInputRuntime = AssistantInputRuntime(AssistantInputRole.WATCH, foreground, peers, transport(context),
            null, AndroidWatchMicrophoneFactory(context), nativeAvailable, lease, onDraft, onOutcome, SystemClock::elapsedRealtime)

        private fun transport(context: Context): (CallerForegroundSignal, SourcePeerBinding, InputRuntimeHandlers) -> InputRuntimeTransport =
            { foreground, peers, handlers ->
                val gms = GmsAssistantInputTransport(context.applicationContext, foreground, peers, handlers.control,
                    handlers.authorizeAudio, handlers.audio, handlers.sourceEnded, handlers.failure)
                InputRuntimeTransport(gms, gms::close)
            }
    }
}
