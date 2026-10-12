package dev.gitblame.voxgolem.capture

import android.content.Context
import android.content.pm.ApplicationInfo
import android.content.pm.PackageManager
import android.os.Build
import android.os.SystemClock
import android.os.Trace
import android.util.Log
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger

/** Local duration markers only: no recording, transcript, capture identity or transport data. */
class CaptureTiming(context: Context, private val component: String) {
    private val startedAt = SystemClock.elapsedRealtime()
    private val run = sequence.incrementAndGet()
    private val enabled = context.applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE != 0 ||
        runCatching {
            context.packageManager.getApplicationInfo(context.packageName, PackageManager.GET_META_DATA)
                .metaData?.getBoolean(ENABLED_METADATA, false) == true
        }.getOrDefault(false)
    private val stages = ConcurrentHashMap.newKeySet<String>()
    private val closed = AtomicBoolean(false)

    init {
        if (enabled && Build.VERSION.SDK_INT >= 29) Trace.beginAsyncSection("VoxGolem/$component", run)
    }

    fun mark(stage: String) {
        if (!enabled || !stages.add(stage)) return
        Log.i(TAG, "component=$component run=$run stage=$stage elapsed_ms=${SystemClock.elapsedRealtime() - startedAt}")
    }

    fun finish(stage: String) {
        mark(stage)
        if (closed.compareAndSet(false, true) && enabled && Build.VERSION.SDK_INT >= 29) {
            Trace.endAsyncSection("VoxGolem/$component", run)
        }
    }

    val isFinished: Boolean get() = closed.get()

    companion object {
        const val TAG = "VoxGolemCaptureTiming"
        const val ENABLED_METADATA = "dev.gitblame.voxgolem.capture.TIMING_ENABLED"
        private val sequence = AtomicInteger()
    }
}

/** One current watch launch for local cross-component correlation; identities are never logged. */
object CaptureStartupTiming {
    @Volatile private var launch: CaptureTiming? = null
    @Volatile private var captureId: String? = null

    fun begin(context: Context): CaptureTiming {
        launch?.finish("launch-replaced")
        captureId = null
        return CaptureTiming(context, "watch").also { launch = it; it.mark("activity-entry") }
    }

    fun current(): CaptureTiming? = launch

    fun bind(id: String) { captureId = id }

    fun forCapture(id: String): CaptureTiming? = launch?.takeIf { captureId == id }
}
