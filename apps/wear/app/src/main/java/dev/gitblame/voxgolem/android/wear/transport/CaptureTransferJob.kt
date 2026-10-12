package dev.gitblame.voxgolem.android.wear.transport

import android.app.job.JobInfo
import android.app.job.JobParameters
import android.app.job.JobScheduler
import android.app.job.JobService
import android.content.ComponentName
import android.content.Context
import dev.gitblame.voxgolem.android.wear.capture.CaptureRepository
import java.time.Duration
import java.time.Instant
import java.util.concurrent.Executors

class CaptureTransferJob : JobService() {
    private val worker = Executors.newSingleThreadExecutor()
    private class Run(val parameters: JobParameters, val transport: WearCaptureTransport) {
        val jobId = parameters.jobId
        var future: java.util.concurrent.Future<*>? = null
        var cancelled = false
    }
    private val lock = Any()
    private var current: Run? = null
    internal var transportFactory: () -> WearCaptureTransport = { WearCaptureTransport(applicationContext) }
    internal var finishJob: (JobParameters) -> Unit = { jobFinished(it, false) }

    override fun onStartJob(parameters: JobParameters): Boolean {
        val run = Run(parameters, transportFactory())
        synchronized(lock) {
            current?.let { retire(it) }
            current = run
            run.future = worker.submit {
                try {
                    run.transport.transferPending()
                } finally {
                    run.transport.close()
                    android.os.Handler(mainLooper).post {
                        synchronized(lock) {
                            if (!run.cancelled && current === run) {
                                current = null
                                finishJob(run.parameters)
                                runCatching {
                                    worker.execute {
                                        synchronized(lock) {
                                            if (current == null && !run.cancelled) schedule(applicationContext)
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        return true
    }

    private fun retire(run: Run) {
        run.cancelled = true
        run.transport.close()
        run.future?.cancel(true)
    }
    override fun onStopJob(parameters: JobParameters): Boolean {
        synchronized(lock) {
            current?.takeIf { it.jobId == parameters.jobId }?.let { retire(it); current = null }
        }
        return true
    }

    override fun onDestroy() {
        synchronized(lock) { current?.let(::retire); current = null }
        worker.shutdownNow()
        super.onDestroy()
    }

    companion object {
        private const val JOB_ID = 0x5648

        fun schedule(context: Context, immediate: Boolean = false) {
            val scheduler = context.getSystemService(JobScheduler::class.java)
            val pending = CaptureRepository(context).pendingTransfers()
            if (pending.isEmpty()) {
                scheduler.cancel(JOB_ID)
                return
            }
            val next = pending.mapNotNull { record ->
                record.nextRetryAtUtc?.let { runCatching { Instant.parse(it) }.getOrNull() }
            }.minOrNull()
            val delay = if (immediate) 0L else next?.let { Duration.between(Instant.now(), it).toMillis() }
                ?.coerceIn(0L, 300_000L) ?: 30_000L
            scheduler.schedule(JobInfo.Builder(JOB_ID, ComponentName(context, CaptureTransferJob::class.java))
                .setMinimumLatency(delay)
                .setPersisted(true)
                .build())
        }
    }
}
