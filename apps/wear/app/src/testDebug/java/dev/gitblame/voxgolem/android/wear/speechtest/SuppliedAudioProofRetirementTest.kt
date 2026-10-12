package dev.gitblame.voxgolem.android.wear.speechtest

import android.os.Looper
import dev.gitblame.voxgolem.assistant.inputtransport.SuppliedInputEvents
import dev.gitblame.voxgolem.assistant.inputtransport.SuppliedInputSession
import dev.gitblame.voxgolem.assistant.recognition.RecognitionResourceLease
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.util.concurrent.atomic.AtomicBoolean

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class SuppliedAudioProofRetirementTest {
    @Test fun failedCloseSettlesOnceAndBlocksReplacementUntilExplicitRetrySucceeds() {
        val lease = RecognitionResourceLease()
        val owner = SuppliedAudioProofRetirement(lease)
        val session = FailingCloseSession(failures = 1)
        val delivered = mutableListOf<Boolean>()
        assertTrue(owner.acquire())
        assertEquals(RecognitionResourceLease.Kind.CAPTURE_PROBE, lease.current()?.kind)
        owner.attach(session)

        owner.settle { delivered += it }
        assertTrue(owner.completed)
        assertThrows(IllegalStateException::class.java) { owner.acquire() }
        assertEquals(listOf(false), delivered)
        assertEquals(1, session.closeAttempts)
        val held = lease.current()
        assertNotNull(held)
        assertFalse(SuppliedAudioProofRetirement(lease).acquire())
        assertNull(lease.tryAcquire(RecognitionResourceLease.Kind.CHAT))
        owner.settle { delivered += it }
        assertEquals(1, session.closeAttempts)
        assertSame(held, lease.current())

        assertTrue(lease.retryFailedRetirement())
        assertEquals(2, session.closeAttempts)
        assertNull(lease.current())
        assertEquals(listOf(false), delivered)
        assertTrue(owner.retryRetirement())
        assertEquals(2, session.closeAttempts)
        assertThrows(IllegalStateException::class.java) { owner.acquire() }
        val replacement = SuppliedAudioProofRetirement(lease)
        assertTrue(replacement.acquire())
        replacement.settle { assertTrue(it) }
        assertNull(lease.current())
    }

    @Test fun foregroundLossRetriesCompletedOwnerWithoutLosingTheFailedSession() {
        val lease = RecognitionResourceLease()
        val owner = SuppliedAudioProofRetirement(lease)
        val session = FailingCloseSession(failures = 2)
        var results = 0
        assertTrue(owner.acquire())
        owner.attach(session)
        owner.settle { assertFalse(it); results++ }
        val held = lease.current()

        // The activity's completed onStop path performs cleanup, not another settlement.
        assertFalse(owner.retryRetirement())
        assertEquals(2, session.closeAttempts)
        assertSame(held, lease.current())
        assertFalse(SuppliedAudioProofRetirement(lease).acquire())
        assertTrue(owner.retryRetirement())
        assertEquals(3, session.closeAttempts)
        assertNull(lease.current())
        owner.settle { results++ }
        assertEquals(1, results)
    }

    @Test fun unavailableOrPreAllocationFailureDoesNotLeakAToken() {
        val lease = RecognitionResourceLease()
        val unavailable = SuppliedAudioProofRetirement(lease)
        unavailable.settle { assertTrue(it) }
        assertNull(lease.current())

        val initializationFailure = SuppliedAudioProofRetirement(lease)
        assertTrue(initializationFailure.acquire())
        initializationFailure.settle { assertTrue(it) }
        assertNull(lease.current())
    }

    @Test fun defaultOwnersShareTheExistingProcessLease() {
        val lease = RecognitionResourceLease.process
        assertNull(lease.current())
        val owner = SuppliedAudioProofRetirement()
        assertTrue(owner.acquire())
        try {
            assertEquals(RecognitionResourceLease.Kind.CAPTURE_PROBE, lease.current()?.kind)
            assertFalse(SuppliedAudioProofRetirement().acquire())
        } finally {
            owner.settle { assertTrue(it) }
        }
        assertNull(lease.current())
    }

    @Test fun offMainRetryRetainsOwnershipWithoutCallingTheSdk() {
        val lease = RecognitionResourceLease()
        val owner = SuppliedAudioProofRetirement(lease)
        val session = FailingCloseSession(failures = 1)
        assertTrue(owner.acquire())
        owner.attach(session)
        owner.settle { assertFalse(it) }
        val held = lease.current()
        val result = AtomicBoolean(true)
        val worker = Thread { result.set(lease.retryFailedRetirement()) }
        worker.start()
        worker.join(1_000)
        assertFalse(worker.isAlive)
        assertFalse(result.get())
        assertEquals(1, session.closeAttempts)
        assertSame(held, lease.current())
        assertTrue(lease.retryFailedRetirement())
        assertEquals(2, session.closeAttempts)
        assertNull(lease.current())
    }

    private class FailingCloseSession(private val failures: Int) : SuppliedInputSession {
        var closeAttempts = 0
            private set
        override fun close() {
            assertEquals(Looper.getMainLooper(), Looper.myLooper())
            closeAttempts++
            if (closeAttempts <= failures) error("Synthetic supplied-input retirement failure")
        }
        override fun checkSupport(result: (Boolean) -> Unit) = error("No SDK support request in this test")
        override fun start(events: SuppliedInputEvents) = error("No recognition start in this test")
        override fun append(pcm: ByteArray): Boolean = error("No input allocation in this test")
        override fun finish() = error("No source stream in this test")
    }
}
