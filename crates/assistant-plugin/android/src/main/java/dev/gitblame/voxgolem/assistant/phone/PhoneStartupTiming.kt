package dev.gitblame.voxgolem.assistant.phone

import android.app.Activity
import android.content.Context
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

/** Independent boundary receipt, not a duration spanning entry operations. */
internal fun reportVoiceEntryStage(context: Context, stage: Stage) {
    AssistantTiming.begin(context, AssistantTiming.Operation.VOICE_ENTRY).finish(stage)
}

/** Exact IPC allowlist; never turn arbitrary frontend strings into diagnostic vocabulary. */
internal fun voiceEntryFrontendStage(stage: String): Stage? = when (stage) {
    "entry_frontend_ready" -> Stage.ENTRY_FRONTEND_READY
    "entry_frontend_claimed" -> Stage.ENTRY_FRONTEND_CLAIMED
    "entry_frontend_invalidated" -> Stage.ENTRY_FRONTEND_INVALIDATED
    "entry_frontend_busy" -> Stage.ENTRY_FRONTEND_BUSY
    "entry_frontend_unavailable" -> Stage.ENTRY_FRONTEND_UNAVAILABLE
    "entry_draft_requested" -> Stage.ENTRY_DRAFT_REQUESTED
    "entry_draft_admitted" -> Stage.ENTRY_DRAFT_ADMITTED
    "entry_draft_rejected" -> Stage.ENTRY_DRAFT_REJECTED
    "entry_channel_unavailable" -> Stage.ENTRY_CHANNEL_UNAVAILABLE
    "entry_dispatched" -> Stage.ENTRY_DISPATCHED
    "entry_resolved" -> Stage.ENTRY_RESOLVED
    "entry_native_rejected" -> Stage.ENTRY_NATIVE_REJECTED
    "entry_event_invalid" -> Stage.ENTRY_EVENT_INVALID
    else -> null
}
