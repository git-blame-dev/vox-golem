package dev.gitblame.voxgolem.assistant.runtime

import android.content.Context
import android.os.SystemClock
import android.os.Build
import android.util.Log
import org.json.JSONObject
import java.io.File
import org.json.JSONArray
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Future
import java.util.concurrent.Executors
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.ConcurrentHashMap

class AgyProviderRouter(
    private val context: Context,
    private val defaultNative: ProviderNativeCalls = AssistantProviderNative,
    private val agyNative: ProviderNativeCalls = AgyProviderNative(context),
    private val geminiNanoNative: ProviderNativeCalls = GeminiNanoProvider(context, searchNative = defaultNative)
) : ProviderNativeCalls {
    private class Route(val provider: ProviderNativeCalls)
    private val requests = ConcurrentHashMap<String, Route>()
    fun prewarm(model: String) {
        if (model == GeminiNanoProvider.MODEL_NAME) (geminiNanoNative as? GeminiNanoProvider)?.prewarm()
        val eligible = isAgyModel(model)
        // Every accepted selection invalidates older work, including selections routed elsewhere.
        (agyNative as? AgyProviderNative)?.prewarm(model, eligible)
    }

    override fun prewarm(model: String, authPath: String) {
        prewarm(model)
        if (!isAgyModel(model)) defaultNative.prewarm(model, authPath)
    }

    override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String {
        val model = try {
            JSONObject(requestJson).optString("model")
        } catch (_: Exception) { "" }
        val route = Route(providerFor(model))
        if (requests.putIfAbsent(requestId, route) != null) {
            return JSONObject().put("status", "error").put("code", "busy").toString()
        }
        // Keep this exact route until physical retirement, including exceptional response returns.
        return route.provider.respond(requestId, authPath, requestJson, callback)
    }

    override fun cancel(requestId: String) {
        requests[requestId]?.provider?.cancel(requestId)
    }

    override fun prepareSearch(requestJson: String, callback: ProviderDeltaCallback): String =
        defaultNative.prepareSearch(requestJson, callback)

    override fun whenRetired(requestId: String, retired: () -> Unit) {
        val route = requests[requestId]
        if (route == null) { retired(); return }
        route.provider.whenRetired(requestId) {
            requests.remove(requestId, route)
            retired()
        }
    }
    private fun isAgyModel(model: String) = model != GeminiNanoProvider.MODEL_NAME && (model.startsWith("agy-") || model.startsWith("claude-") ||
        model.startsWith("gemini-") || model.startsWith("opus") || model.startsWith("sonnet") || model.startsWith("3.8-"))
    private fun providerFor(model: String) = when {
        model == GeminiNanoProvider.MODEL_NAME -> geminiNanoNative
        isAgyModel(model) -> agyNative
        else -> defaultNative
    }
}

