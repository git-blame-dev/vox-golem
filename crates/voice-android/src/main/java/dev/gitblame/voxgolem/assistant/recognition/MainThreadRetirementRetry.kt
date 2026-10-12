package dev.gitblame.voxgolem.assistant.recognition

import android.os.Handler
import android.os.Looper
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean

internal fun retryRetirementOnMain(main: Handler, timeoutMillis: Long = 2_000, retry: () -> Boolean): Boolean {
    if (Looper.myLooper() == main.looper) return retry()
    val done = CountDownLatch(1)
    val completed = AtomicBoolean(false)
    if (!main.post {
        try { completed.set(runCatching(retry).getOrDefault(false)) }
        finally { done.countDown() }
    }) return false
    return try { done.await(timeoutMillis, TimeUnit.MILLISECONDS) && completed.get() }
    catch (_: InterruptedException) { Thread.currentThread().interrupt(); false }
}
