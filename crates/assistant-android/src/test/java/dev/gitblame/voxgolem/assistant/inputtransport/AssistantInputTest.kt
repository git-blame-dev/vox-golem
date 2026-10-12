package dev.gitblame.voxgolem.assistant.inputtransport

import dev.gitblame.voxgolem.assistant.recognition.RecognitionResourceLease
import org.junit.Assert.*
import org.junit.Test
import java.util.UUID
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.DataInputStream
import java.io.DataOutputStream
import java.io.EOFException
import java.util.concurrent.CountDownLatch

class AssistantInputTest {
    private val source = UUID(0, 1).toString()
    private val target = UUID(0, 2).toString()
    private fun frame(kind: InputKind = InputKind.START, sequence: Long = 0, text: String = "") =
        InputFrame(kind, UUID(0, 3).toString(), source, target, "en-US", sequence, text)
    private class FakeSession : SuppliedInputSession {
        lateinit var events: SuppliedInputEvents
        var supported = true
        var deferredSupport = false
        lateinit var supportResult: (Boolean) -> Unit
        var started = false
        var finished = false
        var closed = false
        var closeFails = false
        override fun checkSupport(result: (Boolean) -> Unit) { supportResult = result; if (!deferredSupport) result(supported) }
        override fun start(events: SuppliedInputEvents) { this.events = events; started = true }
        override fun append(pcm: ByteArray): Boolean = true
        override fun finish() { finished = true }
        override fun close() { if (closeFails) error("synthetic teardown failure"); closed = true }
    }
    private class Fixture(val lease: RecognitionResourceLease = RecognitionResourceLease()) {
        val sdk = FakeSession()
        val replies = mutableListOf<InputFrame>()
        var foreground = true
        var trusted = true
        var available = true
        var opens = 0
        var now = 0L
        val phone = PhoneInputController(object : SuppliedInputFactory {
            override fun available(language: String) = available
            override fun open(language: String): SuppliedInputSession {
                assertEquals(RecognitionResourceLease.Kind.CHAT, lease.current()!!.kind)
                opens++; return sdk
            }
        }, CallerForegroundSignal { foreground }, SourcePeerBinding { _, _, _ -> trusted },
            lease, { _, reply -> replies += reply }, { now })
    }
    @Test fun codecRoundtripAndStrictBounds() {
        val original = frame(InputKind.PARTIAL, 7, "synthetic draft")
        assertEquals(original, AssistantInputWire.decode(AssistantInputWire.encode(original)))
        assertThrows(IllegalArgumentException::class.java) { AssistantInputWire.decode(AssistantInputWire.encode(original) + 0) }
        assertThrows(IllegalArgumentException::class.java) { AssistantInputWire.encode(frame(text = "x".repeat(9000))) }
        assertThrows(IllegalArgumentException::class.java) { AssistantInputWire.encode(frame().copy(session = "not-a-uuid")) }
        assertThrows(IllegalArgumentException::class.java) { AssistantInputWire.encode(frame().copy(language = "und")) }
    }
    @Test fun noteLeaseIsNeverPreemptedAndNoSdkIsOpened() {
        val f = Fixture()
        val note = f.lease.tryAcquire(RecognitionResourceLease.Kind.WATCH_NOTE_AUDIO)!!
        assertFalse(f.phone.receive("synthetic-watch", frame()))
        assertEquals(0, f.opens)
        assertSame(note, f.lease.current())
        note.close()
    }
    @Test fun unavailableOrUntrustedOrBackgroundNeverOpensSdk() {
        for (condition in 0..2) {
            val f = Fixture()
            when (condition) { 0 -> f.available = false; 1 -> f.trusted = false; 2 -> f.foreground = false }
            assertFalse(f.phone.receive("synthetic-watch", frame()))
            assertEquals(0, f.opens)
            assertNull(f.lease.current())
        }
    }
    @Test fun stopWaitsForAuthoritativeFinalAndLateCallbacksCannotTouchNewSession() {
        val f = Fixture()
        assertTrue(f.phone.receive("synthetic-watch", frame()))
        assertTrue(f.sdk.started)
        f.sdk.events.partial("synthetic available draft")
        val stopped = f.phone.stop(frame().session)
        assertFalse(stopped.isDone)
        f.sdk.events.final("synthetic authoritative final")
        assertTrue(stopped.join().authoritative)
        assertEquals("synthetic authoritative final", stopped.join().text)
        assertNull(f.lease.current())
        val previous = f.sdk.events
        assertTrue(f.phone.receive("synthetic-watch", frame().copy(session = UUID(0, 4).toString())))
        val count = f.replies.size
        previous.partial("synthetic late callback")
        previous.error("recognition_error_21")
        assertEquals(count, f.replies.size)
        assertNotNull(f.lease.current())
        f.phone.interrupt()
    }
    @Test fun earlyFinalAndTimeoutPreserveDraftWithoutAutosend() {
        val f = Fixture()
        f.phone.receive("synthetic-watch", frame())
        f.sdk.events.partial("synthetic draft")
        f.sdk.events.final("unrequested final")
        assertEquals(InputKind.ERROR, f.replies.last().kind)
        assertEquals("synthetic draft", f.replies.last().text)
        assertTrue(f.sdk.closed)
        val g = Fixture()
        g.phone.receive("synthetic-watch", frame())
        g.sdk.events.partial("synthetic timeout draft")
        val result = g.phone.stop(frame().session)
        g.now = 4000
        g.phone.tick()
        assertFalse(result.join().authoritative)
        assertEquals("synthetic timeout draft", result.join().text)
    }
    @Test fun sourceBindingSequenceAndForegroundAreRechecked() {
        val f = Fixture()
        f.phone.receive("synthetic-watch", frame())
        assertFalse(f.phone.receive("wrong-node", frame(InputKind.STOP, 1)))
        assertFalse(f.phone.receive("synthetic-watch", frame(InputKind.STOP, 0)))
        assertFalse(f.sdk.finished)
        f.foreground = false
        f.phone.tick()
        assertTrue(f.sdk.closed)
        assertEquals(InputKind.ERROR, f.replies.last().kind)
    }
    @Test fun unsupportedExactIntentClosesAndFailedRetirementRetainsLease() {
        val f = Fixture()
        f.sdk.supported = false
        f.phone.receive("synthetic-watch", frame())
        assertFalse(f.sdk.started)
        assertTrue(f.sdk.closed)
        assertNull(f.lease.current())
        val g = Fixture()
        g.phone.receive("synthetic-watch", frame())
        g.sdk.closeFails = true
        g.phone.interrupt()
        assertNotNull(g.lease.current())
        g.sdk.closeFails = false
        assertTrue(g.lease.retryFailedRetirement())
        assertNull(g.lease.current())
    }
    @Test fun stopMessageCannotOvertakePcmChannelEnd() {
        val f = Fixture(); f.phone.receive("synthetic-watch", frame())
        f.phone.receive("synthetic-watch", frame(InputKind.STOP, 1))
        assertFalse(f.sdk.finished)
        assertTrue(f.phone.audio("synthetic-watch", frame().session, ByteArray(640)))
        f.phone.sourceEnded("synthetic-watch", frame().session)
        assertTrue(f.sdk.finished)
        f.sdk.events.final("synthetic ordered final")
        assertEquals(InputKind.FINAL, f.replies.last().kind)
    }
    @Test fun cancellationArrivingBeforeDelayedStartFencesSession() {
        val f = Fixture()
        assertTrue(f.phone.receive("synthetic-watch", frame(InputKind.CANCEL, 1)))
        assertFalse(f.phone.receive("synthetic-watch", frame()))
        assertEquals(0, f.opens)
    }
    @Test fun finalTeardownFailureKeepsTextAsNonAuthoritativeDraft() {
        val f = Fixture(); f.phone.receive("synthetic-watch", frame())
        val stopped = f.phone.stop(frame().session)
        f.sdk.closeFails = true
        f.sdk.events.final("synthetic available final draft")
        assertFalse(stopped.join().authoritative)
        assertEquals("synthetic available final draft", stopped.join().text)
        assertEquals("sdk_retirement_failed", stopped.join().error)
        assertNotNull(f.lease.current())
        f.sdk.closeFails = false; assertTrue(f.lease.retryFailedRetirement())
    }
    @Test fun pcmChannelRequiresBoundedAlignedFramesAndExplicitEnd() {
        val body = ByteArray(640) { 7 }
        val bytes = ByteArrayOutputStream()
        val output = DataOutputStream(bytes)
        PcmChannelWire.write(output, body); PcmChannelWire.end(output)
        val input = DataInputStream(ByteArrayInputStream(bytes.toByteArray()))
        assertArrayEquals(body, PcmChannelWire.read(input)); assertNull(PcmChannelWire.read(input))
        assertThrows(EOFException::class.java) { PcmChannelWire.read(input) }
        assertThrows(IllegalArgumentException::class.java) { PcmChannelWire.write(output, ByteArray(641)) }
        assertThrows(IllegalArgumentException::class.java) { PcmChannelWire.read(DataInputStream(ByteArrayInputStream(byteArrayOf(0, 3, 1, 2, 3)))) }
        assertThrows(IllegalArgumentException::class.java) { PcmChannelWire.read(DataInputStream(ByteArrayInputStream(byteArrayOf(127, -1)))) }
    }
    @Test fun emptyFinalDoesNotEraseAvailableDraftOrAuthorizeSend() {
        val f = Fixture(); f.phone.receive("synthetic-watch", frame())
        f.sdk.events.partial("synthetic available draft")
        val stopped = f.phone.stop(frame().session)
        f.sdk.events.final("")
        assertEquals("synthetic available draft", stopped.join().text)
        assertFalse(stopped.join().authoritative)
        assertEquals("recognition_empty_final", stopped.join().error)
    }
    @Test fun supportQueryTimeoutRetiresAndLateApprovalCannotStart() {
        val f = Fixture(); f.sdk.deferredSupport = true
        f.phone.receive("synthetic-watch", frame())
        assertFalse(f.sdk.started); assertNotNull(f.lease.current())
        f.now = 5000; f.phone.tick()
        assertTrue(f.sdk.closed); assertNull(f.lease.current())
        f.sdk.supportResult(true)
        assertFalse(f.sdk.started)
        assertEquals(InputKind.ERROR, f.replies.last().kind)
    }
    @Test fun simultaneousNoteAndAssistantAdmissionHasOnlyOneOwner() {
        val lease = RecognitionResourceLease()
        val ready = CountDownLatch(2); val go = CountDownLatch(1)
        val tokens = arrayOfNulls<RecognitionResourceLease.Token>(2)
        val threads = listOf(RecognitionResourceLease.Kind.CHAT, RecognitionResourceLease.Kind.WATCH_NOTE_AUDIO)
            .mapIndexed { index, kind -> Thread {
                ready.countDown(); go.await(); tokens[index] = lease.tryAcquire(kind)
            }.apply { start() } }
        ready.await(); go.countDown(); threads.forEach { it.join() }
        assertEquals(1, tokens.count { it != null })
        assertSame(tokens.filterNotNull().single(), lease.current())
        tokens.filterNotNull().single().close()
        assertNull(lease.current())
    }
}
