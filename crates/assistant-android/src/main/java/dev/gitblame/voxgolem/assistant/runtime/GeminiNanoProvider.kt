package dev.gitblame.voxgolem.assistant.runtime

import android.content.Context
import android.content.pm.PackageManager
import android.os.SystemClock
import android.util.Log
import com.google.android.gms.tasks.Tasks
import com.google.android.gms.wearable.MessageClient
import com.google.android.gms.wearable.MessageEvent
import com.google.android.gms.wearable.Wearable
import com.google.mlkit.genai.common.DownloadStatus
import com.google.mlkit.genai.common.FeatureStatus
import com.google.mlkit.genai.common.GenAiException
import com.google.mlkit.genai.prompt.Generation
import com.google.mlkit.genai.prompt.GenerationConfig
import com.google.mlkit.genai.prompt.GenerativeModel
import com.google.mlkit.genai.prompt.ModelPreference
import com.google.mlkit.genai.prompt.ModelReleaseStage
import com.google.mlkit.genai.prompt.generationConfig
import com.google.mlkit.genai.prompt.modelConfig
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import org.json.JSONObject
import java.nio.charset.StandardCharsets
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean

interface GeminiNanoEngine : AutoCloseable {
    suspend fun prepareInferenceEngine()
    fun generateContentStream(prompt: String): Flow<String>
}

class DefaultGeminiNanoEngine(private val context: Context) : GeminiNanoEngine {
    companion object {
        private const val TAG = "DefaultGeminiNanoEngine"

        private val CANDIDATE_CONFIGS: List<GenerationConfig> = listOf(
            // 1. Preview Fast (Feature 646 - AICore on Pixel 8)
            generationConfig {
                modelConfig = modelConfig {
                    releaseStage = ModelReleaseStage.PREVIEW
                    preference = ModelPreference.FAST
                }
            },
            // 2. Preview Full (Feature 647 - AICore on Pixel 8)
            generationConfig {
                modelConfig = modelConfig {
                    releaseStage = ModelReleaseStage.PREVIEW
                    preference = ModelPreference.FULL
                }
            },
            // 3. Stable Full (Feature 636 - ML Kit default)
            generationConfig {
                modelConfig = modelConfig {
                    releaseStage = ModelReleaseStage.STABLE
                    preference = ModelPreference.FULL
                }
            },
            // 4. Stable Fast (Feature 645)
            generationConfig {
                modelConfig = modelConfig {
                    releaseStage = ModelReleaseStage.STABLE
                    preference = ModelPreference.FAST
                }
            }
        )
    }

    private val selectMutex = Mutex()
    @Volatile
    private var model: GenerativeModel? = null

    private suspend fun getOrSelectModel(): GenerativeModel {
        model?.let { return it }
        return selectMutex.withLock {
            model?.let { return it }

            var lastException: Throwable? = null
            var downloadableCandidate: GenerativeModel? = null

            for (config in CANDIDATE_CONFIGS) {
                val candidate = Generation.getClient(config)
                try {
                    val status = candidate.checkStatus()
                    Log.i(TAG, "Candidate config (stage=${config.modelConfig.releaseStage}, pref=${config.modelConfig.preference}) status: $status")
                    when (status) {
                        FeatureStatus.AVAILABLE -> {
                            Log.i(TAG, "Found AVAILABLE Gemini Nano model! Using it.")
                            downloadableCandidate?.close()
                            model = candidate
                            return candidate
                        }
                        FeatureStatus.DOWNLOADING -> {
                            Log.i(TAG, "Found DOWNLOADING Gemini Nano model. Using it.")
                            downloadableCandidate?.close()
                            model = candidate
                            return candidate
                        }
                        FeatureStatus.DOWNLOADABLE -> {
                            if (downloadableCandidate == null) {
                                downloadableCandidate = candidate
                            } else {
                                candidate.close()
                            }
                        }
                        else -> {
                            candidate.close()
                        }
                    }
                } catch (e: Throwable) {
                    Log.d(TAG, "Candidate checkStatus failed (stage=${config.modelConfig.releaseStage}, pref=${config.modelConfig.preference}): ${e.message}")
                    lastException = e
                    candidate.close()
                }
            }

            if (downloadableCandidate != null) {
                Log.i(TAG, "No AVAILABLE model found, but DOWNLOADABLE candidate exists. Selecting it.")
                model = downloadableCandidate
                return downloadableCandidate
            }

            throw lastException ?: GenAiException(
                "No compatible Gemini Nano model configuration found on this device.",
                null,
                GenAiException.ErrorCode.NOT_AVAILABLE
            )
        }
    }

