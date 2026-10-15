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

    @Test fun geminiNanoNeverRoutesToAgyAndSonnetEffortsResolveToDistinctAgyModels() {
        fun provider(tag: String, hits: MutableList<String>) = object : ProviderNativeCalls {
            override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String {
                hits += tag; return """{"status":"complete"}"""
            }
            override fun cancel(requestId: String) {}
        }
        val hits = mutableListOf<String>()
        val router = AgyProviderRouter(RuntimeEnvironment.getApplication(), provider("default", hits), provider("agy", hits), provider("nano", hits))
        listOf("gemini-nano", "agy-sonnet-low", "agy-sonnet-high", "custom-sol-high").forEachIndexed { i, model ->
            router.respond("r$i", "", """{"model":"$model"}""", object : ProviderDeltaCallback { override fun onReady() {}; override fun onDelta(text: String) {} })
        }
        assertEquals(listOf("nano", "agy", "agy", "default"), hits)
        val agy = AgyProviderNative(RuntimeEnvironment.getApplication())
        assertEquals("claude-sonnet-5-5-low", agy.resolveTargetModel("agy-sonnet-low"))
        assertEquals("claude-sonnet-5-5-high", agy.resolveTargetModel("agy-sonnet-high"))
    }

    @Test fun watchRoutesAgyAndNanoThroughThePhoneRelayButNotCustom() {
        fun provider(tag: String, hits: MutableList<String>) = object : ProviderNativeCalls {
            override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String {
                hits += tag; return """{"status":"complete"}"""
            }
            override fun cancel(requestId: String) {}
        }
        val hits = mutableListOf<String>()
        val router = AgyProviderRouter(RuntimeEnvironment.getApplication(), provider("default", hits), provider("localAgy", hits),
            provider("nano", hits), provider("phoneRelay", hits))
        listOf("agy-sonnet-low", "agy-flash-high", "gemini-nano", "custom-sol-high").forEachIndexed { i, model ->
            router.respond("w$i", "", """{"model":"$model"}""", object : ProviderDeltaCallback { override fun onReady() {}; override fun onDelta(text: String) {} })
        }
        assertEquals(listOf("phoneRelay", "phoneRelay", "phoneRelay", "default"), hits)
    }
}
