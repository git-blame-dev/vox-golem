package dev.gitblame.voxgolem.assistant.phone

import android.app.Activity
import android.os.Process
import dev.gitblame.voxgolem.assistant.AssistantTiming
import dev.gitblame.voxgolem.assistant.AssistantTiming.Stage

/** Keeps the shared timing implementation behind the shell's existing plugin dependency. */
class PhoneStartupTiming(activity: Activity) {
    // Android publishes process start in the elapsed-realtime clock used by AssistantTiming.
    internal val trace = AssistantTiming.begin(activity, AssistantTiming.Operation.ACTIVITY,
        originNanos = Process.getStartElapsedRealtime() * 1_000_000L)

    fun activityCreated() = trace.mark(Stage.ACTIVITY_CREATED)
    fun webViewCreated() = trace.mark(Stage.WEBVIEW_CREATED)
    fun close() = trace.finish(Stage.CANCELLED)
}

interface PhoneStartupTimingOwner {
    val startupTiming: PhoneStartupTiming
}