    override suspend fun prepareInferenceEngine() {
        Log.i(TAG, "prepareInferenceEngine: checking/selecting model...")
        val m = getOrSelectModel()
        val status = try {
            m.checkStatus()
        } catch (e: Throwable) {
            Log.w(TAG, "checkStatus in prepareInferenceEngine failed: ${e.message}")
            return
        }
        Log.i(TAG, "AICore Gemini Nano feature status: $status")
        when (status) {
            FeatureStatus.AVAILABLE -> {
                Log.i(TAG, "Gemini Nano is AVAILABLE, warming up...")
                try {
                    m.warmup()
                    Log.i(TAG, "Gemini Nano warmup completed")
                } catch (e: Throwable) {
                    Log.w(TAG, "Warmup warning: ${e.message}")
                }
            }
            FeatureStatus.DOWNLOADABLE -> {
                Log.i(TAG, "Gemini Nano is DOWNLOADABLE, triggering download...")
                m.download().collect { dStatus ->
                    when (dStatus) {
                        is DownloadStatus.DownloadStarted -> {
                            Log.i(TAG, "Gemini Nano download started: ${dStatus.bytesToDownload} bytes")
                        }
                        is DownloadStatus.DownloadProgress -> {
                            Log.d(TAG, "Gemini Nano download progress: ${dStatus.totalBytesDownloaded} bytes")
                        }
                        is DownloadStatus.DownloadCompleted -> {
                            Log.i(TAG, "Gemini Nano download completed successfully")
                        }
                        is DownloadStatus.DownloadFailed -> {
                            Log.w(TAG, "Gemini Nano download failed: ${dStatus.e.message}", dStatus.e)
                        }
                    }
                }
            }
            FeatureStatus.DOWNLOADING -> {
                Log.i(TAG, "Gemini Nano is currently DOWNLOADING in background")
            }
            FeatureStatus.UNAVAILABLE -> {
                Log.w(TAG, "Gemini Nano is UNAVAILABLE on this device")
            }
            else -> {
                Log.i(TAG, "Gemini Nano status code: $status")
            }
        }
    }

    override fun generateContentStream(prompt: String): Flow<String> = flow {
        val m = getOrSelectModel()
        val status = try {
            m.checkStatus()
        } catch (e: Throwable) {
            Log.w(TAG, "checkStatus during stream: ${e.message}", e)
            FeatureStatus.UNAVAILABLE
        }
        if (status == FeatureStatus.DOWNLOADABLE) {
            Log.i(TAG, "Gemini Nano is DOWNLOADABLE, triggering download before generation...")
            m.download().collect { dStatus ->
                if (dStatus is DownloadStatus.DownloadFailed) {
                    throw dStatus.e
                }
            }
        }
        Log.i(TAG, "Calling m.generateContentStream(prompt), length=${prompt.length}")
        m.generateContentStream(prompt).collect { response ->
            val text = response.candidates.firstOrNull()?.text
            if (!text.isNullOrEmpty()) {
                emit(text)
            }
        }
    }

    override fun close() {
        try {
            model?.close()
        } catch (e: Throwable) {
            Log.w(TAG, "Error closing GenerativeModel: ${e.message}")
        }
        model = null
    }
}

interface GeminiNanoWatchRelay {
    fun respond(requestId: String, requestJson: String, callback: ProviderDeltaCallback): String
    fun cancel(requestId: String)
}

