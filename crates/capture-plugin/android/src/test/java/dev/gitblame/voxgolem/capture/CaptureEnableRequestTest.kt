package dev.gitblame.voxgolem.capture

import org.junit.Assert.*
import org.junit.Test

class CaptureEnableRequestTest {
    @Test fun permissionDialogPauseDefersGrantUntilVisibleResume() {
        val cancelled = mutableListOf<Any>(); val enabled = mutableListOf<Any>()
        val pending = CaptureEnableRequest<Any> { cancelled += it }
        val request = Any()
        pending.begin(request)
        pending.permissionResult(request)
        pending.settle(false, true, true) { enabled += it }
        assertTrue(enabled.isEmpty())
        pending.settle(true, true, true) { enabled += it }
        pending.settle(true, true, true) { enabled += it }
        assertEquals(listOf(request), enabled)
        assertTrue(cancelled.isEmpty())
    }

    @Test fun backgroundDestroyReplacementAndRevocationCannotEnableStaleRequests() {
        val cancelled = mutableListOf<Any>(); val enabled = mutableListOf<Any>()
        val pending = CaptureEnableRequest<Any> { cancelled += it }
        val first = Any(); val second = Any()
        pending.begin(first); pending.begin(second)
        pending.permissionResult(first)
        pending.settle(true, true, true) { enabled += it }
        assertTrue(enabled.isEmpty())
        pending.cancel() // Activity STOP: genuine background, not permission-dialog PAUSE.
        pending.permissionResult(second)
        pending.settle(true, true, true) { enabled += it }
        pending.begin(first); pending.permissionResult(first)
        pending.settle(true, false, true) { enabled += it }
        pending.begin(second); pending.permissionResult(second)
        pending.settle(true, true, false) { enabled += it }
        assertTrue(enabled.isEmpty())
        assertEquals(listOf(first, second, first, second), cancelled)
    }
}
