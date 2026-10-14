package dev.gitblame.voxgolem.assistant.runtime

import android.content.Context
import android.os.SystemClock
import android.util.Log
import com.google.ai.edge.aicore.DownloadConfig
import com.google.ai.edge.aicore.GenerationConfig
import com.google.ai.edge.aicore.GenerativeAIException
import com.google.ai.edge.aicore.GenerativeModel
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.runBlocking
import org.json.JSONObject
import java.io.BufferedReader
import java.io.InputStreamReader
import java.net.HttpURLConnection
import java.net.URL
import java.net.URLEncoder
import java.nio.charset.StandardCharsets
import java.util.concurrent.ConcurrentHashMap

interface GeminiNanoEngine : AutoCloseable {
    suspend fun prepareInferenceEngine()
    fun generateContentStream(prompt: String): Flow<String>
}

class DefaultGeminiNanoEngine(private val context: Context) : GeminiNanoEngine {
    private var model: GenerativeModel? = null

    private fun getModel(): GenerativeModel {
        model?.let { return it }
        val config = GenerationConfig.Builder().also {
            it.context = context
        }.build()
        val m = GenerativeModel(config, DownloadConfig())
        model = m
        return m
    }

    override suspend fun prepareInferenceEngine() {
        getModel().prepareInferenceEngine()
    }

    override fun generateContentStream(prompt: String): Flow<String> = flow {
        getModel().generateContentStream(prompt).collect { response ->
            val text = response.text
            if (!text.isNullOrEmpty()) {
                emit(text)
            }
        }
    }

    override fun close() {
        try {
            model?.close()
        } catch (_: Exception) {}
        model = null
    }
}

