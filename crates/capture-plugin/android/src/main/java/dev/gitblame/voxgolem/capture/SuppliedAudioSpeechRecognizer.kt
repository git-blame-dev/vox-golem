package dev.gitblame.voxgolem.capture

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.ParcelFileDescriptor
import android.speech.RecognizerIntent
import android.speech.SpeechRecognizer
import java.io.OutputStream
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.TimeUnit
import java.util.Locale
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Android's external-audio SpeechRecognizer path. This writes only caller-supplied
 * watch PCM into EXTRA_AUDIO_SOURCE; it never requests or opens the phone microphone.
 * Provider support is deliberately runtime-probed rather than inferred from availability.
 */
internal interface SpeechRecognizerAdapter {
    data class Capabilities(
        val formats: Set<CaptureFormat>,
        val languages: Set<String>,
        val livePartials: Boolean,
        val explicitEnd: Boolean,
        val onDevice: Boolean,
        val externalAudio: Boolean,
        val accepted: Boolean,
    )

    interface Events {
        fun onAttemptState(state: String) {}
        fun onHypothesis(text: String, isFinal: Boolean)
        fun onFailure(code: String)
    }

    fun capabilities(): Capabilities
    fun start(sessionId: String, format: CaptureFormat, language: String?, events: Events)
    fun append(sessionId: String, pcm: ByteArray)
    fun finish(sessionId: String)
    fun cancel(sessionId: String)
}

internal class SuppliedAudioSpeechRecognizer(private val context: Context) : SpeechRecognizerAdapter {
    private data class Active(
        val recognizer: SpeechRecognizer,
        val readEnd: ParcelFileDescriptor,
        val output: OutputStream,
        val events: SpeechRecognizerAdapter.Events,
        val chunks: ArrayBlockingQueue<ByteArray> = ArrayBlockingQueue(50),
        val closed: AtomicBoolean = AtomicBoolean(false),
        val completion: RecognitionInputCompletion = RecognitionInputCompletion(),
    )

    private val main = Handler(Looper.getMainLooper())
    private val active = ConcurrentHashMap<String, Active>()

    override fun capabilities(): SpeechRecognizerAdapter.Capabilities = SpeechRecognizerAdapter.Capabilities(
        formats = if (Build.VERSION.SDK_INT >= 33) setOf(CaptureFormat()) else emptySet(),
        languages = emptySet(),
        livePartials = true,
        explicitEnd = true,
        onDevice = Build.VERSION.SDK_INT >= 31 && runCatching { SpeechRecognizer.isOnDeviceRecognitionAvailable(context) }.getOrDefault(false),
        externalAudio = Build.VERSION.SDK_INT >= 33,
        // Android exposes no stable API for preflighting provider acceptance of external PCM.
        accepted = false,
    )