class AgyProviderNative internal constructor(private val context: Context,
    private val executableSource: (() -> File?)? = null,
    private val processFactory: ((File, String, String) -> Process)? = null,
    private val requestTimeoutMs: Long = 180_000,
) : ProviderNativeCalls, AutoCloseable {
    constructor(context: Context) : this(context, null, null)
    companion object {
        private const val TAG = "AgyProvider"
    }

    private val lock = Any()
    private var process: Process? = null
    private var activeModel: String? = null
    private var processUsed = false
    private var activeRequestId: String? = null
    private var ioRetired = true
    private val io = ThreadPoolExecutor(1, 1, 30, TimeUnit.SECONDS, ArrayBlockingQueue(2),
        { task -> Thread(task, "agy-request-io").apply { isDaemon = true } }, ThreadPoolExecutor.AbortPolicy())
        .apply { allowCoreThreadTimeOut(true) }
    private val retirementIo = ThreadPoolExecutor(1, 1, 30, TimeUnit.SECONDS, ArrayBlockingQueue(1),
        { task -> Thread(task, "agy-retirement").apply { isDaemon = true } }, ThreadPoolExecutor.AbortPolicy())
        .apply { allowCoreThreadTimeOut(true) }
    private var retirement: Pair<Process, Future<Boolean>>? = null
    private val retirementMonitor = Executors.newSingleThreadScheduledExecutor { task ->
        Thread(task, "agy-retirement-observer").apply { isDaemon = true }
    }
    private var retirementObserver: ScheduledFuture<*>? = null
    private var waitingRetirement: Pair<String, () -> Unit>? = null
    private var retirementOwner: String? = null
    private val cancelled = AtomicBoolean(false)
    private var prewarmGeneration = 0L

    private fun advancePrewarmSelection(): Long = synchronized(lock) { ++prewarmGeneration }

    private fun preparePrivateEnvironment(): Boolean {
        val provisioned = AgyPrivateEnvironment.prepare(context)
        AssistantRuntime.publishOptionalProviderReadiness(context)
        return provisioned
    }

    private fun getExecutable(): File? {
        if (Build.VERSION.SDK_INT < 26) return null
        executableSource?.let { return it() }
        val lib = File(context.applicationInfo.nativeLibraryDir, "libagy.so")
        if (lib.exists() && lib.canExecute()) return lib
        val filesLib = File(context.filesDir, "libagy.so")
        if (filesLib.exists() && filesLib.canExecute()) return filesLib
        return null
    }

    fun resolveTargetModel(requestedModel: String): String = when (requestedModel) {
        "agy-opus-high", "claude-opus-5-5-high", "opus-5.5", "opus5.5" -> "claude-opus-5-5-high"
        "agy-sonnet-high", "claude-sonnet-5-5-high", "sonnet-5.5", "sonnet5.5" -> "claude-sonnet-5-5-high"
        "agy-flash-low", "gemini-3.8-flash-low" -> "gemini-3.8-flash-low"
        "agy-flash-high", "gemini-3.8-flash-high", "3.8-flash-high" -> "gemini-3.8-flash-high"
        else -> if (requestedModel.startsWith("claude-") || requestedModel.startsWith("gemini-")) requestedModel else "gemini-3.8-flash-high"
    }

    /** Called only by the IO actor. Startup and retirement never hold the cancellation monitor. */
    private fun startProcess(executable: File, targetAgyModel: String, effort: String, generation: Long? = null): Boolean {
        val previous = synchronized(lock) {
            if (generation != null && generation != prewarmGeneration) return false
            if (!processUsed && activeModel == targetAgyModel && agyAlive(process)) return true
            process
        }
        previous?.let { retire(it) }
        if (synchronized(lock) { agyAlive(process) }) return false
        try {
            val pb = ProcessBuilder(
                executable.absolutePath,
                "--input-format", "stream-json",
                "--output-format", "stream-json",
                "--dangerously-skip-permissions",
                "--disable-slash-commands",
                "--effort", effort,
                "--model", targetAgyModel
            )
            pb.directory(File(context.filesDir, "ws"))
            val env = pb.environment()
            env["HOME"] = File(context.filesDir, "home").absolutePath
            env["TMPDIR"] = context.cacheDir.absolutePath
            env["GODEBUG"] = "netdns=cgo"
            env["PATH"] = "/product/bin:/apex/com.android.runtime/bin:/apex/com.android.art/bin:/system/bin:/system/xbin"
            env["ANDROID_ROOT"] = "/system"
            env["ANDROID_DATA"] = "/data"

            val p = processFactory?.invoke(executable, targetAgyModel, effort) ?: pb.start()
            val admitted = synchronized(lock) {
                process = p
                activeModel = targetAgyModel
                val current = if (generation == null) !cancelled.get() else generation == prewarmGeneration
                processUsed = !current
                current
            }
            if (!admitted) { retire(p); return false }

            Thread({
                try {
                    p.errorStream.use { stream ->
                        // Fixed memory drain; never log engine payloads or credentials.
                        val buffer = ByteArray(4096)
                        while (stream.read(buffer) >= 0) { }
                    }
                } catch (_: Exception) {}
            }, "agy-stderr").apply { isDaemon = true }.start()
            return true
        } catch (_: Exception) {
            Log.e(TAG, "Failed to start Agy process")
            return false
        }
    }

    private fun acquirePrewarmProcess(executable: File, targetModel: String, effort: String, generation: Long): Boolean {
        synchronized(lock) {
            if (generation != prewarmGeneration) return false
            // Respond owns model/session acquisition until its complete unwind, including cancellation.
            if (activeRequestId != null || !ioRetired) return false
        }
        if (!preparePrivateEnvironment()) return false
        synchronized(lock) {
            if (generation != prewarmGeneration || activeRequestId != null || !ioRetired) return false
            if (!processUsed && activeModel == targetModel && agyAlive(process)) return true
        }
        return startProcess(executable, targetModel, effort, generation)
    }

    fun prewarm(requestedModel: String, eligible: Boolean) {
        val generation = advancePrewarmSelection()
        if (!eligible) return
        try {
            io.execute {
                if (synchronized(lock) { generation != prewarmGeneration || activeRequestId != null }) return@execute
                val executable = getExecutable() ?: return@execute
                val targetAgyModel = resolveTargetModel(requestedModel)
                val effort = if (targetAgyModel.endsWith("-low")) "low" else "high"
                if (acquirePrewarmProcess(executable, targetAgyModel, effort, generation)) {
                    Log.i(TAG, "Prewarm acquired local engine process; readiness not verified")
                } else Log.i(TAG, "Prewarm was not acquired; readiness not verified")
            }
        } catch (_: RuntimeException) { Log.w(TAG, "Prewarm was not admitted") }
    }

    override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String {
        synchronized(lock) {
            if (activeRequestId != null || waitingRetirement != null || !ioRetired || (processUsed && agyAlive(process))) {
                return JSONObject().put("status", "error").put("code", "busy")
                    .put("message", "An Agy request is already active").toString()
            }
            activeRequestId = requestId
            cancelled.set(false)
        }

        var retired = false
        val outcome = try {
            // Registration, not process/model readiness. Never call the worker under our monitor:
            // it may immediately forward a cancellation latched before this handshake.
            callback.onReady()
            if (cancelled.get()) JSONObject().put("status", "cancelled").toString()
            else respondOwned(requestJson, callback)
        } catch (_: Exception) {
            failure("internal_error", "Agy request failed")
        } finally {
            // Each admitted input permanently mutates implicit CLI history. Never reuse it.
            val owned = synchronized(lock) { process?.takeIf { processUsed } }
            owned?.let { retire(it) }
            synchronized(lock) {
                retired = ioRetired && !agyAlive(process?.takeIf { processUsed })
                retirementOwner = if (retired) null else requestId
                if (activeRequestId == requestId) activeRequestId = null
            }
        }
        if (!retired) {
            Log.w(TAG, "Agy request retirement is unconfirmed; admission remains blocked")
            return failure("retirement_failed", "Agy process retirement is unconfirmed; route remains owned")
        }
        return outcome
    }

    private fun respondOwned(requestJson: String, callback: ProviderDeltaCallback): String {
        val executable = getExecutable()
        if (executable == null) {
            return JSONObject().put("status", "error").put("code", "unsupported_model")
                .put("message", "Agy requires Android 26+ and an installed native executable for this architecture").toString()
        }

        val reqObj = JSONObject(requestJson)
        val requestedModel = reqObj.optString("model", "agy-flash-high")
        val targetAgyModel = resolveTargetModel(requestedModel)
        val effort = if (targetAgyModel.endsWith("-low")) "low" else "high"
        val webEnabled = reqObj.optBoolean("webEnabled", true)
        // Authored CLI contract has no verified tool-denial protocol. Prompt text is not enforcement.
        if (!webEnabled) return failure("unsupported_search_mode", "Agy cannot enforce disabled web tools")
        val serializedRequest = canonicalInput(reqObj)
        synchronized(lock) {
            if (cancelled.get()) return JSONObject().put("status", "cancelled").toString()
            ioRetired = false
        }
        val exited = CountDownLatch(1)
        // Only stale prewarm tasks can be queued: active request admission is exclusive.
        io.queue.clear()
        val future = try { io.submit<String> {
            try {
                if (cancelled.get()) return@submit JSONObject().put("status", "cancelled").toString()
                if (!preparePrivateEnvironment()) return@submit failure("auth_unavailable", "Agy requires externally provisioned app-private access and installation identity")
                if (cancelled.get()) return@submit JSONObject().put("status", "cancelled").toString()
                if (!startProcess(executable, targetAgyModel, effort)) return@submit failure("internal_error", "Failed to start Agy process")
                val owned = synchronized(lock) { processUsed = true; requireNotNull(process) }
                exchange(owned, serializedRequest, targetAgyModel, callback)
            }
            finally { synchronized(lock) { ioRetired = true }; exited.countDown() }
        } } catch (error: RuntimeException) { synchronized(lock) { ioRetired = true }; throw error }
        try {
            val deadline = System.nanoTime() + TimeUnit.MILLISECONDS.toNanos(requestTimeoutMs)
            while (!cancelled.get()) {
                val remaining = deadline - System.nanoTime()
                if (remaining <= 0) return failure("timeout", "Agy request timed out")
                try { return future.get(minOf(remaining, TimeUnit.MILLISECONDS.toNanos(50)), TimeUnit.NANOSECONDS) }
                catch (_: TimeoutException) { }
            }
            return JSONObject().put("status", "cancelled").toString()
        } finally {
            cancelled.set(true)
            synchronized(lock) { process }?.let { retire(it) }
            // A failed/uncooperative reader remains owned and blocks another admission.
            exited.await(1000, TimeUnit.MILLISECONDS)
        }
    }

    private fun exchange(owned: Process, serializedRequest: String, targetAgyModel: String, callback: ProviderDeltaCallback): String {
        val accumulated = StringBuilder()
        val reader = AgyLines(owned.inputStream)
        try {
            if (cancelled.get()) return JSONObject().put("status", "cancelled").toString()
            val timing = AgyRequestTiming(SystemClock::elapsedRealtime)
            callback.onRequestDispatched()
            writeAgyInput(owned, serializedRequest)
            while (true) {
                if (cancelled.get()) {
                    return JSONObject().put("status", "cancelled").toString()
                }
                val line = reader.next() ?: break
                if (cancelled.get()) return JSONObject().put("status", "cancelled").toString()
                val trimmed = line.trim()
                if (trimmed.isEmpty()) continue

                val obj = try { JSONObject(trimmed) } catch (_: Exception) { continue }
                val event = obj.optString("event")

                if (event == "step_update") {
                    val stepUpdate = obj.optJSONObject("step_update") ?: continue
                    val stepType = stepUpdate.optString("step_type")
                    val delta = stepUpdate.optString("text_delta", "")
                    val state = stepUpdate.optString("state", "")

                    if (stepType == "tool" && state == "ACTIVE") {
                        callback.onStatus("Searching")
                    } else if (stepType == "thought" || stepType == "reasoning") {
                        // Internal model reasoning/thought tokens; do not treat as user response text
                    } else if (delta.isNotEmpty() && (stepType.isEmpty() || stepType == "agent_response")) {
                        timing.onForwardedText(delta)?.let { elapsed ->
                            callback.onFirstText(elapsed)
                        }
                        check(accumulated.length + delta.length <= 256 * 1024) { "Agy response budget exceeded" }
                        accumulated.append(delta)
                        callback.onDelta(delta)
                    }
                } else if (event == "result") {
                    val res = obj.optJSONObject("result")
                    val status = res?.optString("status")
                    if (status == "SUCCESS") {
                        val text = accumulated.toString()
                        if (text.isBlank()) return failure("provider_failure", "Agy terminal success contained no answer")
                        val completedMs = timing.completedMs()
                        return JSONObject()
                            .put("status", "complete")
                            .put("contentType", "output_text")
                            .put("text", text)
                            .put("returnedModel", targetAgyModel)
                            .put("firstTextMs", timing.firstTextMs)
                            .put("completedMs", completedMs)
                            .toString()
                    } else {
                        return JSONObject()
                            .put("status", "error")
                            .put("code", "provider_failure")
                            .put("message", "Agy execution failed")
                            .toString()
                    }
                }
            }

            return if (cancelled.get()) JSONObject().put("status", "cancelled").toString()
            else failure("provider_failure", "Agy output closed before terminal success")
        } catch (_: Exception) {
            return if (cancelled.get()) JSONObject().put("status", "cancelled").toString()
            else failure("provider_failure", "Agy stream failed")
        }
    }

    override fun cancel(requestId: String) {
        val owned = synchronized(lock) {
            if (activeRequestId == requestId || retirementOwner == requestId) {
                cancelled.set(true)
                // Registration may precede acquisition: an unused prewarm is not request IO.
                process?.takeIf { processUsed }
            } else null
        }
        // Never wait for or perform blocked request IO under the cancellation monitor.
        owned?.let { beginRetirement(it) }
    }
    private fun beginRetirement(owned: Process): Future<Boolean> = synchronized(lock) {
        retirement?.takeIf { it.first === owned && (!it.second.isDone || runCatching { it.second.get() }.getOrDefault(false)) }?.second ?: retirementIo.submit<Boolean> {
            runCatching { retireAgyProcess(owned) }.getOrDefault(false)
        }.also { retirement = owned to it }
    }
    private fun retire(owned: Process) {
        val retired = runCatching { beginRetirement(owned).get(2500, TimeUnit.MILLISECONDS) }.getOrDefault(false)
        synchronized(lock) {
            if (retired && process === owned) {
                process = null; activeModel = null; processUsed = false; retirement = null
            }
        }
    }
    private fun failure(code: String, message: String) = JSONObject().put("status", "error").put("code", code).put("message", message).toString()

    override fun whenRetired(requestId: String, retired: () -> Unit) {
        val ready = synchronized(lock) {
            if (retirementOwner != requestId) true
            else if (ioRetired && !agyAlive(process?.takeIf { processUsed })) { retirementOwner = null; true } else {
                check(waitingRetirement == null) { "Agy retirement already has an owner" }
                waitingRetirement = requestId to retired
                retirementObserver = retirementMonitor.scheduleWithFixedDelay({ observeRetirement() }, 0, 100, TimeUnit.MILLISECONDS)
                false
            }
        }
        if (ready) retired()
    }
    private fun observeRetirement() {
        val callback = synchronized(lock) {
            if (!ioRetired || agyAlive(process?.takeIf { processUsed })) return
            waitingRetirement?.second.also {
                waitingRetirement = null
                retirementOwner = null
                retirementObserver?.cancel(false); retirementObserver = null
            }
        }
        if (callback != null) {
            Log.i(TAG, "Agy request retirement confirmed")
            callback.invoke()
        }
    }

    override fun close() {
        cancelled.set(true)
        synchronized(lock) { process }?.let { retire(it) }
        io.shutdownNow()
        io.awaitTermination(1000, TimeUnit.MILLISECONDS)
        retirementIo.shutdownNow()
        retirementIo.awaitTermination(1000, TimeUnit.MILLISECONDS)
        observeRetirement()
        retirementMonitor.shutdownNow()
    }

    internal fun canonicalInput(request: JSONObject): String {
        val messages = request.optJSONArray("messages") ?: JSONArray().put(JSONObject().put("role", "user").put("content", request.optString("prompt")))
        require(messages.length() in 1..41)
        val canonical = JSONArray()
        for (index in 0 until messages.length()) {
            val message = messages.getJSONObject(index)
            val role = message.getString("role")
            require(role in setOf("system", "developer", "user", "assistant"))
            val admitted = JSONObject().put("role", role).put("content", message.getString("content"))
            if (message.has("contentType")) {
                val kind = message.getString("contentType")
                require(kind in setOf("output_text", "refusal"))
                admitted.put("contentType", kind)
            }
            canonical.put(admitted)
        }
        val payload = JSONObject().put("instructions", request.optString("instructions"))
            .put("searchMode", request.optString("searchMode", "quick")).put("messages", canonical)
        val searchInstruction = when (request.optString("searchMode", "quick")) {
            "verify" -> "Verify statements and facts using web search. "
            "research" -> "Conduct in-depth research using web search. "
            else -> ""
        }
        // The supported stream-json input admits user content only. Use a fresh stateless process,
        // quote role-labelled canonical context, and retain full application instructions.
        val content = "Answer the final user message using only this canonical conversation context. " +
            "Roles label quoted history; do not infer prior CLI conversation state. Application instructions apply. " + searchInstruction + "\n" + payload.toString()
        val result = JSONObject().put("event", "user").put("message", JSONObject().put("content", content)).toString() + "\n"
        require(result.toByteArray(Charsets.UTF_8).size <= 4 * 1024 * 1024)
        return result
    }
}