class GeminiNanoProvider(
    private val context: Context,
    private val searchNative: ProviderNativeCalls? = null,
    private val watchRelay: GeminiNanoWatchRelay? = null,
    private val isWatchDevice: () -> Boolean = {
        context.packageManager?.hasSystemFeature(PackageManager.FEATURE_WATCH) == true
    },
    private val engineFactory: (Context) -> GeminiNanoEngine = { DefaultGeminiNanoEngine(it) }
) : ProviderNativeCalls {

    companion object {
        private const val TAG = "GeminiNanoProvider"
        const val MODEL_NAME = "gemini-nano"
        private const val MAX_HISTORY_MESSAGES = 8
        private const val MAX_TURN_CHARS = 800
        private const val MAX_HISTORY_CHARS = 3200
        private const val SOURCE_FOOTER_MARKER = "\n\nSources:\n"

        internal fun stripSourceFooter(text: String): String {
            val idx = text.indexOf(SOURCE_FOOTER_MARKER)
            return if (idx >= 0) text.substring(0, idx).trimEnd() else text
        }

        internal fun formatPrompt(reqObj: JSONObject, evidenceContext: String?): String {
            val instructions = reqObj.optString("instructions", "").trim()
            val messages = reqObj.optJSONArray("messages")
            var promptText = reqObj.optString("prompt", "").trim()

            val rawTurns = mutableListOf<Pair<String, String>>()
            if (messages != null && messages.length() > 0) {
                for (i in 0 until messages.length()) {
                    val msg = messages.optJSONObject(i) ?: continue
                    val role = msg.optString("role", "").trim().lowercase()
                    val content = stripSourceFooter(msg.optString("content", "")).trim()
                    if (content.isNotEmpty() && (role == "user" || role == "assistant")) {
                        rawTurns.add(role to content)
                    }
                }
            }

            val historyTurns: List<Pair<String, String>> = if (promptText.isEmpty() && rawTurns.isNotEmpty()) {
                promptText = rawTurns.last().second
                rawTurns.dropLast(1)
            } else if (rawTurns.isNotEmpty() && rawTurns.last().first == "user" && rawTurns.last().second == promptText) {
                rawTurns.dropLast(1)
            } else {
                rawTurns
            }

            val boundedHistory = mutableListOf<String>()
            var totalHistoryChars = 0
            val recentTurns = if (historyTurns.size > MAX_HISTORY_MESSAGES) {
                historyTurns.takeLast(MAX_HISTORY_MESSAGES)
            } else {
                historyTurns
            }
            for ((role, content) in recentTurns.asReversed()) {
                val label = if (role == "assistant") "Assistant" else "User"
                val truncated = if (content.length > MAX_TURN_CHARS) {
                    content.take(MAX_TURN_CHARS).trimEnd() + "..."
                } else {
                    content
                }
                val line = "$label: $truncated"
                if (totalHistoryChars + line.length > MAX_HISTORY_CHARS && boundedHistory.isNotEmpty()) {
                    break
                }
                boundedHistory.add(0, line)
                totalHistoryChars += line.length
            }

            val sb = StringBuilder()
            if (instructions.isNotEmpty()) {
                sb.append("System Instructions:\n").append(instructions).append("\n\n")
            }
            if (instructions.contains("spoken turn", ignoreCase = true)) {
                sb.append("[System note: For this spoken response, give a concise answer in 1-2 short natural sentences.]\n\n")
            }
            if (boundedHistory.isNotEmpty()) {
                sb.append("Conversation History:\n")
                for (line in boundedHistory) {
                    sb.append(line).append("\n")
                }
                sb.append("\n")
            }
            if (!evidenceContext.isNullOrBlank()) {
                sb.append("[Web Search Context:\n")
                    .append(evidenceContext.trim())
                    .append("\n]\n\n")
            }
            if (boundedHistory.isNotEmpty()) {
                sb.append("User: ").append(promptText).append("\nAssistant:")
            } else {
                sb.append(promptText)
            }
            return sb.toString()
        }
    }

    private val lock = Any()
    private var engine: GeminiNanoEngine? = null
    private val activeJobs = ConcurrentHashMap<String, Job>()
    private val cancelledRequests = ConcurrentHashMap<String, Boolean>()
    @Volatile
    private var defaultWatchRelay: GeminiNanoWatchRelay? = null

    private fun resolveWatchRelay(): GeminiNanoWatchRelay {
        watchRelay?.let { return it }
        defaultWatchRelay?.let { return it }
        return synchronized(lock) {
            defaultWatchRelay ?: GmsGeminiNanoWatchRelay(context.applicationContext ?: context).also {
                defaultWatchRelay = it
            }
        }
    }

    private fun getOrCreateModel(): GeminiNanoEngine {
        synchronized(lock) {
            engine?.let { return it }
            val created = engineFactory(context)
            engine = created
            return created
        }
    }

    fun prewarm(async: Boolean = true): Thread? {
        if (isWatchDevice()) {
            Log.i(TAG, "Skipping local Gemini Nano prewarm on Wear OS watch (relayed to phone)")
            return null
        }
        val task = Runnable {
            try {
                Log.i(TAG, "Prewarming Gemini Nano in background thread...")
                val eng = getOrCreateModel()
                runBlocking(Dispatchers.IO) {
                    eng.prepareInferenceEngine()
                }
                Log.i(TAG, "Gemini Nano prewarm completed successfully")
            } catch (e: Throwable) {
                Log.w(TAG, "Gemini Nano prewarm failed: ${e.message}", e)
            }
        }
        return if (async) {
            Thread(task, "gemini-nano-prewarm").apply { start() }
        } else {
            task.run()
            null
        }
    }

    override fun respond(
        requestId: String,
        authPath: String,
        requestJson: String,
        callback: ProviderDeltaCallback
    ): String {
        if (isCancelled(requestId)) {
            cancelledRequests.remove(requestId)
            return cancelledResult()
        }
        if (isWatchDevice()) {
            return try {
                resolveWatchRelay().respond(requestId, requestJson, callback)
            } finally {
                cancelledRequests.remove(requestId)
            }
        }
        try {
            val reqObj = try { JSONObject(requestJson) } catch (_: Exception) { JSONObject() }
            if (!reqObj.has("requestId") && requestId.isNotBlank()) {
                reqObj.put("requestId", requestId)
            }
            val webEnabled = reqObj.optBoolean("webEnabled", reqObj.optBoolean("web_enabled", true))
            val searchMode = reqObj.optString("searchMode", reqObj.optString("web_search_mode", "quick"))

            Log.i(TAG, "GeminiNanoProvider.respond started, requestId=$requestId, searchMode=$searchMode, webEnabled=$webEnabled")
            callback.onReady()

            var evidenceContext: String? = null
            var sourceFooter = ""
            if (webEnabled && searchMode !in setOf("off", "disabled", "none")) {
                if (isCancelled(requestId)) return cancelledResult()
                val prepJson = try {
                    (searchNative ?: AssistantProviderNative).prepareSearch(reqObj.toString(), callback)
                } catch (e: Throwable) {
                    Log.w(TAG, "nativePrepareSearch failed, continuing without search context: ${e.message}")
                    null
                }
                if (isCancelled(requestId)) return cancelledResult()
                if (!prepJson.isNullOrBlank()) {
                    try {
                        val prepObj = JSONObject(prepJson)
                        if (!prepObj.isNull("evidenceContext")) {
                            val ctx = prepObj.optString("evidenceContext", "").trim()
                            if (ctx.isNotEmpty()) {
                                evidenceContext = ctx
                            }
                        }
                        sourceFooter = prepObj.optString("sourceFooter", "")
                    } catch (e: Throwable) {
                        Log.w(TAG, "Failed to parse search preparation JSON: ${e.message}")
                    }
                }
            }

            val finalPrompt = formatPrompt(reqObj, evidenceContext)

            if (isCancelled(requestId)) return cancelledResult()

            val timing = AgyRequestTiming(SystemClock::elapsedRealtime)
            callback.onRequestDispatched()

            val accumulated = StringBuilder()
            var errorResult: String? = null

            val eng = try {
                getOrCreateModel()
            } catch (e: Throwable) {
                Log.e(TAG, "Failed to initialize Gemini Nano engine: ${e.message}", e)
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
                                    Log.i(TAG, "Gemini Nano first text in ${elapsed}ms")
                                    callback.onFirstText(elapsed)
                                }
                                accumulated.append(delta)
                                callback.onDelta(delta)
                            }
                        }
                    } catch (_: CancellationException) {
                        Log.i(TAG, "Gemini Nano request $requestId was cancelled")
                        errorResult = cancelledResult()
                    } catch (e: GenAiException) {
                        Log.e(TAG, "Gemini Nano GenAiException (code ${e.errorCode}): ${e.message}", e)
                        val msg = (e.message ?: "") + " " + (e.cause?.message ?: "")
                        val code = when (e.errorCode) {
                            GenAiException.ErrorCode.NOT_AVAILABLE -> "aicore_not_available"
                            GenAiException.ErrorCode.NEEDS_SYSTEM_UPDATE, GenAiException.ErrorCode.AICORE_INCOMPATIBLE -> "aicore_needs_update"
                            GenAiException.ErrorCode.BUSY -> "aicore_busy"
                            else -> when {
                                msg.contains("download", ignoreCase = true) -> "aicore_downloading"
                                msg.contains("not available", ignoreCase = true) || msg.contains("not found", ignoreCase = true) -> "aicore_not_available"
                                else -> "aicore_unavailable"
                            }
                        }
                        errorResult = JSONObject()
                            .put("status", "error")
                            .put("code", code)
                            .put("message", e.message ?: "Gemini Nano generation failure")
                            .toString()
                    } catch (e: Throwable) {
                        Log.e(TAG, "Gemini Nano generation unexpected error: ${e.message}", e)
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
                Log.e(TAG, "Gemini Nano execution error ($requestId): ${e.message}", e)
                return JSONObject()
                    .put("status", "error")
                    .put("code", "internal_error")
                    .put("message", e.message ?: "Execution error")
                    .toString()
            }

            if (errorResult != null) return errorResult!!
            if (isCancelled(requestId)) return cancelledResult()

            val answerText = accumulated.toString()
            if (sourceFooter.isNotEmpty() && answerText.isNotEmpty()) {
                accumulated.append(sourceFooter)
                callback.onDelta(sourceFooter)
            }
            val fullText = accumulated.toString()
            val completedMs = timing.completedMs()
            val responseJson = JSONObject()
                .put("status", "complete")
                .put("contentType", "output_text")
                .put("text", fullText)
                .put("returnedModel", MODEL_NAME)
                .put("firstTextMs", timing.firstTextMs)
                .put("completedMs", completedMs)
            if (sourceFooter.isNotEmpty() && answerText.isNotBlank()) {
                responseJson.put("speechText", answerText)
            }
            return responseJson.toString()
        } finally {
            cancelledRequests.remove(requestId)
            activeJobs.remove(requestId)
        }
    }

    override fun cancel(requestId: String) {
        cancelledRequests[requestId] = true
        activeJobs.remove(requestId)?.cancel()
        if (isWatchDevice()) {
            try {
                resolveWatchRelay().cancel(requestId)
            } catch (_: Throwable) {}
        } else {
            try {
                (searchNative ?: AssistantProviderNative).cancel(requestId)
            } catch (_: Throwable) {}
        }
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
}

