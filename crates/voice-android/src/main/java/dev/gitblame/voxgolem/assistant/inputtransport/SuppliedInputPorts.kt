package dev.gitblame.voxgolem.assistant.inputtransport

import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.TimeUnit

interface SuppliedInputFactory {
    fun available(language: String): Boolean
    fun open(language: String): SuppliedInputSession
}
interface SuppliedInputSession : AutoCloseable {
    fun checkSupport(result: (Boolean) -> Unit)
    fun start(events: SuppliedInputEvents)
    fun append(pcm: ByteArray): Boolean
    fun appendFromWorker(pcm: ByteArray): Boolean = append(pcm)
    fun finish()
}
interface SuppliedInputEvents {
    fun ready() {}
    fun endOfSpeech() {}
    fun partial(text: String)
    fun final(text: String)
    fun error(code: String)
}

/** Ephemeral bounded queue; never stores raw audio on disk. */
class BoundedPcmQueue(capacity: Int = 25) {
    private val chunks = ArrayBlockingQueue<ByteArray>(capacity)
    @Volatile var ended = false
        private set
    @Volatile var cancelled = false
        private set
    @Synchronized fun offer(pcm: ByteArray): Boolean {
        require(pcm.isNotEmpty() && pcm.size <= VoiceAudioContract.PCM_CHUNK_BYTES && pcm.size % 2 == 0)
        return !ended && !cancelled && chunks.offer(pcm.copyOf())
    }
    fun poll(millis: Long): ByteArray? = chunks.poll(millis, TimeUnit.MILLISECONDS)
    @Synchronized fun finish() { ended = true }
    @Synchronized fun cancel() { cancelled = true; ended = true; chunks.clear() }
    fun drained() = ended && chunks.isEmpty()
}
