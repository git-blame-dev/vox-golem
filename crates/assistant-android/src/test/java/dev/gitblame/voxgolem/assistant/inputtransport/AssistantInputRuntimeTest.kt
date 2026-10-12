package dev.gitblame.voxgolem.assistant.inputtransport

import android.os.Looper
import dev.gitblame.voxgolem.assistant.recognition.RecognitionResourceLease
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.annotation.LooperMode
import java.time.Duration
import java.util.UUID

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
@LooperMode(LooperMode.Mode.PAUSED)
class AssistantInputRuntimeTest {
    private val local = UUID(0, 301).toString()
    private val peer = UUID(0, 302).toString()
    private val session = UUID(0, 303).toString()
    private fun request() = InputFrame(InputKind.START, session, peer, local, "en-US", 0)
    private inner class Peers : AssistantInputPeers {
        var known = true
        var active = false
        var refreshed = 0
        var closed = false
        override fun selected(): AssistantInputPeerRoute? = if (active && known)
            AssistantInputPeerRoute("synthetic-peer", local, peer) else null
        override fun accepts(node: String, source: String, target: String) =
            selected()?.let { node == it.nodeId && source == it.peerInstallation && target == it.localInstallation } == true
        override fun onForeground(active: Boolean) { this.active = active }
        override fun refresh() { refreshed++ }
        override fun close() { active = false; closed = true }
    }
    private class Port : AssistantAudioTransport {
        val sent = mutableListOf<InputFrame>()
        var closed = false
        var opened = 0
        var ended = false
        var cancelled = false
        override fun control(node: String, frame: InputFrame) { sent += frame }
        override fun openAudio(node: String, session: String, result: (Boolean) -> Unit) { opened++; result(true) }
        override fun audio(pcm: ByteArray) = true
        override fun finishAudio() { ended = true }
        override fun cancelAudio() { cancelled = true }
    }
    private class Speech : SuppliedInputFactory, SuppliedInputSession {
        var supported = true
        var opens = 0
        var started = false
        var finished = false
        var closed = false
        lateinit var events: SuppliedInputEvents
        override fun available(language: String) = supported
        override fun open(language: String): SuppliedInputSession { opens++; return this }
        override fun checkSupport(result: (Boolean) -> Unit) { result(supported) }
        override fun start(events: SuppliedInputEvents) { this.events = events; started = true }
        override fun append(pcm: ByteArray) = true
        override fun finish() { finished = true }
        override fun close() { closed = true }
    }
    private inner class Fixture(val role: AssistantInputRole = AssistantInputRole.PHONE) {
        var window = true
        var native = false
        var micOpened = 0
        var micClosed = false
        val peers = Peers()
        val speech = Speech()
        val ports = mutableListOf<Port>()
        val handlers = mutableListOf<InputRuntimeHandlers>()
        val outcomes = mutableListOf<InputOutcome>()
        val lease = RecognitionResourceLease()
        val runtime = AssistantInputRuntime(role, CallerForegroundSignal { window }, peers,
            { _, _, callbacks ->
                handlers += callbacks
                val port = Port(); ports += port
                InputRuntimeTransport(port) { port.closed = true }
            }, speech, WatchMicrophoneFactory {
                assertEquals(RecognitionResourceLease.Kind.CHAT, lease.current()!!.kind)
                micOpened++
                object : WatchMicrophone {
                    override fun start(pcm: (ByteArray) -> Boolean, error: () -> Unit) {}
                    override fun close() { micClosed = true }
                }
            }, { native }, lease, {}, { outcomes += it },
            { android.os.SystemClock.uptimeMillis() })
        fun resume() { runtime.onForeground(true) }
        fun control(frame: InputFrame, node: String = "synthetic-peer") = handlers.last().control(node, frame)
        fun watchReply(kind: InputKind, sequence: Long, text: String = "") = ports.last().sent.first().copy(
            kind = kind, source = peer, target = local, sequence = sequence, text = text)
    }
    @Test fun foregroundServerWiresActualControllerOnlyForBoundIncomingRequest() {
        val f = Fixture()
        try {
            assertTrue(f.ports.isEmpty()); f.resume(); assertEquals(1, f.ports.size); assertEquals(0, f.speech.opens)
            f.control(request(), "synthetic-wrong-node"); f.control(request().copy(source = UUID(0, 304).toString()))
            assertEquals(0, f.speech.opens)
            f.control(request())
            assertEquals(1, f.speech.opens); assertTrue(f.speech.started)
            assertEquals(InputKind.READY, f.ports.last().sent.last().kind)
            assertTrue(f.handlers.last().authorizeAudio("synthetic-peer", session))
            assertTrue(f.handlers.last().audio("synthetic-peer", session, ByteArray(640)))
            f.speech.events.partial("synthetic retained draft")
            f.control(request().copy(kind = InputKind.STOP, sequence = 1))
            assertFalse(f.speech.finished)
            f.handlers.last().sourceEnded("synthetic-peer", session)
            assertTrue(f.speech.finished)
            f.speech.events.final("synthetic authoritative final")
            assertTrue(f.outcomes.last().authoritative); assertNull(f.lease.current())
        } finally { f.runtime.close() }
    }
    @Test fun defaultProductionPhoneFactoryRemainsUnavailableAndRepliesWithTypingReason() {
        val peers = Peers(); lateinit var callbacks: InputRuntimeHandlers; val port = Port()
        val runtime = AssistantInputRuntime(AssistantInputRole.PHONE, CallerForegroundSignal { true }, peers,
            { _, _, handlers -> callbacks = handlers; InputRuntimeTransport(port) { port.closed = true } },
            AndroidSuppliedAudioFactory(RuntimeEnvironment.getApplication()), null, { false }, RecognitionResourceLease(), {}, {},
            { android.os.SystemClock.uptimeMillis() })
        try {
            runtime.onForeground(true); callbacks.control("synthetic-peer", request())
            assertEquals(InputKind.ERROR, port.sent.single().kind)
            assertEquals("supplied_audio_unavailable", port.sent.single().error)
            assertEquals(AssistantInputRoute.TYPING, runtime.availability("en-US").route)
            assertEquals(AssistantInputUnavailable.PROVIDER_UNAVAILABLE, runtime.availability("en-US").unavailable)
        } finally { runtime.close() }
    }
    @Test fun oneOwnedTickerStopsServerOnWindowLossAndFencesOldCallbacksAcrossResume() {
        val f = Fixture()
        try {
            f.resume(); f.resume(); assertEquals(1, f.ports.size)
            f.control(request()); f.speech.events.partial("synthetic exit draft")
            val stale = f.handlers.single()
            f.window = false
            shadowOf(Looper.getMainLooper()).idleFor(Duration.ofMillis(250))
            assertTrue(f.speech.closed); assertTrue(f.ports.single().closed); assertNull(f.lease.current())
            assertEquals("synthetic exit draft", f.outcomes.last().text)
            val refreshes = f.peers.refreshed
            shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(2))
            assertEquals(refreshes, f.peers.refreshed)
            f.window = true; f.resume(); assertEquals(2, f.ports.size)
            stale.control("synthetic-peer", request().copy(session = UUID(0, 305).toString()))
            assertEquals(1, f.speech.opens)
            f.runtime.close(); assertTrue(f.peers.closed)
        } finally { f.runtime.close() }
    }
    @Test fun watchPrefersExactNativeRouteAndUnknownPeerMeansTypingWithoutMic() {
        val f = Fixture(AssistantInputRole.WATCH)
        try {
            f.resume(); f.native = true
            assertEquals(AssistantInputRoute.NATIVE_ON_DEVICE, f.runtime.start("en-US").route)
            assertTrue(f.ports.single().sent.isEmpty()); assertEquals(0, f.micOpened)
            f.native = false; f.peers.known = false
            val unavailable = f.runtime.start("en-US")
            assertEquals(AssistantInputRoute.TYPING, unavailable.route)
            assertEquals(AssistantInputUnavailable.PEER_UNAVAILABLE, unavailable.unavailable)
            assertEquals(0, f.micOpened)
        } finally { f.runtime.close() }
    }
    @Test fun watchHandlerWiresReadyMicStopAndFinalWithoutSendingConversation() {
        val f = Fixture(AssistantInputRole.WATCH)
        try {
            f.resume(); val started = f.runtime.start("en-US")
            assertEquals(AssistantInputRoute.WATCH_PHONE, started.route); assertEquals(0, f.micOpened)
            f.control(f.watchReply(InputKind.READY, 1)); assertEquals(1, f.micOpened)
            f.control(f.watchReply(InputKind.PARTIAL, 2, "synthetic draft"))
            val result = f.runtime.stop(started.session!!)
            assertFalse(result.isDone); assertTrue(f.micClosed); assertTrue(f.ports.single().ended)
            f.control(f.watchReply(InputKind.FINAL, 3, "synthetic final"))
            assertTrue(result.join().authoritative); assertEquals("synthetic final", result.join().text)
            assertTrue(f.ports.single().sent.all { it.kind in setOf(InputKind.START, InputKind.STOP) })
        } finally { f.runtime.close() }
    }
    @Test fun watchdogTimeoutAndNoteBusyProduceTypedFallbackWithoutPreemption() {
        val f = Fixture(AssistantInputRole.WATCH)
        try {
            f.resume(); val note = f.lease.tryAcquire(RecognitionResourceLease.Kind.WATCH_NOTE_AUDIO)!!
            assertEquals(AssistantInputRoute.TYPING, f.runtime.start("en-US").route)
            assertSame(note, f.lease.current()); note.close()
            val started = f.runtime.start("en-US"); assertNotNull(started.session)
            shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(5))
            assertFalse(f.outcomes.last().authoritative); assertEquals("phone_unavailable", f.outcomes.last().error)
            assertEquals(0, f.micOpened); assertNull(f.lease.current())
        } finally { f.runtime.close() }
    }
    @Test fun remoteProviderUnavailableLatchesTypingForThisWindowWithoutMicOrAutoRetry() {
        val f = Fixture(AssistantInputRole.WATCH)
        try {
            f.resume(); f.runtime.start("en-US")
            f.control(f.watchReply(InputKind.ERROR, 1).copy(error = "supplied_audio_unavailable"))
            assertEquals(0, f.micOpened); assertNull(f.lease.current())
            assertEquals(AssistantInputUnavailable.PROVIDER_UNAVAILABLE, f.runtime.availability("en-US").unavailable)
            assertEquals(AssistantInputRoute.TYPING, f.runtime.start("en-US").route)
            assertEquals(1, f.ports.single().sent.count { it.kind == InputKind.START })
            f.runtime.onForeground(false); f.resume()
            assertEquals(AssistantInputRoute.WATCH_PHONE, f.runtime.availability("en-US").route)
            assertTrue(f.ports.last().sent.isEmpty())
        } finally { f.runtime.close() }
    }
    @Test fun calibratedEvidenceRequiresExactCurrentProviderOsAndLanguage() {
        val approved = NativeInputProviderStamp("synthetic.provider/Recognizer", 7, android.os.Build.FINGERPRINT)
        var current: NativeInputProviderStamp? = approved
        val evidence = NativeSuppliedAudioCalibration("en-US", approved) { current }
        assertTrue(evidence.permits("en-US")); assertFalse(evidence.permits("fr-FR"))
        current = approved.copy(packageVersion = 8); assertFalse(evidence.permits("en-US"))
        current = null; assertFalse(evidence.permits("en-US"))
        val changedOs = NativeSuppliedAudioCalibration("en-US", approved.copy(osBuild = "synthetic-other-os")) { approved }
        assertFalse(changedOs.permits("en-US"))
    }
}