class GeminiNanoProvider(
    private val context: Context,
    private val engineFactory: (Context) -> GeminiNanoEngine = { DefaultGeminiNanoEngine(it) }
) : ProviderNativeCalls {

    companion object {
        private const val TAG = "GeminiNanoProvider"
        const val MODEL_NAME = "gemini-nano"
    }

    private val lock = Any()
    private var engine: GeminiNanoEngine? = null
    private val activeJobs = ConcurrentHashMap<String, Job>()
    private val cancelledRequests = ConcurrentHashMap<String, Boolean>()
    internal var searchProvider: ((String) -> String)? = null

    private fun getOrCreateModel(): GeminiNanoEngine {
        synchronized(lock) {
            engine?.let { return it }
            val created = engineFactory(context)
            engine = created
            return created
        }
    }

    fun prewarm() {
        try {
            val eng = getOrCreateModel()
            runBlocking(Dispatchers.IO) {
                eng.prepareInferenceEngine()
            }
        } catch (e: Throwable) {
            Log.w(TAG, "Gemini Nano prewarm failed: ${e.message}")
        }
    }

    override fun respond(
        requestId: String,
        authPath: String,
        requestJson: String,
        callback: ProviderDeltaCallback
    ): String {
        if (isCancelled(requestId)) return cancelledResult()
        try {
            val reqObj = try { JSONObject(requestJson) } catch (_: Exception) { JSONObject() }
            val webEnabled = reqObj.optBoolean("webEnabled", true)
        val searchMode = reqObj.optString("searchMode", "quick")
        val instructions = reqObj.optString("instructions", "").trim()

        val messages = reqObj.optJSONArray("messages")
        var promptText = reqObj.optString("prompt", "")
        if (promptText.isEmpty() && messages != null && messages.length() > 0) {
            val last = messages.getJSONObject(messages.length() - 1)
            promptText = last.optString("content", "")
        }

        callback.onReady()

        var searchContext = ""
        if (webEnabled && (searchMode == "verify" || searchMode == "research")) {
            if (isCancelled(requestId)) return cancelledResult()
            callback.onStatus("Searching")
            val evidence = searchWeb(promptText)
            if (isCancelled(requestId)) return cancelledResult()
            if (evidence.isNotBlank()) {
                callback.onStatus("Reading")
                searchContext = "[Web Search Context:\n$evidence\n]\n\n"
            } else {
                callback.onStatus("Fallback")
            }
        }

        val spokenDirective = if (instructions.contains("spoken turn")) {
            "[System note: For this spoken response, give a concise answer in 1-2 short natural sentences.]\n\n"
        } else ""

        val finalPrompt = "$searchContext$spokenDirective$promptText"

        if (isCancelled(requestId)) return cancelledResult()

        val timing = AgyRequestTiming(SystemClock::elapsedRealtime)
        callback.onRequestDispatched()

        val accumulated = StringBuilder()
        var errorResult: String? = null

        val eng = try {
            getOrCreateModel()
        } catch (e: Throwable) {
            return JSONObject()
                .put("status", "error")
                .put("code", "aicore_unavailable")
                .put("message", "Failed to initialize Gemini Nano: ${e.message}")
                .toString()
        }

        try {
            runBlocking(Dispatchers.IO) {
                val currentJob = coroutineContext[Job]
                if (currentJob != null) {
                    activeJobs[requestId] = currentJob
                }

                if (isCancelled(requestId)) {
                    errorResult = cancelledResult()
                    return@runBlocking
                }

                try {
                    val stream = eng.generateContentStream(finalPrompt)
                    stream.collect { delta ->
                        if (isCancelled(requestId)) {
                            errorResult = cancelledResult()
                            currentJob?.cancel()
                            return@collect
                        }
                        if (delta.isNotEmpty()) {
                            timing.onForwardedText(delta)?.let { elapsed ->
                                callback.onFirstText(elapsed)
                            }
                            accumulated.append(delta)
                            callback.onDelta(delta)
                        }
                    }
                } catch (_: CancellationException) {
                    errorResult = cancelledResult()
                } catch (e: GenerativeAIException) {
                    val code = when (e.errorCode) {
                        GenerativeAIException.ErrorCode.NOT_AVAILABLE -> "aicore_not_available"
                        GenerativeAIException.ErrorCode.NEEDS_SYSTEM_UPDATE -> "aicore_needs_update"
                        GenerativeAIException.ErrorCode.BUSY -> "aicore_busy"
                        else -> "provider_failure"
                    }
                    errorResult = JSONObject()
                        .put("status", "error")
                        .put("code", code)
                        .put("message", e.message ?: "Gemini Nano generation failure")
                        .toString()
                } catch (e: Throwable) {
                    errorResult = JSONObject()
                        .put("status", "error")
                        .put("code", "internal_error")
                        .put("message", e.message ?: "Inference error")
                        .toString()
                } finally {
                    activeJobs.remove(requestId)
                }
            }
        } catch (e: Throwable) {
            if (isCancelled(requestId)) return cancelledResult()
            return JSONObject()
                .put("status", "error")
                .put("code", "internal_error")
                .put("message", e.message ?: "Execution error")
                .toString()
        }

        if (errorResult != null) return errorResult!!
        if (isCancelled(requestId)) return cancelledResult()

        val text = accumulated.toString()
        val completedMs = timing.completedMs()
        return JSONObject()
            .put("status", "complete")
            .put("contentType", "output_text")
            .put("text", text)
            .put("returnedModel", MODEL_NAME)
            .put("firstTextMs", timing.firstTextMs)
            .put("completedMs", completedMs)
            .toString()
        } finally {
            cancelledRequests.remove(requestId)
            activeJobs.remove(requestId)
        }
    }

    override fun cancel(requestId: String) {
        cancelledRequests[requestId] = true
        activeJobs.remove(requestId)?.cancel()
    }

    fun close() {
        synchronized(lock) {
            try {
                engine?.close()
            } catch (_: Exception) {}
            engine = null
        }
    }

    private fun isCancelled(requestId: String): Boolean =
        cancelledRequests[requestId] == true

    private fun cancelledResult(): String =
        JSONObject().put("status", "cancelled").toString()

    internal fun searchWeb(query: String): String {
        searchProvider?.let { return it(query) }
        if (query.isBlank()) return ""

        // Primary: DuckDuckGo Instant Answer API
        try {
            val url = URL("https://api.duckduckgo.com/?q=${URLEncoder.encode(query, "UTF-8")}&format=json&no_html=1&skip_disambig=1")
            val conn = (url.openConnection() as HttpURLConnection).apply {
                connectTimeout = 3000
                readTimeout = 3000
                setRequestProperty("User-Agent", "VoxGolem/1.0")
            }
            if (conn.responseCode == 200) {
                val body = conn.inputStream.bufferedReader(StandardCharsets.UTF_8).use { it.readText() }
                val json = JSONObject(body)
                val abstractText = json.optString("AbstractText", "")
                if (abstractText.isNotBlank()) {
                    return abstractText
                }
            }
        } catch (_: Exception) {}

        // Fallback: Wikipedia Search API
        try {
            val url = URL("https://en.wikipedia.org/w/api.php?action=query&list=search&srsearch=${URLEncoder.encode(query, "UTF-8")}&utf8=&format=json")
            val conn = (url.openConnection() as HttpURLConnection).apply {
                connectTimeout = 3000
                readTimeout = 3000
                setRequestProperty("User-Agent", "VoxGolem/1.0")
            }
            if (conn.responseCode == 200) {
                val body = conn.inputStream.bufferedReader(StandardCharsets.UTF_8).use { it.readText() }
                val json = JSONObject(body)
                val searchArr = json.optJSONObject("query")?.optJSONArray("search")
                if (searchArr != null && searchArr.length() > 0) {
                    val snippets = mutableListOf<String>()
                    val count = minOf(searchArr.length(), 3)
                    for (i in 0 until count) {
                        val item = searchArr.getJSONObject(i)
                        val title = item.optString("title", "")
                        val snippet = item.optString("snippet", "")
                            .replace(Regex("<[^>]*>"), "")
                            .replace("&quot;", "\"")
                            .replace("&amp;", "&")
                        if (snippet.isNotBlank()) {
                            snippets.add("$title: $snippet")
                        }
                    }
                    if (snippets.isNotEmpty()) {
                        return snippets.joinToString("\n")
                    }
                }
            }
        } catch (_: Exception) {}

        return ""
    }
}