object GeminiNanoRelayWire {
    const val PREFIX = "/voxgolem/assistant/v1/nano-relay/"
    const val PATH_REQUEST = "/voxgolem/assistant/v1/nano-relay/request"
    const val PATH_CANCEL = "/voxgolem/assistant/v1/nano-relay/cancel"
    const val PATH_EVENT = "/voxgolem/assistant/v1/nano-relay/event"
}

class GmsGeminiNanoWatchRelay(
    private val context: Context,
    private val pollTimeoutMs: Long = 250L,
    private val maxDurationMs: Long = 90_000L
) : GeminiNanoWatchRelay {
    companion object {
        private const val TAG = "GeminiNanoWatchRelay"
    }

    internal class RelaySession(val requestId: String) {
        val events = LinkedBlockingQueue<JSONObject>()
        val cancelled = AtomicBoolean(false)
        val seenSeq = ConcurrentHashMap.newKeySet<Long>()
        @Volatile var nodeId: String? = null

        fun offerEvent(event: JSONObject) {
            val seq = event.optLong("seq", -1L)
            if (seq >= 0L && !seenSeq.add(seq)) {
                return
            }
            events.offer(event)
        }

        fun cancel() {
            if (cancelled.compareAndSet(false, true)) {
                events.offer(JSONObject().put("type", "cancelled"))
            }
        }
    }

    override fun respond(
        requestId: String,
        requestJson: String,
        callback: ProviderDeltaCallback
    ): String {
        val session = GeminiNanoRelayBridge.registerWatchSession(requestId)
        val messageClient = try {
            Wearable.getMessageClient(context)
        } catch (e: Throwable) {
            GeminiNanoRelayBridge.unregisterWatchSession(requestId)
            return JSONObject()
                .put("status", "error")
                .put("code", "aicore_unavailable")
                .put("message", "Wearable MessageClient unavailable: ${e.message}")
                .toString()
        }

        val listener = MessageClient.OnMessageReceivedListener { event ->
            if (event.path == GeminiNanoRelayWire.PATH_EVENT) {
                GeminiNanoRelayBridge.deliverWatchEvent(event.data)
            }
        }

        try {
            try {
                Tasks.await(messageClient.addListener(listener), 3, TimeUnit.SECONDS)
            } catch (_: Throwable) {}

            val nodes = try {
                Tasks.await(Wearable.getNodeClient(context).connectedNodes, 5, TimeUnit.SECONDS)
            } catch (e: Throwable) {
                Log.w(TAG, "Failed to query connected nodes for Gemini Nano relay: ${e.message}")
                emptyList()
            }

            val targetNode = nodes.firstOrNull { it.isNearby } ?: nodes.firstOrNull()
            if (targetNode == null) {
                return JSONObject()
                    .put("status", "error")
                    .put("code", "aicore_unavailable")
                    .put("message", "No paired phone connected to run Gemini Nano")
                    .toString()
            }
            session.nodeId = targetNode.id

            if (session.cancelled.get()) {
                return JSONObject().put("status", "cancelled").toString()
            }

            val payload = JSONObject()
                .put("requestId", requestId)
                .put("requestJson", requestJson)
                .toString()
                .toByteArray(StandardCharsets.UTF_8)

            try {
                Tasks.await(
                    messageClient.sendMessage(targetNode.id, GeminiNanoRelayWire.PATH_REQUEST, payload),
                    5,
                    TimeUnit.SECONDS
                )
            } catch (e: Throwable) {
                return JSONObject()
                    .put("status", "error")
                    .put("code", "transport")
                    .put("message", "Failed to relay Gemini Nano request to phone: ${e.message}")
                    .toString()
            }

            val deadline = SystemClock.elapsedRealtime() + maxDurationMs
            var readyDelivered = false
            while (SystemClock.elapsedRealtime() < deadline) {
                if (session.cancelled.get()) {
                    sendCancelToNode(messageClient, targetNode.id, requestId)
                    return JSONObject().put("status", "cancelled").toString()
                }
                val evt = session.events.poll(pollTimeoutMs, TimeUnit.MILLISECONDS) ?: continue
                when (evt.optString("type")) {
                    "cancelled" -> {
                        sendCancelToNode(messageClient, targetNode.id, requestId)
                        return JSONObject().put("status", "cancelled").toString()
                    }
                    "ready" -> {
                        if (!readyDelivered) {
                            readyDelivered = true
                            callback.onReady()
                        }
                    }
                    "dispatched" -> {
                        if (!readyDelivered) {
                            readyDelivered = true
                            callback.onReady()
                        }
                        callback.onRequestDispatched()
                    }
                    "status" -> {
                        val status = evt.optString("status", "")
                        if (status.isNotEmpty()) callback.onStatus(status)
                    }
                    "first_text" -> {
                        callback.onFirstText(evt.optLong("milliseconds", 0L))
                    }
                    "delta" -> {
                        val text = evt.optString("text", "")
                        if (text.isNotEmpty()) callback.onDelta(text)
                    }
                    "outcome" -> {
                        return evt.optString(
                            "outcomeJson",
                            JSONObject().put("status", "error").put("code", "invalid_response").toString()
                        )
                    }
                }
            }

            sendCancelToNode(messageClient, targetNode.id, requestId)
            return JSONObject()
                .put("status", "error")
                .put("code", "timeout")
                .put("message", "Timed out waiting for Gemini Nano response from phone")
                .toString()
        } finally {
            GeminiNanoRelayBridge.unregisterWatchSession(requestId)
            try {
                messageClient.removeListener(listener)
            } catch (_: Throwable) {}
        }
    }

    override fun cancel(requestId: String) {
        val session = GeminiNanoRelayBridge.cancelWatchSession(requestId)
        val nodeId = session?.nodeId
        Thread({
            try {
                val client = Wearable.getMessageClient(context)
                if (nodeId != null) {
                    sendCancelToNode(client, nodeId, requestId)
                } else {
                    val nodes = Tasks.await(Wearable.getNodeClient(context).connectedNodes, 2, TimeUnit.SECONDS)
                    for (node in nodes) {
                        sendCancelToNode(client, node.id, requestId)
                    }
                }
            } catch (_: Throwable) {}
        }, "nano-relay-cancel").start()
    }

    private fun sendCancelToNode(client: MessageClient, nodeId: String, requestId: String) {
        try {
            val bytes = JSONObject().put("requestId", requestId).toString().toByteArray(StandardCharsets.UTF_8)
            client.sendMessage(nodeId, GeminiNanoRelayWire.PATH_CANCEL, bytes)
        } catch (_: Throwable) {}
    }
}

