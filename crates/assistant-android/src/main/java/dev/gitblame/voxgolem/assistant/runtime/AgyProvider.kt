package dev.gitblame.voxgolem.assistant.runtime

import android.content.Context
import android.os.SystemClock
import android.util.Log
import org.json.JSONObject
import java.io.BufferedReader
import java.io.File
import java.io.FileOutputStream
import java.io.InputStreamReader
import java.io.OutputStreamWriter
import java.nio.charset.StandardCharsets
import java.util.concurrent.atomic.AtomicBoolean

class AgyProviderRouter(
    private val context: Context,
    private val defaultNative: ProviderNativeCalls = AssistantProviderNative,
    private val agyNative: ProviderNativeCalls = AgyProviderNative(context)
) : ProviderNativeCalls {
    fun prewarm(model: String) {
        val eligible = model.startsWith("agy-") || model.startsWith("claude-") || model.startsWith("gemini-") || model.startsWith("opus") || model.startsWith("sonnet") || model.startsWith("3.8-")
        // Every accepted selection invalidates older work, including selections routed elsewhere.
        (agyNative as? AgyProviderNative)?.prewarm(model, eligible)
    }

    override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String {
        val model = try {
            JSONObject(requestJson).optString("model")
        } catch (_: Exception) { "" }
        return if (model.startsWith("agy-") || model.startsWith("claude-") || model.startsWith("gemini-") || model.startsWith("opus") || model.startsWith("sonnet") || model.startsWith("3.8-")) {
            agyNative.respond(requestId, authPath, requestJson, callback)
        } else {
            defaultNative.respond(requestId, authPath, requestJson, callback)
        }
    }

    override fun cancel(requestId: String) {
        agyNative.cancel(requestId)
        defaultNative.cancel(requestId)
    }
}

class AgyProviderNative(private val context: Context) : ProviderNativeCalls {
    companion object {
        private const val TAG = "AgyProvider"
    }

    private val lock = Any()
    private var process: Process? = null
    private var processWriter: OutputStreamWriter? = null
    private var processReader: BufferedReader? = null
    private var activeModel: String? = null
    private var activeSessionId: String? = null
    private var activeRequestId: String? = null
    private val cancelled = AtomicBoolean(false)
    private var prewarmGeneration = 0L

    private fun advancePrewarmSelection(): Long = synchronized(lock) { ++prewarmGeneration }

    private fun ensureCredentials() {
        val homeDir = File(context.filesDir, "home")
        val dotGemini = File(homeDir, ".gemini/antigravity-cli")
        dotGemini.mkdirs()
        val wsDir = File(context.filesDir, "ws")
        wsDir.mkdirs()

        copyAsset("agy/antigravity-oauth-token", File(dotGemini, "antigravity-oauth-token"))
        copyAsset("agy/installation_id", File(dotGemini, "installation_id"))
        copyAsset("agy/settings.json", File(dotGemini, "settings.json"))
    }

    private fun copyAsset(assetName: String, dest: File) {
        if (dest.exists() && dest.length() > 0) return
        try {
            context.assets.open(assetName).use { input ->
                FileOutputStream(dest).use { output ->
                    input.copyTo(output)
                }
            }
        } catch (e: Exception) {
            Log.w(TAG, "Could not copy asset $assetName: ${e.message}")
        }
    }

