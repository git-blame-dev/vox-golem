package dev.gitblame.voxgolem.assistant.inputtransport

import dev.gitblame.voxgolem.assistant.recognition.RecognitionResourceLease
import java.util.UUID
import java.util.concurrent.CompletableFuture

interface AssistantAudioTransport {
    fun control(node: String, frame: InputFrame)
    fun openAudio(node: String, session: String, result: (Boolean) -> Unit)
    fun audio(pcm: ByteArray): Boolean
    fun finishAudio()
    fun cancelAudio()
}
interface WatchMicrophone : AutoCloseable { fun start(pcm: (ByteArray) -> Boolean, error: () -> Unit) }
fun interface WatchMicrophoneFactory { fun open(): WatchMicrophone }

/** Main-dispatcher owner. Native availability takes precedence; Home/window exit calls interrupt. */
class WatchInputController(
    private val transport: AssistantAudioTransport,
    private val microphone: WatchMicrophoneFactory,
    private val foreground: CallerForegroundSignal,
    private val peers: SourcePeerBinding,
    private val nativeAvailable: (String) -> Boolean,
    private val lease: RecognitionResourceLease = RecognitionResourceLease.process,
    private val onDraft: (String) -> Unit = {},
    private val now: () -> Long = { System.nanoTime() / 1000000 },
    private val onOutcome: (InputOutcome) -> Unit = {},
) {
    private class Active(val node: String, val start: InputFrame, val token: RecognitionResourceLease.Token, val at: Long) {
        var microphone: WatchMicrophone? = null
        var incoming = 0L
        var outgoing = 0L
        var opening = false
        var stopping: Long? = null
        var draft = ""
        val result = CompletableFuture<InputOutcome>()
    }
    private var active: Active? = null
    fun start(node: String, source: String, target: String, language: String): String? {
        if (active != null || nativeAvailable(language) || !foreground.isForeground() || !peers.accepts(node, target, source)) return null
        val frame = InputFrame(InputKind.START, UUID.randomUUID().toString(), source, target, language, 0)
        AssistantInputWire.validate(frame)
        val token = lease.tryAcquire(RecognitionResourceLease.Kind.CHAT) ?: return null
        val a = Active(node, frame, token, now()); active = a
        try { transport.control(node, frame) } catch (_: Exception) { terminate(a, false, "phone_unavailable") }
        return if (active === a) frame.session else null
    }
    fun receive(node: String, frame: InputFrame): Boolean {
        val a = active ?: return false
        if (runCatching { AssistantInputWire.validate(frame) }.isFailure || node != a.node || frame.session != a.start.session ||
            frame.source != a.start.target || frame.target != a.start.source || frame.language != a.start.language ||
            frame.sequence <= a.incoming || !peers.accepts(node, frame.source, frame.target)) return false
        if (frame.kind !in setOf(InputKind.READY, InputKind.PARTIAL, InputKind.FINAL, InputKind.ERROR)) return false
        a.incoming = frame.sequence
        if (!foreground.isForeground()) { terminate(a, false, "foreground_lost"); return false }
        when (frame.kind) {
            InputKind.READY -> {
                if (a.stopping != null || a.opening || a.microphone != null) return false
                a.opening = true
                try { transport.openAudio(a.node, a.start.session) { opened ->
                    if (active !== a || a.stopping != null) return@openAudio
                    if (!opened || !eligible(a)) { terminate(a, false, "phone_unavailable"); return@openAudio }
                    try {
                        a.microphone = microphone.open()
                        a.microphone!!.start({ transport.audio(it) }, {
                            if (active === a) terminate(a, false, "watch_audio_failed")
                        })
                    } catch (_: Exception) { terminate(a, false, "watch_audio_failed") }
                } } catch (_: Exception) { terminate(a, false, "phone_unavailable") }
            }
            InputKind.PARTIAL -> { a.draft = frame.text; runCatching { onDraft(a.draft) } }
            InputKind.FINAL -> {
                if (a.stopping == null) terminate(a, false, "unrequested_final")
                else if (frame.text.isBlank()) terminate(a, false, "recognition_empty_final")
                else { a.draft = frame.text; terminate(a, true, null) }
            }
            InputKind.ERROR -> { if (frame.text.isNotEmpty()) a.draft = frame.text; terminate(a, false, frame.error.ifEmpty { "phone_input_failed" }) }
            else -> Unit
        }
        return true
    }
    fun stop(session: String): CompletableFuture<InputOutcome> {
        val a = active?.takeIf { it.start.session == session }
            ?: return CompletableFuture.completedFuture(InputOutcome(session, "", false, "session_unavailable"))
        if (a.stopping == null) {
            a.stopping = now()
            try {
                a.microphone?.close(); a.microphone = null
                transport.finishAudio()
                transport.control(a.node, a.start.copy(kind = InputKind.STOP, sequence = ++a.outgoing))
            } catch (_: Exception) { terminate(a, false, "stop_failed") }
        }
        return a.result
    }
    fun tick() {
        val a = active ?: return
        when {
            !eligible(a) -> terminate(a, false, "foreground_or_peer_lost")
            a.stopping?.let { now() - it >= AssistantInputWire.IDLE_MILLIS } == true -> terminate(a, false, "final_timeout")
            now() - a.at >= AssistantInputWire.MAX_SESSION_MILLIS -> terminate(a, false, "session_timeout")
            a.microphone == null && a.stopping == null && now() - a.at >= AssistantInputWire.IDLE_MILLIS -> terminate(a, false, "phone_unavailable")
        }
    }
    fun interrupt() { active?.let { terminate(it, false, "interrupted") } }
    fun transportFailure(node: String, session: String) {
        active?.takeIf { it.node == node && it.start.session == session }?.let { terminate(it, false, "transport_failed") }
    }
    private fun eligible(a: Active) = foreground.isForeground() && peers.accepts(a.node, a.start.target, a.start.source)
    private fun terminate(a: Active, authoritative: Boolean, error: String?) {
        if (active !== a) return
        active = null
        val retired = runCatching { a.microphone?.close() }.isSuccess
        if (retired) a.token.close() else a.token.onRetirementRetry {
            runCatching { a.microphone?.close(); a.token.close(); true }.getOrDefault(false)
        }
        runCatching { transport.cancelAudio() }
        if (!authoritative) runCatching { transport.control(a.node, a.start.copy(kind = InputKind.CANCEL, sequence = ++a.outgoing)) }
        val outcome = InputOutcome(a.start.session, a.draft, authoritative && retired,
            if (!retired) "watch_audio_retirement_failed" else error)
        a.result.complete(outcome); runCatching { onDraft(a.draft) }; runCatching { onOutcome(outcome) }
    }
}
