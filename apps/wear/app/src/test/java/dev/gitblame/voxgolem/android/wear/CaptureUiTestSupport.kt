package dev.gitblame.voxgolem.android.wear

import android.os.Looper
import org.robolectric.Shadows.shadowOf

internal fun awaitCaptureIdle(activity: CaptureActivity) {
    val deadline = System.nanoTime() + java.util.concurrent.TimeUnit.SECONDS.toNanos(5)
    do {
        shadowOf(Looper.getMainLooper()).idle()
        if (!activity.commandsPending && !activity.presentation.refreshing) return
        check(System.nanoTime() < deadline) { "Capture IO did not settle" }
        Thread.sleep(1)
    } while (true)
}