    private fun getExecutable(): File? {
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

    private fun startProcess(executable: File, targetAgyModel: String, effort: String, sessionId: String?): Boolean {
        synchronized(lock) {
            if (process != null && activeModel == targetAgyModel && isAlive(process)) {
                if (activeSessionId == null || activeSessionId == sessionId) {
                    activeSessionId = sessionId
                    return true
                }
            }
            destroyProcess()
            try {
                val homeDir = File(context.filesDir, "home")
                val wsDir = File(context.filesDir, "ws")
                val cacheDir = context.cacheDir

                val pb = ProcessBuilder(
                    executable.absolutePath,
                    "--input-format", "stream-json",
                    "--output-format", "stream-json",
                    "--dangerously-skip-permissions",
                    "--disable-slash-commands",
                    "--effort", effort,
                    "--model", targetAgyModel
                )
                pb.directory(wsDir)
                val env = pb.environment()
                env["HOME"] = homeDir.absolutePath
                env["TMPDIR"] = cacheDir.absolutePath
                env["GODEBUG"] = "netdns=cgo"
                env["PATH"] = "/product/bin:/apex/com.android.runtime/bin:/apex/com.android.art/bin:/system/bin:/system/xbin"
                env["ANDROID_ROOT"] = "/system"
                env["ANDROID_DATA"] = "/data"

                val p = pb.start()
                process = p
                activeModel = targetAgyModel
                activeSessionId = sessionId
                processWriter = OutputStreamWriter(p.outputStream, StandardCharsets.UTF_8)
                processReader = BufferedReader(InputStreamReader(p.inputStream, StandardCharsets.UTF_8))

                Thread({
                    try {
                        p.errorStream.bufferedReader().useLines { lines ->
                            // Drain without exposing engine payloads or credentials in diagnostics.
                            lines.forEach { _ -> }
                        }
                    } catch (_: Exception) {}
                }, "agy-stderr").start()
                return true
            } catch (e: Exception) {
                Log.e(TAG, "Failed to start agy process", e)
                return false
            }
        }
    }

    private fun acquirePrewarmProcess(executable: File, targetModel: String, effort: String, generation: Long): Boolean =
        synchronized(lock) {
            if (generation != prewarmGeneration) return@synchronized false
            // Respond owns model/session acquisition until its complete unwind, including cancellation.
            if (activeRequestId != null) return@synchronized false
            // Prewarm has no conversation identity and must not reset one on a reusable process.
            if (activeModel == targetModel && isAlive(process)) return@synchronized true
            startProcess(executable, targetModel, effort, null)
        }

    fun prewarm(requestedModel: String, eligible: Boolean) {
        val generation = advancePrewarmSelection()
        if (!eligible) return
        Thread({
            try {
                val executable = getExecutable() ?: return@Thread
                ensureCredentials()
                val targetAgyModel = resolveTargetModel(requestedModel)
                val effort = if (targetAgyModel.endsWith("-low")) "low" else "high"
                if (acquirePrewarmProcess(executable, targetAgyModel, effort, generation)) {
                    Log.i(TAG, "Prewarm acquired local engine process; readiness not verified")
                }
            } catch (e: Exception) {
                Log.w(TAG, "Failed to prewarm local engine: ${e.message}")
            }
        }, "agy-prewarm").start()
    }

    override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String {
        synchronized(lock) {
            if (activeRequestId != null) {
                return JSONObject().put("status", "error").put("code", "busy")
                    .put("message", "An Agy request is already active").toString()
            }
            activeRequestId = requestId
            cancelled.set(false)
        }

        return try {
            respondOwned(requestJson, callback)
        } finally {
            synchronized(lock) {
                if (activeRequestId == requestId) activeRequestId = null
            }
        }
    }

    private fun respondOwned(requestJson: String, callback: ProviderDeltaCallback): String {
        val executable = getExecutable()
        if (executable == null) {
            return JSONObject().put("status", "error").put("code", "unsupported_model")
                .put("message", "Agy native engine is not available on this device architecture").toString()
        }

        ensureCredentials()

        val reqObj = JSONObject(requestJson)
        val requestedModel = reqObj.optString("model", "agy-flash-high")
        val targetAgyModel = resolveTargetModel(requestedModel)
        val effort = if (targetAgyModel.endsWith("-low")) "low" else "high"
        val sessionId = reqObj.optString("externalSessionId", "").ifEmpty { null }
        val webEnabled = reqObj.optBoolean("webEnabled", true)
        val searchMode = reqObj.optString("searchMode", "quick")
        val instructions = reqObj.optString("instructions", "").trim()

        val messages = reqObj.optJSONArray("messages")
        var promptText = reqObj.optString("prompt", "")
        if (promptText.isEmpty() && messages != null && messages.length() > 0) {
            val last = messages.getJSONObject(messages.length() - 1)
            promptText = last.optString("content", "")
        }

        val searchDirective = when {
            !webEnabled -> "[System note: Web search is disabled. Do not use search tools. Answer strictly from existing knowledge.]\n\n"
            searchMode == "verify" -> "[System note: Verify statements and facts using web search.]\n\n"
            searchMode == "research" -> "[System note: Conduct in-depth research using web search.]\n\n"
            else -> ""
        }
        val spokenDirective = if (instructions.contains("spoken turn")) {
            "[System note: For this spoken response, give a concise answer in 1-2 short natural sentences.]\n\n"
        } else ""

        val finalPrompt = "$searchDirective$spokenDirective$promptText"

        var writer: OutputStreamWriter
        var reader: BufferedReader

        synchronized(lock) {
            if (cancelled.get()) return JSONObject().put("status", "cancelled").toString()
            if (process == null || activeModel != targetAgyModel || (activeSessionId != null && activeSessionId != sessionId) || !isAlive(process)) {
                if (!startProcess(executable, targetAgyModel, effort, sessionId)) {
                    return JSONObject().put("status", "error").put("code", "internal_error")
                        .put("message", "Failed to start agy process").toString()
                }
            } else if (activeSessionId == null) {
                activeSessionId = sessionId
            }
            writer = processWriter!!
            reader = processReader!!
        }

        callback.onReady()

        val accumulated = StringBuilder()

        try {
            val msg = JSONObject()
            msg.put("event", "user")
            val inner = JSONObject()
            inner.put("content", finalPrompt)
            msg.put("message", inner)
            val serializedRequest = msg.toString() + "\n"

            // Dispatch initiation includes lock acquisition/write/flush, not engine or HTTP readiness.
            val timing = AgyRequestTiming(SystemClock::elapsedRealtime)
            // Do not call the worker while holding the process lock: cancellation takes them in the reverse order.
            callback.onRequestDispatched()
            synchronized(lock) {
                if (cancelled.get()) return JSONObject().put("status", "cancelled").toString()
                writer.write(serializedRequest)
                writer.flush()
            }

            while (true) {
                if (cancelled.get()) {
                    destroyProcess()
                    return JSONObject().put("status", "cancelled").toString()
                }

                val line = reader.readLine() ?: break
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
                        accumulated.append(delta)
                        callback.onDelta(delta)
                    }
                } else if (event == "result") {
                    val res = obj.optJSONObject("result")
                    val status = res?.optString("status")
                    if (status == "SUCCESS") {
                        val text = accumulated.toString()
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
                        val err = res?.optString("error", "Agy execution failed") ?: "Unknown error"
                        return JSONObject()
                            .put("status", "error")
                            .put("code", "provider_failure")
                            .put("message", err)
                            .toString()
                    }
                }
            }

            val text = accumulated.toString()
            if (text.isNotEmpty()) {
                val completedMs = timing.completedMs()
                return JSONObject()
                    .put("status", "complete")
                    .put("contentType", "output_text")
                    .put("text", text)
                    .put("returnedModel", targetAgyModel)
                    .put("firstTextMs", timing.firstTextMs)
                    .put("completedMs", completedMs)
                    .toString()
            }
            return JSONObject().put("status", "error").put("code", "internal_error").put("message", "Agy process output closed").toString()
        } catch (e: Exception) {
            destroyProcess()
            return JSONObject().put("status", "error").put("code", "internal_error").put("message", e.message ?: "Stream failure").toString()
        }
    }

    override fun cancel(requestId: String) {
        synchronized(lock) {
            if (activeRequestId == requestId) {
                cancelled.set(true)
                destroyProcess()
            }
        }
    }

    private fun destroyProcess() {
        synchronized(lock) {
            try { process?.destroy() } catch (_: Exception) {}
            process = null
            processWriter = null
            processReader = null
            activeModel = null
            activeSessionId = null
        }
    }

    private fun isAlive(p: Process?): Boolean {
        if (p == null) return false
        return try {
            p.exitValue()
            false
        } catch (_: IllegalThreadStateException) {
            true
        }
    }
}
