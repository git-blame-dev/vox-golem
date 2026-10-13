package dev.gitblame.voxgolem.assistant.runtime

import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.File
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class AgyPrewarmTest {
    private val callback = object : ProviderDeltaCallback {
        override fun onReady() { fail("Unavailable or overlapping requests must not become ready") }
        override fun onDelta(text: String) { fail("No response expected") }
    }
    private class LiveProcess : Process() {
        var destroyed = false
        override fun getOutputStream() = ByteArrayOutputStream()
        override fun getInputStream() = ByteArrayInputStream(byteArrayOf())
        override fun getErrorStream() = ByteArrayInputStream(byteArrayOf())
        override fun waitFor() = 0
        override fun exitValue(): Int = throw IllegalThreadStateException("synthetic live process")
        override fun destroy() { destroyed = true }
    }

    private fun AgyProviderNative.set(name: String, value: Any?) {
        javaClass.getDeclaredField(name).apply { isAccessible = true }.set(this, value)
    }
    private fun AgyProviderNative.get(name: String): Any? =
        javaClass.getDeclaredField(name).apply { isAccessible = true }.get(this)
    private fun AgyProviderNative.acceptSelection(): Long =
        javaClass.getDeclaredMethod("advancePrewarmSelection").apply { isAccessible = true }.invoke(this) as Long
    private fun AgyProviderNative.acquire(model: String, generation: Long = acceptSelection()): Boolean =
        javaClass.getDeclaredMethod("acquirePrewarmProcess", File::class.java, String::class.java, String::class.java,
            Long::class.javaPrimitiveType)
            .apply { isAccessible = true }.invoke(this, File("/synthetic/nonexistent-agy"), model, "high", generation) as Boolean

    @Test fun delayedSelectionCannotReplaceNewerAcquiredProcess() {
        val provider = AgyProviderNative(RuntimeEnvironment.getApplication())
        val delayedA = provider.acceptSelection()
        val acceptedB = provider.acceptSelection()
        val processB = LiveProcess()
        provider.set("process", processB)
        provider.set("activeModel", "claude-sonnet-5-5-high")
        provider.set("activeSessionId", "synthetic-session-b")
        assertTrue(provider.acquire("claude-sonnet-5-5-high", acceptedB))
        assertFalse(provider.acquire("gemini-3.8-flash-high", delayedA))
        assertSame(processB, provider.get("process"))
        assertFalse(processB.destroyed)
        assertEquals("synthetic-session-b", provider.get("activeSessionId"))
    }

    @Test fun nonAgySelectionInvalidatesDelayedAgyAcquisitionEvenForSameLiveModel() {
        val provider = AgyProviderNative(RuntimeEnvironment.getApplication())
        val delayedA = provider.acceptSelection()
        // The router must forward non-AGY selections to invalidate pending work too.
        val defaultNative = object : ProviderNativeCalls {
            override fun respond(requestId: String, authPath: String, requestJson: String,
                callback: ProviderDeltaCallback) = error("No response expected")
            override fun cancel(requestId: String) {}
        }
        AgyProviderRouter(RuntimeEnvironment.getApplication(), defaultNative, provider).prewarm("custom-sol-high")
        val process = LiveProcess()
        provider.set("process", process)
        provider.set("activeModel", "gemini-3.8-flash-high")
        assertFalse(provider.acquire("gemini-3.8-flash-high", delayedA))
        assertSame(process, provider.get("process"))
        assertFalse(process.destroyed)
    }

    @Test fun sameModelPrewarmPreservesLiveProcessAndConversationSession() {
        val provider = AgyProviderNative(RuntimeEnvironment.getApplication())
        val process = LiveProcess()
        provider.set("process", process)
        provider.set("activeModel", "gemini-3.8-flash-high")
        provider.set("activeSessionId", "synthetic-session")
        assertTrue(provider.acquire("gemini-3.8-flash-high"))
        assertSame(process, provider.get("process"))
        assertEquals("synthetic-session", provider.get("activeSessionId"))
        assertFalse(process.destroyed)
    }

    @Test fun prewarmCannotReplaceProcessOwnedByAnActiveRequest() {
        val provider = AgyProviderNative(RuntimeEnvironment.getApplication())
        val process = LiveProcess()
        provider.set("process", process)
        provider.set("activeModel", "gemini-3.8-flash-high")
        provider.set("activeSessionId", "synthetic-session")
        provider.set("activeRequestId", "synthetic-request")
        assertFalse(provider.acquire("claude-sonnet-5-5-high"))
        assertSame(process, provider.get("process"))
        assertFalse(process.destroyed)
    }

    @Test fun unavailableEngineReleasesRequestOwnershipOnEveryReturn() {
        val provider = AgyProviderNative(RuntimeEnvironment.getApplication())
        repeat(2) { index ->
            val result = org.json.JSONObject(provider.respond("synthetic-request-$index", "", "{}", callback))
            assertEquals("unsupported_model", result.getString("code"))
            assertNull(provider.get("activeRequestId"))
        }
    }

    @Test fun overlappingRespondCannotStealRequestOwnership() {
        val provider = AgyProviderNative(RuntimeEnvironment.getApplication())
        provider.set("activeRequestId", "synthetic-original")
        val result = org.json.JSONObject(provider.respond("synthetic-overlap", "", "{}", callback))
        assertEquals("busy", result.getString("code"))
        assertEquals("synthetic-original", provider.get("activeRequestId"))
    }

    @Test fun completedRequestCanReuseSessionButLateCancelCannotDestroyIt() {
        val provider = AgyProviderNative(RuntimeEnvironment.getApplication())
        val process = LiveProcess()
        provider.set("process", process)
        provider.set("activeModel", "gemini-3.8-flash-high")
        provider.set("activeSessionId", "synthetic-session")
        provider.cancel("synthetic-completed")
        assertTrue(provider.acquire("gemini-3.8-flash-high"))
        assertFalse(process.destroyed)
        assertEquals("synthetic-session", provider.get("activeSessionId"))
    }
}