    @android.annotation.TargetApi(33)
    override fun start(sessionId: String, format: CaptureFormat, language: String?, events: SpeechRecognizerAdapter.Events) {
        require(Build.VERSION.SDK_INT >= 33) { "external-audio-speech-api-unavailable" }
        require(format in capabilities().formats) { "unsupported-external-audio-format" }
        require(capabilities().onDevice) { "on-device-speech-provider-unavailable" }
        check(context.checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED) {
            "speech-recognizer-requires-record-audio-permission"
        }
        onMain {
            check(!active.containsKey(sessionId)) { "recognition-session-already-active" }
            val (readEnd, writeEnd) = ParcelFileDescriptor.createPipe()
            val recognizer = SpeechRecognizer.createOnDeviceSpeechRecognizer(context)
            val output = ParcelFileDescriptor.AutoCloseOutputStream(writeEnd)
            val current = Active(recognizer, readEnd, output, events)
            active[sessionId] = current
            startWriter(sessionId, current)
            recognizer.setRecognitionListener(object : SegmentedRecognitionCallbacks() {
                private var finalSent = false
                private val transcript = SegmentedTranscriptAssembler()

                override fun onReadyForSpeech(params: android.os.Bundle?) {
                    if (active[sessionId] === current) events.onAttemptState("provider-ready")
                }
                override fun onBeginningOfSpeech() = Unit
                override fun onRmsChanged(rmsdB: Float) = Unit
                override fun onBufferReceived(buffer: ByteArray?) = Unit
                override fun onEndOfSpeech() = Unit
                override fun onEvent(eventType: Int, params: android.os.Bundle?) = Unit
                override fun onPartialResults(partialResults: android.os.Bundle?) {
                    if (finalSent || active[sessionId] !== current) return
                    val text = partialResults?.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)?.firstOrNull() ?: return
                    events.onHypothesis(transcript.hypothesis(text), false)
                }
                override fun onSegmentResultsReceived(segmentResults: android.os.Bundle?) {
                    if (finalSent || active[sessionId] !== current) return
                    val segment = segmentResults?.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)?.firstOrNull() ?: return
                    if (segment.isNotBlank()) {
                        events.onHypothesis(transcript.commit(segment), false)
                    }
                }
                override fun onSegmentedSessionEnded() {
                    if (finalSent || active[sessionId] !== current) return
                    finalSent = true
                    if (!current.completion.canAcceptFinal()) {
                        events.onFailure("speech-provider-ended-before-source")
                        dispose(sessionId)
                        return
                    }
                    events.onHypothesis(transcript.finish(""), true)
                    dispose(sessionId)
                }
                override fun onResults(results: android.os.Bundle?) {
                    if (finalSent || active[sessionId] !== current) return
                    finalSent = true
                    if (!current.completion.canAcceptFinal()) {
                        events.onFailure("speech-provider-ended-before-source")
                        dispose(sessionId)
                        return
                    }
                    val text = results?.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)?.firstOrNull().orEmpty()
                    events.onHypothesis(transcript.finish(text), true)
                    dispose(sessionId)
                }
                override fun onError(error: Int) {
                    if (finalSent || active[sessionId] !== current) return
                    finalSent = true
                    events.onFailure("speech-recognizer-$error")
                    dispose(sessionId)
                }
            })
            val intent = Intent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH).apply {
                putExtra(RecognizerIntent.EXTRA_LANGUAGE_MODEL, RecognizerIntent.LANGUAGE_MODEL_FREE_FORM)
                putExtra(RecognizerIntent.EXTRA_PARTIAL_RESULTS, true)
                putExtra(RecognizerIntent.EXTRA_LANGUAGE, language?.takeIf(String::isNotBlank) ?: Locale.getDefault().toLanguageTag())
                putExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE, readEnd)
                putExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE_CHANNEL_COUNT, format.channels)
                putExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE_ENCODING, android.media.AudioFormat.ENCODING_PCM_16BIT)
                putExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE_SAMPLING_RATE, format.sampleRate)
                putExtra(RecognizerIntent.EXTRA_SEGMENTED_SESSION, RecognizerIntent.EXTRA_AUDIO_SOURCE)
            }
            try {
                recognizer.startListening(intent)
            } catch (failure: Throwable) {
                active.remove(sessionId)
                current.readEnd.close()
                current.output.close()
                recognizer.destroy()
                events.onFailure("external-audio-rejected-${failure.javaClass.simpleName}")
            }
        }
    }

    override fun append(sessionId: String, pcm: ByteArray) {
        require(pcm.size % 2 == 0) { "unaligned-pcm-frame" }
        val current = active[sessionId] ?: return
        check(!current.closed.get()) { "recognition-session-closed" }
        if (!current.chunks.offer(pcm.copyOf(), 250, TimeUnit.MILLISECONDS)) {
            current.events.onFailure("external-audio-provider-backpressure")
            cancel(sessionId)
            throw IllegalStateException("external-audio-provider-backpressure")
        }
    }

    override fun finish(sessionId: String) {
        active[sessionId]?.closed?.set(true)
        // The writer drains queued frames before closing the pipe, which signals source EOF.
    }

    override fun cancel(sessionId: String) = onMain { dispose(sessionId) }

    private fun dispose(sessionId: String) {
        val current = active.remove(sessionId) ?: return
        current.closed.set(true)
        current.chunks.clear()
        runCatching { current.output.close() }
        runCatching { current.readEnd.close() }
        runCatching { current.recognizer.cancel() }
        current.recognizer.destroy()
    }

    private fun onMain(action: () -> Unit) {
        if (Looper.myLooper() == Looper.getMainLooper()) {
            action()
            return
        }
        val latch = java.util.concurrent.CountDownLatch(1)
        var failure: Throwable? = null
        main.post {
            try { action() } catch (error: Throwable) { failure = error } finally { latch.countDown() }
        }
        check(latch.await(10, TimeUnit.SECONDS)) { "speech-recognizer-main-thread-timeout" }
        failure?.let { throw it }
    }

    private fun startWriter(sessionId: String, current: Active) {
        Thread({
            try {
                while (true) {
                    val next = current.chunks.poll(100, TimeUnit.MILLISECONDS)
                    if (next == null) {
                        if (current.closed.get()) break
                        continue
                    }
                    current.output.write(next)
                }
                current.completion.closeAfterDrain {
                    current.output.flush()
                    current.output.close()
                }
            } catch (failure: Exception) {
                if (active[sessionId] === current && !current.completion.canAcceptFinal()) {
                    current.events.onFailure("external-audio-write-failed")
                    main.post { dispose(sessionId) }
                }
            }
        }, "capture-audio-$sessionId").apply { isDaemon = true; start() }
    }

}

/** Turns segmented commits plus the mutable current hypothesis into one full-text snapshot. */
internal class SegmentedTranscriptAssembler {
    private val committed = StringBuilder()

    fun commit(segment: String): String {
        val normalized = segment.trim()
        if (normalized.isNotEmpty()) {
            if (committed.isNotEmpty()) committed.append(' ')
            committed.append(normalized)
        }
        return committed.toString()
    }

    fun hypothesis(text: String): String = join(committed.toString(), text)

    fun finish(text: String): String = join(committed.toString(), text)

    private fun join(prefix: String, suffix: String): String {
        if (prefix.isBlank()) return suffix
        if (suffix.isBlank() || suffix == prefix || suffix.startsWith("$prefix ")) return if (suffix.isBlank()) prefix else suffix
        return "$prefix ${suffix.trim()}"
    }
}