object GeminiNanoRelayBridge {
    private const val TAG = "GeminiNanoRelayBridge"
    private val watchSessions = ConcurrentHashMap<String, GmsGeminiNanoWatchRelay.RelaySession>()
    @Volatile
    private var phoneProvider: GeminiNanoProvider? = null
    internal var phoneProviderFactory: (Context) -> ProviderNativeCalls = { ctx ->
        GeminiNanoProvider(ctx.applicationContext ?: ctx, isWatchDevice = { false })
    }
    internal var phoneEventSender: ((Context, String, ByteArray) -> Unit)? = null

    internal fun registerWatchSession(requestId: String): GmsGeminiNanoWatchRelay.RelaySession {
        val session = GmsGeminiNanoWatchRelay.RelaySession(requestId)
        watchSessions[requestId] = session
        return session
    }

    internal fun unregisterWatchSession(requestId: String) {
        watchSessions.remove(requestId)
    }

    internal fun cancelWatchSession(requestId: String): GmsGeminiNanoWatchRelay.RelaySession? {
        val session = watchSessions[requestId]
        session?.cancel()
        return session
    }

    internal fun deliverWatchEvent(data: ByteArray) {
        try {
            val json = JSONObject(String(data, StandardCharsets.UTF_8))
            val requestId = json.optString("requestId", "")
            if (requestId.isNotEmpty()) {
                watchSessions[requestId]?.offerEvent(json)
            }
        } catch (e: Throwable) {
            Log.w(TAG, "Failed to parse watch relay event: ${e.message}")
        }
    }

