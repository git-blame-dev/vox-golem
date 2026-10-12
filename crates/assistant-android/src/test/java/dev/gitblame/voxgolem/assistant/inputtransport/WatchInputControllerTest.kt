package dev.gitblame.voxgolem.assistant.inputtransport

import dev.gitblame.voxgolem.assistant.recognition.RecognitionResourceLease
import org.junit.Assert.*
import org.junit.Test
import java.util.UUID

class WatchInputControllerTest {
    private class FakeTransport : AssistantAudioTransport {
        val controls = mutableListOf<InputFrame>()
        lateinit var opened: (Boolean) -> Unit
        var audioOpened = 0
        var ended = false
        var cancelled = false
        override fun control(node: String, frame: InputFrame) { controls += frame }
        override fun openAudio(node: String, session: String, result: (Boolean) -> Unit) { audioOpened++; opened = result }
        override fun audio(pcm: ByteArray) = true
        override fun finishAudio() { ended = true }
        override fun cancelAudio() { cancelled = true }
    }
    private class Fixture {
        val lease = RecognitionResourceLease()
        val transport = FakeTransport()
        var native = false
        var foreground = true
        var now = 0L
        var opens = 0
        var started = false
        var closed = false
        val source = UUID(0, 10).toString()
        val target = UUID(0, 11).toString()
        val controller = WatchInputController(transport, WatchMicrophoneFactory {
            assertEquals(RecognitionResourceLease.Kind.CHAT, lease.current()!!.kind)
            opens++
            object : WatchMicrophone {
                override fun start(pcm: (ByteArray) -> Boolean, error: () -> Unit) { started = true }
                override fun close() { closed = true }
            }
        }, CallerForegroundSignal { foreground }, SourcePeerBinding { node, peer, local -> node == "synthetic-phone" && peer == target && local == source },
            { native }, lease, {}, { now })
        fun start() = controller.start("synthetic-phone", source, target, "en-US")
        fun reply(kind: InputKind, sequence: Long, text: String = "") = transport.controls.first().copy(kind = kind,
            source = target, target = source, sequence = sequence, text = text)
    }
    @Test fun nativeAvailableAndNoteBusyNeverStartFallback() {
        val f = Fixture(); f.native = true
        assertNull(f.start()); assertTrue(f.transport.controls.isEmpty()); assertEquals(0, f.opens)
        f.native = false
        val note = f.lease.tryAcquire(RecognitionResourceLease.Kind.CAPTURE_LIVE)!!
        assertNull(f.start()); assertSame(note, f.lease.current()); assertTrue(f.transport.controls.isEmpty())
        note.close()
    }
    @Test fun channelReadyBeforeMicAndStopReturnsOnlyAuthoritativeFinal() {
        val f = Fixture(); val session = f.start()!!
        assertEquals(0, f.opens)
        assertTrue(f.controller.receive("synthetic-phone", f.reply(InputKind.READY, 1)))
        assertEquals(0, f.opens)
        f.transport.opened(true)
        assertTrue(f.started); assertEquals(1, f.opens)
        f.controller.receive("synthetic-phone", f.reply(InputKind.PARTIAL, 2, "synthetic draft"))
        val result = f.controller.stop(session)
        assertFalse(result.isDone); assertTrue(f.closed); assertTrue(f.transport.ended)
        assertEquals(InputKind.STOP, f.transport.controls.last().kind)
        f.controller.receive("synthetic-phone", f.reply(InputKind.FINAL, 3, "synthetic authoritative"))
        assertEquals("synthetic authoritative", result.join().text); assertTrue(result.join().authoritative)
        assertNull(f.lease.current())
    }
    @Test fun homeInterruptCancelsAndLateChannelReadyCannotOpenMic() {
        val f = Fixture(); f.start()!!
        f.controller.receive("synthetic-phone", f.reply(InputKind.READY, 1))
        f.controller.interrupt()
        f.transport.opened(true)
        assertEquals(0, f.opens); assertFalse(f.started)
        assertTrue(f.transport.cancelled); assertEquals(InputKind.CANCEL, f.transport.controls.last().kind)
        assertNull(f.lease.current())
    }
    @Test fun wrongNodeReplayAndUnrequestedFinalNeverSendConversation() {
        val f = Fixture(); f.start()!!
        assertFalse(f.controller.receive("wrong-peer", f.reply(InputKind.READY, 1)))
        assertEquals(0, f.transport.audioOpened)
        f.controller.receive("synthetic-phone", f.reply(InputKind.PARTIAL, 2, "synthetic draft"))
        assertFalse(f.controller.receive("synthetic-phone", f.reply(InputKind.PARTIAL, 1, "synthetic old draft")))
        f.controller.receive("synthetic-phone", f.reply(InputKind.FINAL, 3, "synthetic unrequested"))
        assertNull(f.lease.current())
        assertEquals(InputKind.CANCEL, f.transport.controls.last().kind)
    }
    @Test fun timeoutPreservesAvailableTextAndNoFinalIsInvented() {
        val f = Fixture(); val session = f.start()!!
        f.controller.receive("synthetic-phone", f.reply(InputKind.PARTIAL, 1, "synthetic saved-in-ui draft"))
        val result = f.controller.stop(session)
        f.now = 5000; f.controller.tick()
        assertFalse(result.join().authoritative)
        assertEquals("synthetic saved-in-ui draft", result.join().text)
        assertEquals("final_timeout", result.join().error)
    }
}
