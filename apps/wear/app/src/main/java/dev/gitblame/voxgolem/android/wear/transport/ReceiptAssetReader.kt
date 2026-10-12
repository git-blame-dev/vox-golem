package dev.gitblame.voxgolem.android.wear.transport

import com.google.android.gms.tasks.Task
import com.google.android.gms.tasks.Tasks
import com.google.android.gms.wearable.DataClient
import java.io.FilterInputStream
import java.io.IOException
import java.io.InputStream
import java.util.concurrent.Executor
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

internal class ReceiptAssetReader(
    private val task: Task<DataClient.GetFdForAssetResponse>,
    private val timeoutMs: Long = 20_000,
) : AutoCloseable {
    private val lock = Any()
    private var retired = false
    private var response: DataClient.GetFdForAssetResponse? = null
    private var input: InputStream? = null
    private var deadline: java.util.concurrent.ScheduledFuture<*>? = null
    init {
        task.addOnSuccessListener(Executor { it.run() }) { acquired ->
            val late = synchronized(lock) { if (retired) true else { response = acquired; false } }
            if (late) runCatching { acquired.release() }
        }
        deadline = deadlines.schedule(::close, timeoutMs, TimeUnit.MILLISECONDS)
    }
    fun open(): InputStream {
        try {
            val acquired = Tasks.await(task, timeoutMs, TimeUnit.MILLISECONDS)
            val source = acquired.inputStream
            val late = synchronized(lock) { if (retired) true else { input = source; false } }
            if (late) { closeInput(source); throw IOException("Receipt asset retired") }
            return object : FilterInputStream(source) {
                private fun checkActive() { if (synchronized(lock) { retired }) throw IOException("Receipt asset retired") }
                override fun read(): Int { checkActive(); return super.read().also { checkActive() } }
                override fun read(bytes: ByteArray, offset: Int, count: Int): Int {
                    checkActive(); return source.read(bytes, offset, count).also { checkActive() }
                }
                override fun close() = this@ReceiptAssetReader.close()
            }
        } catch (failure: Exception) { close(); throw failure }
    }
    override fun close() {
        val owned = synchronized(lock) {
            if (retired) return
            retired = true
            (response to input).also { response = null; input = null }
        }
        deadline?.cancel(false)
        owned.first?.let { runCatching { it.release() } }
        owned.second?.let(::closeInput)
    }
    companion object {
        private val deadlines = Executors.newSingleThreadScheduledExecutor { runnable -> Thread(runnable, "capture-receipt-deadline").apply { isDaemon = true } }
        private val cleanup = Executors.newFixedThreadPool(2) { runnable -> Thread(runnable, "capture-receipt-retirement").apply { isDaemon = true } }
        private fun closeInput(input: InputStream) { cleanup.execute { runCatching { input.close() } } }
    }
}
