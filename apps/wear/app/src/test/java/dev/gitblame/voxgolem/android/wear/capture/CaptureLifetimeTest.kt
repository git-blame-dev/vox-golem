package dev.gitblame.voxgolem.android.wear.capture

import org.junit.Assert.*
import org.junit.Test

class CaptureLifetimeTest {
    @Test fun retirementCannotReleaseAdmissionUntilEveryActualOperationHasCompleted() {
        val owner = CaptureLifetime.tryAcquire()!!
        val recovery = owner.retain()
        val repair = owner.retain()
        try {
            owner.close()
            owner.close()
            assertSame(owner, CaptureLifetime.current())
            assertNull(CaptureLifetime.tryAcquire())
            assertTrue(owner.permitsRecovery())
            recovery.close()
            recovery.close()
            assertSame(owner, CaptureLifetime.current())
            repair.close()
            assertNull(CaptureLifetime.current())
            val replacement = CaptureLifetime.tryAcquire()!!
            try {
                owner.close()
                recovery.close()
                repair.close()
                assertSame(replacement, CaptureLifetime.current())
            } finally { replacement.close() }
        } finally {
            owner.close()
            recovery.close()
            repair.close()
        }
    }

    @Test fun boundWriterIsNotAbandonedEvenAfterItsServiceRetires() {
        val owner = CaptureLifetime.tryAcquire()!!
        val operation = owner.retain()
        val id = "00000000-0000-4000-8000-000000000001"
        try {
            owner.bindCapture(id)
            assertFalse(owner.permitsRecovery())
            owner.close()
            assertTrue(CaptureLifetime.ownsCapture(id))
            assertNull(CaptureLifetime.tryAcquire())
            operation.close()
            assertFalse(CaptureLifetime.ownsCapture(id))
            assertNull(CaptureLifetime.current())
        } finally {
            owner.close()
            operation.close()
        }
    }
}
