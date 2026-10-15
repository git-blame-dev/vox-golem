package dev.gitblame.voxgolem.assistant.runtime

import android.content.Context
import android.util.Log
import com.google.ai.edge.aicore.DownloadCallback
import com.google.ai.edge.aicore.DownloadConfig
import com.google.ai.edge.aicore.GenerativeAIException
import com.google.ai.edge.aicore.GenerativeModel
import com.google.ai.edge.aicore.generationConfig
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.Executors
import kotlin.coroutines.resume
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.flowOn
import kotlinx.coroutines.flow.mapNotNull
import kotlinx.coroutines.launch
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout

internal interface LegacyNanoClient : AutoCloseable {
    suspend fun prepareInferenceEngine()
    fun generateContentStream(prompt: String): Flow<String>
    suspend fun awaitCancellationDispatch() {}
}

private const val TAG = "LegacyGeminiNano"

private class AicoreLegacyNanoClient(context: Context) : LegacyNanoClient {
    private val orderedWorker = Executors.newSingleThreadExecutor()
    private val config = generationConfig {
        // The public builder allocates a default pool even when it is replaced.
        this.workerExecutor.shutdown()
        this.workerExecutor = orderedWorker
        this.context = context.applicationContext
        candidateCount = 1
        maxOutputTokens = 256
        temperature = 0.0f
    }
    // SDK close does not shut down its configured executor.
    private val workerExecutor = config.workerExecutor
    private val model = try {
        GenerativeModel(config, DownloadConfig(object : DownloadCallback {
            override fun onDownloadStarted(bytesToDownload: Long) {
                Log.i(TAG, "stage=download_started")
            }
            override fun onDownloadCompleted() {
                Log.i(TAG, "stage=download_completed")
            }
            override fun onDownloadDidNotStart(e: GenerativeAIException) {
                Log.w(TAG, "stage=download_not_started sdk_code=${e.errorCode}")
            }
            override fun onDownloadFailed(failureStatus: String, e: GenerativeAIException) {
                Log.w(TAG, "stage=download_failed sdk_code=${e.errorCode}")
            }
        }))
    } catch (error: Throwable) {
        workerExecutor.shutdown()
        throw error
    }

    override suspend fun prepareInferenceEngine() {
        model.prepareInferenceEngine()
    }

    override fun generateContentStream(prompt: String): Flow<String> =
        model.generateContentStream(prompt).mapNotNull { it.text }

    override suspend fun awaitCancellationDispatch() {
        // SDK cancellation listeners and this barrier share FIFO execution. This proves only
        // local dispatch ordering: AICore's one-way Binder cancellation has no remote ack.
        suspendCancellableCoroutine<Unit> { continuation ->
            workerExecutor.execute { continuation.resume(Unit) }
        }
    }

    override fun close() {
        try {
            model.close()
        } finally {
            workerExecutor.shutdown()
        }
    }
}

class DefaultGeminiNanoEngine internal constructor(
    private val clientFactory: () -> LegacyNanoClient,
    private val dispatcher: CoroutineDispatcher = Dispatchers.IO
) : GeminiNanoEngine {
    constructor(context: Context) : this({ AicoreLegacyNanoClient(context.applicationContext) })

    private val admission = Mutex()
    private val closed = AtomicBoolean(false)
    private val lifecycle = Any()
    private var activeJob: Job? = null // Guarded by lifecycle, not the suspending admission mutex.
    private var client: LegacyNanoClient? = null
    private var warmed = false
    private var dispatchFailed = false // Guarded by admission; a failed barrier poisons reuse.

    private suspend fun awaitCancellationDispatch(): Boolean = withContext(NonCancellable) {
        try {
            withTimeout(5_000) { client?.awaitCancellationDispatch() }
            true
        } catch (error: Exception) {
            dispatchFailed = true
            Log.w(TAG, "stage=cancellation_dispatch_failed sdk_code=${(error as? GenerativeAIException)?.errorCode ?: -1}")
            false
        }
    }

    private suspend fun <T> admitted(operation: suspend () -> T): T = admission.withLock {
        // A dedicated child job lets close cancel this operation without cancelling its caller's parent.
        coroutineScope {
            val job = currentCoroutineContext()[Job]!!
            synchronized(lifecycle) {
                check(!closed.get()) { "Gemini Nano engine is closed" }
                check(!dispatchFailed) { "Gemini Nano cancellation dispatch failed; engine cannot be reused" }
                activeJob = job
            }
            var completedNormally = false
            try {
                currentCoroutineContext().ensureActive()
                operation().also { completedNormally = true }
            } finally {
                // SDK callback-flow cleanup can queue cancellation after a downstream exception
                // while this admitted scope is still active, not only after Job.cancel().
                if (!completedNormally || job.isCancelled) awaitCancellationDispatch()
                synchronized(lifecycle) { activeJob = null }
            }
        }
    }

    private fun getClient(): LegacyNanoClient {
        check(!closed.get()) { "Gemini Nano engine is closed" }
        return client ?: clientFactory().also { client = it }
    }

    private suspend fun prepare(client: LegacyNanoClient) {
        if (warmed) return
        Log.i(TAG, "stage=sdk_prepare_started")
        try {
            client.prepareInferenceEngine()
            currentCoroutineContext().ensureActive()
            warmed = true
            Log.i(TAG, "stage=sdk_prepare_completed")
        } catch (error: Throwable) {
            currentCoroutineContext().ensureActive()
            Log.w(TAG, "stage=sdk_prepare_failed sdk_code=${(error as? GenerativeAIException)?.errorCode ?: -1}")
            throw error
        }
    }

    override suspend fun prepareInferenceEngine() = withContext(dispatcher) {
        admitted { prepare(getClient()) }
    }

    override fun generateContentStream(prompt: String): Flow<String> = flow {
        admitted {
            val activeClient = getClient()
            prepare(activeClient)
            Log.i(TAG, "stage=stream_started")
            try {
                activeClient.generateContentStream(prompt).collect { emit(it) }
                currentCoroutineContext().ensureActive()
                Log.i(TAG, "stage=stream_completed")
            } catch (error: Throwable) {
                // Legacy SDK can wrap cancellation in an inference exception.
                currentCoroutineContext().ensureActive()
                Log.w(TAG, "stage=stream_failed sdk_code=${(error as? GenerativeAIException)?.errorCode ?: -1}")
                throw error
            }
        }
    }.flowOn(dispatcher)

    override fun close() {
        val admittedJob = synchronized(lifecycle) {
            if (!closed.compareAndSet(false, true)) return
            activeJob
        }
        admittedJob?.cancel()
        // Wait for cancellation cleanup off the UI thread before retiring SDK resources.
        CoroutineScope(dispatcher).launch {
            admission.withLock {
                if (!awaitCancellationDispatch()) {
                    // Keep the client quarantined rather than closing ahead of queued SDK work.
                    Log.w(TAG, "stage=close_skipped reason=cancellation_dispatch_failed")
                    return@withLock
                }
                try {
                    client?.close()
                } catch (error: Exception) {
                    Log.w(TAG, "stage=close_failed sdk_code=${(error as? GenerativeAIException)?.errorCode ?: -1}")
                } finally {
                    client = null
                    warmed = false
                }
            }
        }
    }
}