    private fun getOrCreatePhoneProvider(context: Context): ProviderNativeCalls {
        phoneProvider?.let { return it }
        synchronized(this) {
            phoneProvider?.let { return it }
            val created = phoneProviderFactory(context)
            if (created is GeminiNanoProvider) {
                phoneProvider = created
            }
            return created
        }
    }

    fun onMessageReceived(context: Context, messageEvent: MessageEvent) {
        onMessageReceived(context, messageEvent.sourceNodeId, messageEvent.path, messageEvent.data)
    }

    fun onMessageReceived(context: Context, sourceNodeId: String, path: String, data: ByteArray) {
        when (path) {
            GeminiNanoRelayWire.PATH_EVENT -> {
                deliverWatchEvent(data)
            }
            GeminiNanoRelayWire.PATH_CANCEL -> {
                try {
                    val json = JSONObject(String(data, StandardCharsets.UTF_8))
                    val requestId = json.optString("requestId", "")
                    if (requestId.isNotEmpty()) {
                        getOrCreatePhoneProvider(context).cancel(requestId)
                    }
                } catch (e: Throwable) {
                    Log.w(TAG, "Failed to handle relay cancel: ${e.message}")
                }
            }
            GeminiNanoRelayWire.PATH_REQUEST -> {
                Thread({
                    handlePhoneRelayRequest(context, sourceNodeId, data)
                }, "nano-phone-relay").start()
            }
        }
    }

