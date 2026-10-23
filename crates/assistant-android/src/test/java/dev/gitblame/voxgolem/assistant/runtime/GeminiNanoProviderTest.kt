package dev.gitblame.voxgolem.assistant.runtime

import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow
import com.google.ai.edge.aicore.GenerativeAIException
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class GeminiNanoProviderTest {
    private val capabilityNote = "[System note: This local model has no browser or web tools. Do not claim to have searched or verified current information; disclose gaps when current evidence is needed.]\n\n"

    private fun joinWarmup(thread: Thread?) {
        thread?.join(5_000)
        assertFalse("Warmup did not finish", thread?.isAlive == true)
    }

    @Test fun concurrentPrewarmsShareOneAttemptAndSuccessfulWarmth() {
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val attempts = AtomicInteger()
        val engine = object : GeminiNanoEngine {
            override suspend fun prepareInferenceEngine() {
                attempts.incrementAndGet(); entered.countDown()
                check(release.await(5, TimeUnit.SECONDS))
            }
            override fun generateContentStream(prompt: String): Flow<String> = flow {}
            override fun close() {}
        }
        val provider = GeminiNanoProvider(RuntimeEnvironment.getApplication()) { engine }
        val first = provider.prewarm()
        assertTrue(entered.await(5, TimeUnit.SECONDS))
        val second = provider.prewarm()
        try { assertSame(first, second) } finally {
            release.countDown(); joinWarmup(first); joinWarmup(second)
        }
        joinWarmup(provider.prewarm())
        assertEquals(1, attempts.get())
    }

    @Test fun failedWarmupRetriesAndCloseAllowsFreshEngine() {
        val attempts = AtomicInteger()
        val factories = AtomicInteger()
        val closes = AtomicInteger()
        val provider = GeminiNanoProvider(RuntimeEnvironment.getApplication()) {
            factories.incrementAndGet()
            object : GeminiNanoEngine {
                override suspend fun prepareInferenceEngine() {
                    if (attempts.incrementAndGet() == 1) error("synthetic preparation failure")
                }
                override fun generateContentStream(prompt: String): Flow<String> = flow {}
                override fun close() { closes.incrementAndGet() }
            }
        }
        joinWarmup(provider.prewarm()); joinWarmup(provider.prewarm()); joinWarmup(provider.prewarm())
        assertEquals(2, attempts.get()); assertEquals(1, factories.get())
        provider.close(); joinWarmup(provider.prewarm())
        assertEquals(3, attempts.get()); assertEquals(2, factories.get()); assertEquals(1, closes.get())
    }

    @Test fun staleWarmupCompletionCannotMarkReplacementWarmOrClearItsAttempt() {
        val oldEntered = CountDownLatch(1)
        val releaseOld = CountDownLatch(1)
        val newEntered = CountDownLatch(1)
        val releaseNew = CountDownLatch(1)
        val factories = AtomicInteger()
        val provider = GeminiNanoProvider(RuntimeEnvironment.getApplication()) {
            val old = factories.incrementAndGet() == 1
            object : GeminiNanoEngine {
                override suspend fun prepareInferenceEngine() {
                    (if (old) oldEntered else newEntered).countDown()
                    check((if (old) releaseOld else releaseNew).await(5, TimeUnit.SECONDS))
                }
                override fun generateContentStream(prompt: String): Flow<String> = flow {}
                override fun close() {}
            }
        }
        val old = provider.prewarm()
        assertTrue(oldEntered.await(5, TimeUnit.SECONDS)); provider.close()
        val replacement = provider.prewarm()
        try {
            assertTrue(newEntered.await(5, TimeUnit.SECONDS)); releaseOld.countDown(); joinWarmup(old)
            assertSame(replacement, provider.prewarm())
        } finally {
            releaseOld.countDown(); releaseNew.countDown(); joinWarmup(old); joinWarmup(replacement)
        }
        assertEquals(2, factories.get())
    }

    @Test fun capabilityDisclosureAndSpokenFormatQualificationRemainExplicit() {
        val request = JSONObject().put("prompt", "Return JSON.").put("instructions",
            "Custom instruction.")
        assertEquals(capabilityNote + "System Instructions:\nCustom instruction.\n\nReturn JSON.",
            GeminiNanoProvider.formatPrompt(request))
        assertEquals(capabilityNote + "Hello", GeminiNanoProvider.formatPrompt(JSONObject().put("prompt", "Hello")))
    }

    @Test fun normalAppInstructionsCompactToExactlyPolicyAndCapabilityDisclosure() {
        val instructions = "${AssistantEngine.BASE_INSTRUCTIONS}\n\n${AssistantEngine.PLAIN_TEXT_POLICY}"
        val request = JSONObject().put("prompt", "Synthetic question.").put("instructions", instructions)
        assertEquals(capabilityNote + "System Instructions:\n${AssistantEngine.PLAIN_TEXT_POLICY}\n\nSynthetic question.",
            GeminiNanoProvider.formatPrompt(request))
    }

    @Test fun compactionPreservesCustomInstructionsAlongsideAppPolicy() {
        val custom = "Preserve synthetic_id and return the requested JSON."
        val instructions = "${AssistantEngine.BASE_INSTRUCTIONS}\n\n$custom\n\n${AssistantEngine.PLAIN_TEXT_POLICY}"
        val request = JSONObject().put("prompt", "Synthetic question.").put("instructions", instructions)
        assertEquals(capabilityNote + "System Instructions:\n$custom\n\n${AssistantEngine.PLAIN_TEXT_POLICY}\n\nSynthetic question.",
            GeminiNanoProvider.formatPrompt(request))
    }

    @Test fun boundedHistoryPreservesExactSuffixFallbackAndCharacterBudget() {
        val messages = JSONArray()
        repeat(100) { messages.put(JSONObject().put("role", "user").put("content", "Discarded $it")) }
        messages.put(JSONObject().put("role", "user").put("content", "a".repeat(281)))
        messages.put(JSONObject().put("role", "assistant").put("content", "b".repeat(281)))
        messages.put(JSONObject().put("role", "user").put("content", "c".repeat(281)))
        messages.put(JSONObject().put("role", "assistant").put("content", "Short\n\nSources:\nhttps://example.invalid/source"))
        messages.put(JSONObject().put("role", "user").put("content", "Current"))
        messages.put(JSONObject().put("role", "tool").put("content", "Ignored"))
        messages.put(JSONObject().put("role", "user").put("content", "   "))
        messages.put(JSONObject.NULL)
        val expected = capabilityNote + "Conversation History:\nAssistant: ${"b".repeat(280)}...\nUser: ${"c".repeat(280)}...\nAssistant: Short\n\nSources:\nhttps://example.invalid/source\n\nUser: Current\nAssistant:"
        assertEquals(expected, GeminiNanoProvider.formatPrompt(JSONObject().put("messages", messages)))
        assertEquals(expected, GeminiNanoProvider.formatPrompt(JSONObject().put("messages", messages).put("prompt", "Current")))
        assertEquals(capabilityNote + "Assistant fallback", GeminiNanoProvider.formatPrompt(JSONObject().put("messages",
            JSONArray().put(JSONObject().put("role", "assistant").put("content", "Assistant fallback")))))
    }

    @Test fun historyTraversalStopsAfterFiveEligibleTurns() {
        var reads = 0
        val messages = object : JSONArray() {
            override fun optJSONObject(index: Int): JSONObject? { reads++; return super.optJSONObject(index) }
        }
        repeat(100) { messages.put(JSONObject().put("role", "user").put("content", "Turn $it")) }
        assertEquals(capabilityNote + "Conversation History:\nUser: Turn 96\nUser: Turn 97\nUser: Turn 98\nUser: Turn 99\n\nUser: Direct\nAssistant:",
            GeminiNanoProvider.formatPrompt(JSONObject().put("prompt", "Direct").put("messages", messages)))
        assertEquals(5, reads)
    }

    @Test fun sourceHeadingsDoNotCollapseDifferentPriorUserContentIntoTheCurrentPrompt() {
        val messages = JSONArray()
            .put(JSONObject().put("role", "assistant").put("content", "Previous"))
            .put(JSONObject().put("role", "user").put("content", "Current\n\nSources:\nhttps://example.invalid/source"))
        assertEquals(capabilityNote + "Conversation History:\nAssistant: Previous\nUser: Current\n\nSources:\nhttps://example.invalid/source\n\nUser: Current\nAssistant:",
            GeminiNanoProvider.formatPrompt(JSONObject().put("prompt", "Current").put("messages", messages)))
    }

    private class FakeEngine(
        val chunks: List<String> = listOf("Hello", " from", " Gemini Nano"),
        val onStreamPrompt: ((String) -> Unit)? = null
    ) : GeminiNanoEngine {
        var prewarmed = false
        override suspend fun prepareInferenceEngine() { prewarmed = true }
        override fun generateContentStream(prompt: String): Flow<String> = flow {
            onStreamPrompt?.invoke(prompt)
            for (chunk in chunks) emit(chunk)
        }
        override fun close() {}
    }

    @Test fun sourceHeadingsInsideTheCurrentUserRequestReachTheSelectedEngineVerbatim() {
        val current = "  Summarize this supplied text.\n\nSources:\nhttps://example.invalid/synthetic\nKeep the instruction after the source heading.  "
        var captured = ""
        val engine = FakeEngine(onStreamPrompt = { captured = it })
        val provider = GeminiNanoProvider(RuntimeEnvironment.getApplication()) { engine }
        val request = JSONObject().put("model", "gemini-nano").put("webEnabled", true)
            .put("messages", JSONArray().put(JSONObject().put("role", "user").put("content", current)))
        for (explicitPrompt in listOf(false, true)) {
            if (explicitPrompt) request.put("prompt", current)
            val result = JSONObject(provider.respond("synthetic-source-heading", "", request.toString(), RecordingCallback()))
            assertEquals("complete", result.getString("status"))
            assertTrue("legacy citation formatting must not delete or trim user instructions", captured.endsWith(current))
        }
        provider.close()
    }

    private class UnusedNative : ProviderNativeCalls {
        override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String = error("Unexpected provider dispatch")
        override fun cancel(requestId: String) {}
    }

    private class RecordingCallback : ProviderDeltaCallback {
        var ready = 0
        var dispatched = 0
        val deltas = mutableListOf<String>()
        val statuses = mutableListOf<String>()
        val firstTexts = mutableListOf<Long>()
        override fun onReady() { ready++ }
        override fun onDelta(text: String) { deltas.add(text) }
        override fun onStatus(status: String) { statuses.add(status) }
        override fun onFirstText(milliseconds: Long) { firstTexts.add(milliseconds) }
        override fun onRequestDispatched() { dispatched++ }
    }

    @Test fun standardInferenceStreamsDeltasAndCompletes() {
        val provider = GeminiNanoProvider(RuntimeEnvironment.getApplication()) { FakeEngine() }
        val callback = RecordingCallback()
        val result = JSONObject(provider.respond("synthetic-stream", "/synthetic/auth",
            JSONObject().put("prompt", "Synthetic question").put("webEnabled", false).toString(), callback))
        assertEquals("complete", result.getString("status")); assertEquals("output_text", result.getString("contentType"))
        assertEquals("Hello from Gemini Nano", result.getString("text")); assertFalse(result.has("speechText"))
        assertEquals("gemini-nano", result.getString("returnedModel"))
        assertTrue(result.has("firstTextMs")); assertTrue(result.has("completedMs"))
        assertEquals(1, callback.ready); assertEquals(1, callback.dispatched)
        assertEquals(listOf("Hello", " from", " Gemini Nano"), callback.deltas)
        assertEquals(1, callback.firstTexts.size); assertTrue(callback.statuses.isEmpty())
    }

    @Test fun plainCurrentAndMixedPromptsStayOnChosenLocalEngineWithoutSearchOrFallback() {
        // There is deliberately no native-search dependency to inject or invoke.
        for (web in listOf(false, true)) {
            for (prompt in listOf("Hello", "What is today's synthetic beacon status?",
                "Explain beacons and check today's synthetic beacon status.")) {
                var captured = ""
                val provider = GeminiNanoProvider(RuntimeEnvironment.getApplication()) {
                    FakeEngine(onStreamPrompt = { captured = it })
                }
                val callback = RecordingCallback()
                val request = JSONObject().put("prompt", prompt).put("webEnabled", web)
                    .put("searchMode", "research") // Older peer field is inert, not a control.
                val result = JSONObject(provider.respond("synthetic-local", "/synthetic/auth", request.toString(), callback))
                assertEquals("complete", result.getString("status"))
                assertEquals(capabilityNote + prompt, captured)
                assertTrue(callback.statuses.isEmpty()); assertFalse(result.has("route")); assertFalse(result.has("searchStatus"))
                assertEquals("gemini-nano", result.getString("returnedModel"))
                provider.close()
            }
        }
    }

    private fun failingLegacyRequest(errorCode: Int): JSONObject {
        val provider = GeminiNanoProvider(RuntimeEnvironment.getApplication()) {
            object : GeminiNanoEngine {
                override suspend fun prepareInferenceEngine() {}
                override fun generateContentStream(prompt: String): Flow<String> = flow { throw LegacyNanoFailureFixtures.inference(errorCode) }
                override fun close() {}
            }
        }
        return JSONObject(provider.respond("synthetic-sdk-error", "/synthetic/auth",
            JSONObject().put("prompt", "A synthetic request").put("webEnabled", false).toString(), RecordingCallback()))
    }

    @Test fun legacySdkFailuresKeepExistingProviderErrorContract() {
        val cases = mapOf(
            GenerativeAIException.ErrorCode.NOT_AVAILABLE to "aicore_not_available",
            GenerativeAIException.ErrorCode.NEEDS_SYSTEM_UPDATE to "aicore_needs_update",
            GenerativeAIException.ErrorCode.BUSY to "aicore_busy",
            GenerativeAIException.ErrorCode.RESPONSE_PROCESSING_ERROR to "aicore_unavailable",
            GenerativeAIException.ErrorCode.REQUEST_TOO_LARGE to "too_large")
        for ((sdkCode, expectedCode) in cases) {
            val outcome = failingLegacyRequest(sdkCode)
            assertEquals("error", outcome.getString("status")); assertEquals(expectedCode, outcome.getString("code"))
            assertFalse(outcome.getString("message").contains("synthetic SDK failure"))
        }
    }

    @Test fun legacySdkCancellationDoesNotBecomeInternalError() {
        assertEquals("cancelled", failingLegacyRequest(GenerativeAIException.ErrorCode.CANCELLED).getString("status"))
    }

    @Test fun emptyLegacyStreamCannotBeReportedAsSuccessfulAnswer() {
        val provider = GeminiNanoProvider(RuntimeEnvironment.getApplication()) { FakeEngine(chunks = emptyList()) }
        val outcome = JSONObject(provider.respond("synthetic-empty", "/synthetic/auth",
            JSONObject().put("prompt", "A synthetic request").put("webEnabled", false).toString(), RecordingCallback()))
        assertEquals("error", outcome.getString("status")); assertEquals("invalid_response", outcome.getString("code"))
    }

    @Test fun multiTurnConversationHistoryPreservesSourceTextWithoutInventingFreshEvidence() {
        var captured = ""
        val provider = GeminiNanoProvider(RuntimeEnvironment.getApplication()) { FakeEngine(onStreamPrompt = { captured = it }) }
        val messages = JSONArray()
            .put(JSONObject().put("role", "user").put("content", "Tell me about synthetic beacons."))
            .put(JSONObject().put("role", "assistant").put("content", "A synthetic beacon.\n\nSources:\nhttps://example.invalid/beacon"))
            .put(JSONObject().put("role", "user").put("content", "What about its color?"))
        val request = JSONObject().put("messages", messages).put("webEnabled", true)
            .put("instructions", "You are a helpful assistant. Be concise.")
        assertEquals("complete", JSONObject(provider.respond("synthetic-history", "", request.toString(), RecordingCallback())).getString("status"))
        assertTrue(captured.contains("System Instructions:")); assertTrue(captured.contains("https://example.invalid/beacon"))
        assertTrue(captured.contains("Conversation History:\nUser: Tell me about synthetic beacons.\nAssistant: A synthetic beacon."))
        assertTrue(captured.endsWith("User: What about its color?\nAssistant:"))
    }

    @Test fun prewarmCallsPrepareInferenceEngineOnPhoneAndSkipsOnWatch() {
        val phoneEngine = FakeEngine()
        val phone = GeminiNanoProvider(RuntimeEnvironment.getApplication()) { phoneEngine }
        joinWarmup(phone.prewarm()); assertTrue(phoneEngine.prewarmed)
        val watchEngine = FakeEngine()
        val watch = GeminiNanoProvider(RuntimeEnvironment.getApplication()) { watchEngine }
        val router = AgyProviderRouter(RuntimeEnvironment.getApplication(), UnusedNative(), UnusedNative(), watch, UnusedNative())
        router.prewarmSelection("gemini-nano", true); assertFalse(watchEngine.prewarmed)
    }

    @Test fun phoneNanoPrewarmDoesNotInitializeCustomNative() {
        val engine = FakeEngine()
        val nano = GeminiNanoProvider(RuntimeEnvironment.getApplication()) { engine }
        val custom = object : ProviderNativeCalls {
            override fun prewarm(model: String, authPath: String, webEnabled: Boolean) { fail("Nano must not warm Custom native") }
            override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String = error("Not used")
            override fun cancel(requestId: String) {}
        }
        val router = AgyProviderRouter(RuntimeEnvironment.getApplication(), custom, UnusedNative(), nano, null)
        router.prewarm("gemini-nano", "", true)
        joinWarmup(nano.prewarm()); assertTrue(engine.prewarmed)
        nano.close()
    }

    @Test fun preCancelledAndReadyCancelledRequestsSkipGeneration() {
        var streams = 0
        val provider = GeminiNanoProvider(RuntimeEnvironment.getApplication()) { FakeEngine(onStreamPrompt = { streams++ }) }
        provider.cancel("synthetic-pre-cancel")
        val callback = RecordingCallback()
        assertEquals("cancelled", JSONObject(provider.respond("synthetic-pre-cancel", "", "{}", callback)).getString("status"))
        assertEquals(0, callback.ready)
        val readyCancel = object : ProviderDeltaCallback {
            override fun onReady() { provider.cancel("synthetic-ready-cancel") }
            override fun onDelta(text: String) { fail("Cancelled request emitted text") }
        }
        assertEquals("cancelled", JSONObject(provider.respond("synthetic-ready-cancel", "", "{}", readyCancel)).getString("status"))
        assertEquals(0, streams)
    }

    @Test fun routerDispatchesGeminiNanoSeparatelyFromAgyAndDefault() {
        val nano = GeminiNanoProvider(RuntimeEnvironment.getApplication()) { FakeEngine() }
        var agyCalls = 0
        var customCalls = 0
        fun native(onCall: () -> Unit) = object : ProviderNativeCalls {
            override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String {
                onCall(); return """{"status":"complete"}"""
            }
            override fun cancel(requestId: String) {}
        }
        val router = AgyProviderRouter(RuntimeEnvironment.getApplication(), native { customCalls++ }, native { agyCalls++ }, nano)
        val callback = RecordingCallback()
        val result = JSONObject(router.respond("synthetic-nano", "", JSONObject().put("model", "gemini-nano").put("prompt", "Hi").toString(), callback))
        assertEquals("gemini-nano", result.getString("returnedModel")); assertEquals(0, agyCalls); assertEquals(0, customCalls)
        router.respond("synthetic-agy", "", JSONObject().put("model", "agy-flash-high").toString(), callback)
        assertEquals(1, agyCalls); assertEquals(0, customCalls)
        router.respond("synthetic-custom", "", JSONObject().put("model", "custom-sol-high").toString(), callback)
        assertEquals(1, customCalls)
    }
}
