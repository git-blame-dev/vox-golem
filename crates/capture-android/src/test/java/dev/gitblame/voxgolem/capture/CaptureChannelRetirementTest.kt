package dev.gitblame.voxgolem.capture

import org.junit.Assert.*
import org.junit.Test
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger

class CaptureChannelRetirementTest {
    @Test fun asynchronousFailureRetainsAdmissionAndExactCloserUntilBoundedRecoverySucceeds() {
        val owner = CaptureChannelRetirementOwner(1)
        val callbacks = java.util.concurrent.CopyOnWriteArrayList<(Boolean) -> Unit>()
        val lifetime = owner.reserve(20)!!
        lifetime.acquiredNative { callbacks += it }
        repeat(3) { index ->
            await { callbacks.size > index }
            callbacks[index](false)
        }
        assertEquals(3, callbacks.size); assertEquals(1, owner.size)
        assertTrue(lifetime.isRetired); assertFalse(lifetime.nativeRetired)
        assertNull(owner.reserve()); assertFalse(lifetime.ifActive { fail("Revoked channel committed") })
        owner.retryFailedRetirements()
        await { callbacks.size == 4 }
        callbacks[3](true)
        await { owner.size == 0 }
        val replacement = owner.reserve()!!
        replacement.acquiredNative { it(true) }
        callbacks[0](false); callbacks[1](true)
        assertEquals(1, owner.size); assertFalse(replacement.nativeRetired); assertFalse(replacement.isRetired)
        replacement.close(); await { owner.size == 0 }
    }
    @Test fun synchronousCloseExceptionIsBoundedAndMatchingCallbackConfirmsRetirement() {
        val owner = CaptureChannelRetirementOwner(1)
        val attempts = AtomicInteger()
        val lifetime = owner.reserve(20)!!
        lifetime.acquiredNative { attempts.incrementAndGet(); throw IllegalStateException("Synthetic /private/retirement handle failure") }
        await { lifetime.retirementExhausted }
        assertEquals(1, owner.size); assertFalse(lifetime.nativeRetired); assertNull(owner.reserve())
        lifetime.confirmNativeRetired()
        await { owner.size == 0 }
        assertTrue(lifetime.nativeRetired)
        assertEquals(3, attempts.get())
    }
    @Test fun nativeConfirmationCannotFreeSlotBeforeLateStreamAcquisitionAndFailedStreamRecovery() {
        val owner = CaptureChannelRetirementOwner(1)
        val lifetime = owner.reserve(20)!!
        lifetime.acquiredNative { it(true) }
        lifetime.beginStreamAcquisition()
        await { lifetime.nativeRetired }
        assertEquals(1, owner.size); assertNull(owner.reserve())
        val attempts = AtomicInteger()
        val succeed = java.util.concurrent.atomic.AtomicBoolean(false)
        assertFalse(lifetime.streamAcquisitionCompleted(AutoCloseable {
            attempts.incrementAndGet()
            if (!succeed.get()) throw IllegalStateException("Synthetic stream retirement failure")
        }))
        await { lifetime.retirementExhausted }
        assertEquals(1, owner.size); assertNull(owner.reserve())
        succeed.set(true); owner.retryFailedRetirements()
        await { owner.size == 0 }
        assertTrue(attempts.get() in 4..6)
    }
    @Test fun pendingNativeOpenDeliveredAfterRevocationIsRetiredWithoutLosingAdmissionAccounting() {
        val owner = CaptureChannelRetirementOwner(1)
        val lifetime = owner.reserve(20)!!
        await { lifetime.isRetired }
        assertEquals(1, owner.size); assertNull(owner.reserve())
        val closed = CountDownLatch(1)
        assertFalse(lifetime.acquiredNative { completed -> closed.countDown(); completed(true) })
        assertTrue(closed.await(2, TimeUnit.SECONDS)); await { owner.size == 0 }
    }
    @Test fun blockedNativeRequestDoesNotBlockOtherDeadlineRevocations() {
        val owner = CaptureChannelRetirementOwner(2)
        val blocked = CountDownLatch(1)
        val release = CountDownLatch(1)
        val first = owner.reserve(20)!!
        first.acquiredNative { completed -> blocked.countDown(); check(release.await(2, TimeUnit.SECONDS)); completed(true) }
        val second = owner.reserve(30)!!
        second.acquiredNative { it(true) }
        try {
            assertTrue(blocked.await(2, TimeUnit.SECONDS))
            await { second.isRetired }
            assertFalse(second.ifActive { fail("Deadline scheduler was blocked by native IO") })
            assertEquals(2, owner.size)
        } finally { release.countDown() }
        await { owner.size == 0 }
    }
    @Test fun saturatedNativeCallbackDispatchBackpressuresRatherThanForgettingAnotherHandle() {
        val owner = CaptureChannelRetirementOwner(1)
        val held = owner.reserve()!!
        held.acquiredNative { }
        held.close()
        val started = CountDownLatch(1)
        val adopted = CountDownLatch(1)
        val worker = Thread {
            started.countDown()
            val late = owner.awaitReservation(20)
            late.acquiredNative { it(true) }
            late.close(); adopted.countDown()
        }
        worker.start(); assertTrue(started.await(2, TimeUnit.SECONDS))
        assertEquals(1L, adopted.count); assertEquals(1, owner.size)
        held.confirmNativeRetired()
        assertTrue(adopted.await(2, TimeUnit.SECONDS)); worker.join(2_000)
        assertFalse(worker.isAlive); await { owner.size == 0 }
    }
    private fun await(condition: () -> Boolean) {
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(5)
        while (!condition()) { check(System.nanoTime() < deadline) { "Synthetic retirement did not complete" }; Thread.sleep(1) }
    }
}
