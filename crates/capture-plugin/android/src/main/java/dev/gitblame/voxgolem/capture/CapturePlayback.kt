package dev.gitblame.voxgolem.capture

import app.tauri.plugin.JSObject
import java.util.concurrent.atomic.AtomicLong

/** Main-thread platform playback lifetime; callbacks can retire only their own player. */
internal class CapturePlayback(private val changed: (JSObject) -> Unit) {
    private var owner: Any? = null
    private var release: (() -> Unit)? = null
    private var revision = sequence.incrementAndGet()
    private var cleanupFailed = false
    private var retiring = false
    private var captureId: String? = null
    private var phase = "idle"

    fun snapshot(): JSObject = JSObject().put("revision", revision)
        .put("captureId", captureId ?: org.json.JSONObject.NULL).put("phase", phase)

    fun <T : Any> begin(id: String, create: () -> T, retire: (T) -> Unit): T? {
        if (!stop()) return null
        val token = create()
        owner = token
        release = { retire(token) }
        cleanupFailed = false
        captureId = id
        publish("preparing")
        return token
    }

    fun prepared(token: Any, start: () -> Unit) {
        if (owner !== token || cleanupFailed || retiring) return
        try {
            start()
            if (owner === token && !cleanupFailed) publish("playing")
        } catch (_: Exception) { finish(token, "failed") }
    }

    fun finish(token: Any, terminal: String) {
        if (owner !== token || cleanupFailed || retiring) return
        retire(terminal)
    }

    /** Only an explicit stop/replacement retries uncertain cleanup, never a late SDK callback. */
    fun stop(): Boolean = if (owner == null) true else retire("stopped")

    private fun retire(terminal: String): Boolean {
        if (retiring) return false
        retiring = true
        try {
            release?.invoke()
        } catch (_: Exception) {
            cleanupFailed = true
            publish("failed")
            return false
        } finally {
            retiring = false
        }
        owner = null
        release = null
        cleanupFailed = false
        publish(terminal)
        return true
    }

    private fun publish(value: String) {
        phase = value
        revision = sequence.incrementAndGet()
        runCatching { changed(snapshot()) }
    }

    private companion object { val sequence = AtomicLong() }
}
