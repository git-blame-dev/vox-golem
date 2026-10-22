package dev.gitblame.voxgolem.assistant.runtime

import android.content.Context
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.system.Os
import android.util.Log
import dev.gitblame.voxgolem.assistant.replication.ChatGptAccessSync
import org.json.JSONObject
import java.io.File
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.Executor
import java.util.concurrent.RejectedExecutionException
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit

/** Process lifetime owner: leaving the Activity to enter a code does not cancel Rust auth. */
object ChatGptAuth {
    const val VERIFICATION_URL = "https://auth.openai.com/codex/device"
    private val worker = ThreadPoolExecutor(1, 1, 30, TimeUnit.SECONDS, ArrayBlockingQueue(4),
        { work -> Thread(work, "chatgpt-auth") }, ThreadPoolExecutor.AbortPolicy()).apply { allowCoreThreadTimeOut(true) }
    private var owner: Owner? = null
    /** Rust requires a private (0700) parent; Android's app directories are 0771, so own a dedicated one. */
    fun managedFile(context: Context): File {
        val directory = File(context.applicationContext.noBackupFilesDir, "chatgpt")
        runCatching { directory.mkdirs(); Os.chmod(directory.path, 0b111_000_000) }
        return File(directory, "chatgpt-auth.json")
    }

    fun command(context: Context, action: String, completed: (JSONObject) -> Unit) {
        require(action in setOf("start", "status", "cancel", "sign_out"))
        resolve(context).command(action, completed)
    }

    @Synchronized private fun resolve(context: Context): Owner = owner ?: run {
        val app = context.applicationContext
        val main = Handler(Looper.getMainLooper())
        Owner(worker, { action -> AssistantProviderNative.chatGptAuth(managedFile(app).path, action) },
            { task, delay -> main.postDelayed(task, delay); { main.removeCallbacks(task) } },
            SystemClock::elapsedRealtime, {
                AssistantRuntime.refreshIfStarted()
                ChatGptAccessSync.changed(app)
            }, AssistantRuntime::cancelActiveForSignOut).also { owner = it }
    }

    /** Scheduling only: Rust remains the sole authorization/polling/credential state owner. */
    internal class Owner(
        private val executor: Executor,
        private val native: (String) -> String,
        private val schedule: (Runnable, Long) -> (() -> Unit),
        private val now: () -> Long,
        private val changed: () -> Unit,
        private val cancelAssistant: () -> Unit,
    ) {
        companion object {
            const val CHECK_MILLIS = 1_000L
            const val MAX_WATCH_MILLIS = 15 * 60_000L + 5_000L
        }
        private class Read(val epoch: Long, val callbacks: MutableList<(JSONObject) -> Unit> = mutableListOf())
        private val lock = Any()
        private var epoch = 0L
        private var deadline: Long? = null
        private var timer: (() -> Unit)? = null
        private var timerVersion = 0L
        private var read: Read? = null
        private var lastPhase: String? = null

        fun command(action: String, completed: (JSONObject) -> Unit) {
            require(action in setOf("start", "status", "cancel", "sign_out"))
            if (action == "status") { status(completed); return }
            val (version, previousDeadline) = synchronized(lock) {
                val previous = deadline
                epoch++
                clearTimer()
                read = null
                if (action != "start") deadline = null
                else if (deadline == null || deadline!! <= now()) deadline = now() + MAX_WATCH_MILLIS
                epoch to previous
            }
            execute({
                if (!current(version)) { completed(failure("auth_cancelled")); return@execute }
                val result = call(action)
                publish(version, action, result)
                deliver(version, result, completed)
            }) {
                synchronized(lock) {
                    if (epoch == version) { deadline = previousDeadline; arm(version) }
                }
                completed(failure("auth_busy"))
            }
        }

        private fun status(completed: ((JSONObject) -> Unit)? = null) {
            val request: Read
            synchronized(lock) {
                val active = read
                if (active != null) {
                    if (completed != null) {
                        if (active.callbacks.size == 4) completed(failure("auth_busy")) else active.callbacks.add(completed)
                    }
                    return
                }
                clearTimer()
                request = Read(epoch)
                completed?.let(request.callbacks::add)
                read = request
            }
            execute({
                val result = if (current(request.epoch)) call("status") else failure("auth_cancelled")
                publish(request.epoch, "status", result)
                val callbacks = synchronized(lock) {
                    if (read === request) read = null
                    request.callbacks.toList()
                }
                callbacks.forEach { deliver(request.epoch, result, it) }
            }) {
                val callbacks = synchronized(lock) {
                    if (read === request) read = null
                    request.callbacks.toList()
                }
                callbacks.forEach { it(failure("auth_busy")) }
                synchronized(lock) { if (request.epoch == epoch) arm(request.epoch) }
            }
        }

        private fun call(action: String): JSONObject = try {
            if (action == "sign_out") cancelAssistant()
            publicStatus(native(action))
        } catch (_: Exception) { failure("auth_unavailable") }
          catch (_: LinkageError) { failure("auth_unavailable") }

        private fun publish(version: Long, action: String, value: JSONObject) = synchronized(lock) {
            if (epoch != version) return@synchronized
            val phase = value.getString("phase")
            if (lastPhase != phase) { lastPhase = phase; Log.i("ChatGptAuth", "stage=$phase") }
            if (phase in setOf("starting", "pending") && action !in setOf("cancel", "sign_out")) {
                if (deadline == null) deadline = now() + MAX_WATCH_MILLIS
                arm(version)
            } else {
                deadline = null
                clearTimer()
            }
            if (phase == "connected" || action == "sign_out" && phase == "signed_out") changed()
        }

        private fun current(version: Long) = synchronized(lock) { epoch == version }
        private fun deliver(version: Long, result: JSONObject, callback: (JSONObject) -> Unit) = synchronized(lock) {
            callback(if (epoch == version) result else failure("auth_cancelled"))
        }
        private fun clearTimer() { timer?.invoke(); timer = null; timerVersion++ }
        private fun arm(version: Long) {
            clearTimer()
            val remaining = (deadline ?: return) - now()
            if (remaining <= 0) return
            val scheduled = timerVersion
            timer = schedule(Runnable {
                synchronized(lock) {
                    // A removed Handler callback can already be dequeued. Fence it as well.
                    if (epoch != version || timerVersion != scheduled || now() > (deadline ?: return@Runnable)) return@Runnable
                    timer = null
                    status()
                }
            }, minOf(CHECK_MILLIS, remaining))
        }

        private fun execute(work: () -> Unit, rejected: () -> Unit) {
            try { executor.execute(work) } catch (_: RejectedExecutionException) { rejected() }
        }
    }

    internal fun publicStatus(raw: String): JSONObject {
        require(raw.toByteArray(Charsets.UTF_8).size <= 4096)
        val value = JSONObject(raw)
        val phase = value.getString("phase")
        require(phase in setOf("signed_out", "starting", "pending", "connected", "error"))
        val safe = JSONObject().put("phase", phase)
        if (phase == "pending") {
            val code = value.getString("userCode")
            require(code.matches(Regex("[A-Za-z0-9-]{4,32}")))
            safe.put("userCode", code).put("verificationUrl", VERIFICATION_URL)
        }
        // Native errors are deliberately not passed through: they are not a credential-safe API.
        if (phase == "error") safe.put("error", "ChatGPT sign-in failed. Please try again.")
        return safe
    }

    private fun failure(code: String) = JSONObject().put("phase", "error").put("error", code)
}
