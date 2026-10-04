package dev.gitblame.voxgolem.android.wear.transport

import android.app.job.JobInfo
import android.app.job.JobParameters
import android.app.job.JobScheduler
import android.app.job.JobService
import android.content.ComponentName
import android.content.Context
import dev.gitblame.voxgolem.android.wear.capture.CaptureRepository
import dev.gitblame.voxgolem.capture.CaptureState
import dev.gitblame.voxgolem.capture.TransferState
import java.time.Duration
import java.time.Instant
import java.util.concurrent.Executors

class CaptureTransferJob : JobService() {
    private val worker = Executors.newSingleThreadExecutor()

    override fun onStartJob(parameters: JobParameters): Boolean {
        worker.execute {
            try {
                WearCaptureTransport(applicationContext).transferPending()
            } finally {
                jobFinished(parameters, false)
                schedule(applicationContext)
            }
        }
        return true
    }

    override fun onStopJob(parameters: JobParameters): Boolean = true

    override fun onDestroy() {
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
