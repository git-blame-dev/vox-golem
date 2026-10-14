package dev.gitblame.voxgolem.assistant.runtime

import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class GeminiNanoProviderTest {

    private class FakeEngine(
        val chunks: List<String> = listOf("Hello", " from", " Gemini Nano"),
        val onStreamPrompt: ((String) -> Unit)? = null
    ) : GeminiNanoEngine {
        var prewarmed = false
        var closed = false

        override suspend fun prepareInferenceEngine() {
            prewarmed = true
        }

        override fun generateContentStream(prompt: String): Flow<String> = flow {
            onStreamPrompt?.invoke(prompt)
            for (chunk in chunks) {
                emit(chunk)
            }
        }

        override fun close() {
            closed = true
        }
    }

    private class RecordingCallback : ProviderDeltaCallback {
        val ready = mutableListOf<Boolean>()
        val deltas = mutableListOf<String>()
        val statuses = mutableListOf<String>()
        val firstTexts = mutableListOf<Long>()
        val dispatched = mutableListOf<Boolean>()

        override fun onReady() { ready.add(true) }
        override fun onDelta(text: String) { deltas.add(text) }
        override fun onStatus(status: String) { statuses.add(status) }
        override fun onFirstText(milliseconds: Long) { firstTexts.add(milliseconds) }
        override fun onRequestDispatched() { dispatched.add(true) }
    }

    @Test
    fun standardInferenceStreamsDeltasAndCompletes() {
        val fakeEngine = FakeEngine()
        val provider = GeminiNanoProvider(RuntimeEnvironment.getApplication(), engineFactory = { fakeEngine })
        val callback = RecordingCallback()

        val requestJson = JSONObject()
            .put("model", "gemini-nano")
            .put("prompt", "What is the capital of France?")
            .put("webEnabled", false)
            .put("searchMode", "quick")
            .toString()

        val resultStr = provider.respond("req-1", "/synthetic/auth", requestJson, callback)
        val result = JSONObject(resultStr)

        assertEquals("complete", result.getString("status"))
        assertEquals("output_text", result.getString("contentType"))
        assertEquals("Hello from Gemini Nano", result.getString("text"))
        assertEquals("gemini-nano", result.getString("returnedModel"))
        assertTrue(result.has("firstTextMs"))
        assertTrue(result.has("completedMs"))

        assertEquals(1, callback.ready.size)
        assertEquals(1, callback.dispatched.size)
        assertEquals(listOf("Hello", " from", " Gemini Nano"), callback.deltas)
        assertEquals(1, callback.firstTexts.size)
        assertEquals(0, callback.statuses.size)
    }

    @Test
    fun searchGroundedInferenceEmitsStatusesAndAugmentsPrompt() {
        var capturedPrompt = ""
        val fakeEngine = FakeEngine(onStreamPrompt = { capturedPrompt = it })
        val provider = GeminiNanoProvider(RuntimeEnvironment.getApplication(), engineFactory = { fakeEngine })
        provider.searchProvider = { query -> "Search evidence snippet for $query" }
        val callback = RecordingCallback()

        val requestJson = JSONObject()
            .put("model", "gemini-nano")
            .put("prompt", "Who won the game?")
            .put("webEnabled", true)
            .put("searchMode", "verify")
            .toString()

        val resultStr = provider.respond("req-2", "/synthetic/auth", requestJson, callback)
        val result = JSONObject(resultStr)

        assertEquals("complete", result.getString("status"))
        assertEquals(listOf("Searching", "Reading"), callback.statuses)
        assertTrue(capturedPrompt.contains("[Web Search Context:"))
        assertTrue(capturedPrompt.contains("Search evidence snippet for Who won the game?"))
        assertTrue(capturedPrompt.contains("Who won the game?"))
    }

    @Test
    fun searchFallbackEmittedWhenSearchReturnsEmpty() {
        var capturedPrompt = ""
        val fakeEngine = FakeEngine(onStreamPrompt = { capturedPrompt = it })
        val provider = GeminiNanoProvider(RuntimeEnvironment.getApplication(), engineFactory = { fakeEngine })
        provider.searchProvider = { "" }
        val callback = RecordingCallback()

        val requestJson = JSONObject()
            .put("model", "gemini-nano")
            .put("prompt", "Obscure fact question")
            .put("webEnabled", true)
            .put("searchMode", "research")
            .toString()

        val resultStr = provider.respond("req-3", "/synthetic/auth", requestJson, callback)
        val result = JSONObject(resultStr)

        assertEquals("complete", result.getString("status"))
        assertEquals(listOf("Searching", "Fallback"), callback.statuses)
        assertFalse(capturedPrompt.contains("[Web Search Context:"))
        assertTrue(capturedPrompt.contains("Obscure fact question"))
    }

    @Test
    fun prewarmCallsPrepareInferenceEngine() {
        val fakeEngine = FakeEngine()
        val provider = GeminiNanoProvider(RuntimeEnvironment.getApplication(), engineFactory = { fakeEngine })

        assertFalse(fakeEngine.prewarmed)
        provider.prewarm()
        assertTrue(fakeEngine.prewarmed)
    }

    @Test
    fun cancelAbortsActiveRequest() {
        val fakeEngine = FakeEngine(chunks = listOf("First", "Second"))
        val provider = GeminiNanoProvider(RuntimeEnvironment.getApplication(), engineFactory = { fakeEngine })
        val callback = RecordingCallback()

        provider.cancel("req-cancelled")
        val resultStr = provider.respond(
            "req-cancelled",
            "/synthetic/auth",
            JSONObject().put("prompt", "Hello").toString(),
            callback
        )
        val result = JSONObject(resultStr)
        assertEquals("cancelled", result.getString("status"))
    }

    @Test
    fun routerDispatchesGeminiNanoSeparatelyFromAgyAndDefault() {
        val fakeEngine = FakeEngine()
        val nanoProvider = GeminiNanoProvider(RuntimeEnvironment.getApplication(), engineFactory = { fakeEngine })

        var agyCalled = false
        val fakeAgy = object : ProviderNativeCalls {
            override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String {
                agyCalled = true
                return JSONObject().put("status", "complete").put("model", "agy").toString()
            }
            override fun cancel(requestId: String) {}
        }

        var defaultCalled = false
        val fakeDefault = object : ProviderNativeCalls {
            override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String {
                defaultCalled = true
                return JSONObject().put("status", "complete").put("model", "default").toString()
            }
            override fun cancel(requestId: String) {}
        }

        val router = AgyProviderRouter(
            RuntimeEnvironment.getApplication(),
            defaultNative = fakeDefault,
            agyNative = fakeAgy,
            geminiNanoNative = nanoProvider
        )

        val callback = RecordingCallback()

        // 1. gemini-nano should hit GeminiNanoProvider
        val nanoResult = router.respond("req-nano", "/auth", JSONObject().put("model", "gemini-nano").put("prompt", "Hi").toString(), callback)
        assertEquals("complete", JSONObject(nanoResult).getString("status"))
        assertEquals("gemini-nano", JSONObject(nanoResult).getString("returnedModel"))
        assertFalse(agyCalled)
        assertFalse(defaultCalled)

        // 2. agy-flash-high should hit agyNative
        router.respond("req-agy", "/auth", JSONObject().put("model", "agy-flash-high").put("prompt", "Hi").toString(), callback)
        assertTrue(agyCalled)
        assertFalse(defaultCalled)

        // 3. custom-sol-high should hit defaultNative
        router.respond("req-custom", "/auth", JSONObject().put("model", "custom-sol-high").put("prompt", "Hi").toString(), callback)
        assertTrue(defaultCalled)
    }
}
