package dev.gitblame.voxgolem.assistant

import android.content.Context
import android.content.pm.ApplicationInfo
import android.content.pm.PackageManager
import android.os.Build
import android.os.SystemClock
import android.os.Trace
import android.util.Log
import java.util.EnumSet
import java.util.concurrent.atomic.AtomicInteger

/** Process-local monotonic diagnostics; only fixed vocabulary and ephemeral ordinals are emitted. */
class AssistantTiming private constructor(
    val operation: Operation,
    val run: Int,
    private val parentRun: Int?,
    private val enabled: Boolean,
    private val clock: () -> Long,
    private val sink: Sink,
    origin: Long? = null,
) {
    enum class Operation { PREPARATION, SPEECH, TTS_INITIALIZATION, TTS_UTTERANCE, RECOGNITION, SUPPORT_CHECK, GENERATION, ACTIVITY }
    enum class Stage {
        REQUESTED, ADMITTED, READY_REUSED, PENDING_REUSED,
        CREATE_REQUESTED, INIT_CALLBACK, INIT_MAIN_DELIVERED, INIT_READY, INIT_FAILED, VOICES_READY,
        LOCAL_ROUTE_SELECTED, NETWORK_ROUTE_SELECTED, LEASE_ACQUIRED, LEASE_DENIED, FOCUS_ACQUIRED, FOCUS_DENIED,
        QUEUE_REQUESTED, QUEUE_ACCEPTED, QUEUE_REJECTED, ON_START, ON_DONE, ON_ERROR, ON_STOP,
        SETTLED, FAILED, CANCELLED, REPLACED, SESSION_RETIRED, RETIREMENT_FAILED,
        SDK_CREATED, SUPPORT_REQUESTED, SUPPORT_CALLBACK, SUPPORTED, UNSUPPORTED,
        LISTENING_REQUESTED, READY_CALLBACK, READY_MAIN_DELIVERED, FIRST_PARTIAL, END_OF_SPEECH,
        FINISH_REQUESTED, FINAL_CALLBACK, FINAL_MAIN_DELIVERED, TIMED_OUT,
        WORKER_STARTED, NATIVE_READY, FIRST_TEXT, PROVISIONAL_STATE_APPLIED, OUTCOME_RECEIVED, DELIVERY_WAIT, DELIVERY_READY, DURABLE_COMMITTED,
        ACTIVITY_CREATED, RESUMED, FIRST_PROJECTION,
    }
    enum class Kind { BEGIN, MARK, END }
    data class Event(val operation: Operation, val run: Int, val parentRun: Int?, val kind: Kind,
        val stage: Stage?, val elapsedNanos: Long, val observedNanos: Long)
    fun interface Sink { fun emit(event: Event) }

    private val startedAt = if (enabled) origin ?: now() else 0L
    private val stages = if (enabled) EnumSet.noneOf(Stage::class.java) else null
    @Volatile private var finished = false
    val isFinished: Boolean get() = finished

    init { if (enabled) emit(Kind.BEGIN, null) }

    fun mark(stage: Stage) {
        if (!enabled || finished) return
        synchronized(this) { if (!finished && stages!!.add(stage)) emit(Kind.MARK, stage) }
    }

    fun finish(stage: Stage) {
        if (finished) return
        synchronized(this) {
            if (finished) return
            finished = true
            if (enabled) {
                if (stages!!.add(stage)) emit(Kind.MARK, stage)
                emit(Kind.END, stage)
            }
        }
    }

    /** Children share the root's monotonic origin and observer, but retire independently (including warm SDKs). */
    fun child(operation: Operation): AssistantTiming = AssistantTiming(operation,
        if (enabled) sequence.incrementAndGet() else 0, if (enabled) run else null, enabled, clock, sink, startedAt)

    private fun now(): Long = try { clock() } catch (_: RuntimeException) { 0L }
    private fun emit(kind: Kind, stage: Stage?) {
        try {
            val observed = now()
            sink.emit(Event(operation, run, parentRun, kind, stage, (observed - startedAt).coerceAtLeast(0), observed))
        }
        catch (_: RuntimeException) { /* Diagnostics must not alter resource or callback ownership. */ }
    }

    companion object {
        const val TAG = "VoxGolemAssistantTiming"
        const val ENABLED_METADATA = "dev.gitblame.voxgolem.assistant.TIMING_ENABLED"
        private val sequence = AtomicInteger()

        fun begin(context: Context?, operation: Operation,
            clock: () -> Long = SystemClock::elapsedRealtimeNanos, sink: Sink = AndroidSink): AssistantTiming {
            val enabled = context != null && (context.applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE != 0 ||
                context.applicationInfo.metaData?.getBoolean(ENABLED_METADATA, false) == true ||
                runCatching {
                    context.packageManager.getApplicationInfo(context.packageName, PackageManager.GET_META_DATA)
                        .metaData?.getBoolean(ENABLED_METADATA, false) == true
                }.getOrDefault(false))
            return AssistantTiming(operation, if (enabled) sequence.incrementAndGet() else 0, null, enabled, clock, sink)
        }
    }

    private object AndroidSink : Sink {
        override fun emit(event: Event) {
            val name = "VoxGolem/assistant/${event.operation.name}"
            when (event.kind) {
                Kind.BEGIN -> if (Build.VERSION.SDK_INT >= 29) Trace.beginAsyncSection(name, event.run)
                Kind.END -> if (Build.VERSION.SDK_INT >= 29) Trace.endAsyncSection(name, event.run)
                Kind.MARK -> Log.i(TAG, "operation=${event.operation.name} run=${event.run}" +
                    (event.parentRun?.let { " parent=$it" } ?: "") +
                    " stage=${event.stage!!.name} elapsed_ms=${event.elapsedNanos / 1_000_000}" +
                    " observed_ms=${event.observedNanos / 1_000_000}")
            }
        }
    }
}