    internal fun handlePhoneRelayRequest(context: Context, sourceNodeId: String, data: ByteArray) {
        val payload = try {
            JSONObject(String(data, StandardCharsets.UTF_8))
        } catch (e: Throwable) {
            Log.w(TAG, "Invalid relay request JSON: ${e.message}")
            return
        }
        val requestId = payload.optString("requestId", "")
        val requestJson = payload.optString("requestJson", "")
        if (requestId.isEmpty() || requestJson.isEmpty()) return

        var seq = 0L
        fun sendEvent(event: JSONObject) {
            event.put("requestId", requestId)
            event.put("seq", seq++)
            val bytes = event.toString().toByteArray(StandardCharsets.UTF_8)
            val customSender = phoneEventSender
            if (customSender != null) {
                customSender(context, sourceNodeId, bytes)
                return
            }
            try {
                Tasks.await(
                    Wearable.getMessageClient(context).sendMessage(
                        sourceNodeId,
                        GeminiNanoRelayWire.PATH_EVENT,
                        bytes
                    ),
                    5,
                    TimeUnit.SECONDS
                )
            } catch (e: Throwable) {
                Log.w(TAG, "Failed to send relay event to watch $sourceNodeId: ${e.message}")
            }
        }

        val provider = getOrCreatePhoneProvider(context)
        val outcomeJson = try {
            provider.respond(
                requestId = requestId,
                authPath = "",
                requestJson = requestJson,
                callback = object : ProviderDeltaCallback {
                    override fun onReady() {
                        sendEvent(JSONObject().put("type", "ready"))
                    }
                    override fun onRequestDispatched() {
                        sendEvent(JSONObject().put("type", "dispatched"))
                    }
                    override fun onStatus(status: String) {
                        sendEvent(JSONObject().put("type", "status").put("status", status))
                    }
                    override fun onFirstText(milliseconds: Long) {
                        sendEvent(JSONObject().put("type", "first_text").put("milliseconds", milliseconds))
                    }
                    override fun onDelta(text: String) {
                        sendEvent(JSONObject().put("type", "delta").put("text", text))
                    }
                }
            )
        } catch (e: Throwable) {
            JSONObject()
                .put("status", "error")
                .put("code", "internal_error")
                .put("message", e.message ?: "Phone relay failure")
                .toString()
        }
        sendEvent(JSONObject().put("type", "outcome").put("outcomeJson", outcomeJson))
    }
}

