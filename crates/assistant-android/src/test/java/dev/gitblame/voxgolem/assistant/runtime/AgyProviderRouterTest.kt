package dev.gitblame.voxgolem.assistant.runtime

import java.util.concurrent.Executor
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class AgyProviderRouterTest {
    @Test fun defaultRouteRetainsItsOwnAsynchronousRetirementAndCancellationOwner() {
        lateinit var physicallyRetired: () -> Unit
        var defaultCancels = 0; var defaultRetirementRegistrations = 0
        var agyCalls = 0; var terminalRetirements = 0
        val default = object : ProviderNativeCalls {
            override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String {
                callback.onReady(); return """{"status":"complete"}"""
            }
            override fun cancel(requestId: String) { defaultCancels++ }
            override fun whenRetired(requestId: String, retired: () -> Unit) { defaultRetirementRegistrations++; physicallyRetired = retired }
        }
        val agy = object : ProviderNativeCalls {
            override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String {
                agyCalls++; callback.onReady(); return """{"status":"complete"}"""
            }
            override fun cancel(requestId: String) { fail("Default request cancellation must not be sent to Agy") }
            override fun whenRetired(requestId: String, retired: () -> Unit) { agyCalls++; retired() }
        }
        val worker = AssistantRequestWorker(Executor { it.run() }, AgyProviderRouter(RuntimeEnvironment.getApplication(), default, agy))
        val listener = object : AssistantRequestListener {
            override fun onDelta(requestId: String, text: String) { fail("No text expected") }
            override fun onOutcome(requestId: String, outcomeJson: String) {}
            override fun onRetired(requestId: String) { terminalRetirements++ }
        }
        try {
            assertTrue(worker.submit("synthetic-default", "", """{"model":"custom-sol-high"}""", listener))
            assertEquals(1, defaultRetirementRegistrations); assertEquals(0, agyCalls); assertEquals(0, terminalRetirements)
            assertFalse(worker.submit("synthetic-overlap", "", """{"model":"agy-flash-high"}""", listener))
            worker.cancel(); assertEquals(1, defaultCancels)
            physicallyRetired(); physicallyRetired()
            assertEquals(1, terminalRetirements)
            assertTrue(worker.submit("synthetic-agy", "", """{"model":"agy-flash-high"}""", listener))
            assertEquals(2, agyCalls); assertEquals(2, terminalRetirements)
        } finally { worker.close() }
    }
}
