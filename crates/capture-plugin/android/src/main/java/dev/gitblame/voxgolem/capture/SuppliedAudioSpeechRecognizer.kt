package dev.gitblame.voxgolem.capture

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.ParcelFileDescriptor
import android.os.SystemClock
import android.speech.RecognizerIntent
import android.speech.SpeechRecognizer
import dev.gitblame.voxgolem.assistant.recognition.AndroidLocalRecognizerFactory
import dev.gitblame.voxgolem.assistant.inputtransport.AssistantSegmentedListener
import java.io.OutputStream
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.TimeUnit
import java.util.Locale
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Android's external-audio SpeechRecognizer path. This writes only caller-supplied
 * watch PCM into EXTRA_AUDIO_SOURCE. The app opens no phone microphone; provider acceptance
 * and input-source isolation require physical verification, not an availability result.
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
        fun isCurrent(): Boolean = true
        fun onAttemptState(state: String) {}
        fun onHypothesis(text: String, isFinal: Boolean)
        fun onFailure(code: String)
    }

    fun capabilities(): Capabilities
    fun supportsPolicy(networkFallback: Boolean, capabilities: Capabilities = capabilities()): Boolean = capabilities.onDevice
    fun start(sessionId: String, format: CaptureFormat, language: String?, events: Events)
    fun startWithPolicy(sessionId: String, format: CaptureFormat, language: String?, networkFallback: Boolean, events: Events) =
        start(sessionId, format, language, events)
    fun append(sessionId: String, pcm: ByteArray)
    fun finish(sessionId: String)
    fun cancel(sessionId: String)
    fun appendOwned(sessionId: String, owner: Events, pcm: ByteArray): Boolean {
        if (!owner.isCurrent()) return false
        append(sessionId, pcm); return true
    }
    fun finishOwned(sessionId: String, owner: Events): Boolean {
        if (!owner.isCurrent()) return false
        finish(sessionId); return true
    }
    fun cancelOwned(sessionId: String, owner: Events) = cancel(sessionId)
}

internal class SuppliedAudioSpeechRecognizer @JvmOverloads constructor(
    private val context: Context,
    private val destroyRecognizer: (SpeechRecognizer) -> Unit = { it.destroy() },
) : SpeechRecognizerAdapter {
    private data class PcmChunk(val bytes: ByteArray, val queuedAt: Long)
    private data class Active(
        val recognizer: SpeechRecognizer,
        val readEnd: ParcelFileDescriptor,
        val output: OutputStream,
        val events: SpeechRecognizerAdapter.Events,
        val probeRetirement: PhoneCaptureCoordinator.ProbeRetirementObserver?,
        val chunks: ArrayBlockingQueue<PcmChunk> = ArrayBlockingQueue(50),
        val closed: AtomicBoolean = AtomicBoolean(false),
        val retiring: AtomicBoolean = AtomicBoolean(false),
        val completion: RecognitionInputCompletion = RecognitionInputCompletion(),
    )

    private val main = Handler(Looper.getMainLooper())
    private val factory = AndroidLocalRecognizerFactory(context.applicationContext)
    private val active = ConcurrentHashMap<String, Active>()

    override fun capabilities(): SpeechRecognizerAdapter.Capabilities = SpeechRecognizerAdapter.Capabilities(
        formats = if (Build.VERSION.SDK_INT >= 33) setOf(16_000, 24_000, 48_000).map { CaptureFormat(sampleRate = it) }.toSet() else emptySet(),
        languages = emptySet(),
        livePartials = true,
        explicitEnd = true,
        onDevice = Build.VERSION.SDK_INT >= 31 && runCatching { SpeechRecognizer.isOnDeviceRecognitionAvailable(context) }.getOrDefault(false),
        externalAudio = Build.VERSION.SDK_INT >= 33,
        // Android exposes no stable API for preflighting provider acceptance of external PCM.
        accepted = false,
    )

    override fun supportsPolicy(networkFallback: Boolean, capabilities: SpeechRecognizerAdapter.Capabilities): Boolean {
        var supported = false
        onMain { supported = runCatching { factory.availableForPolicy(networkFallback) }.getOrDefault(false) }
        return supported
    }

    override fun start(sessionId: String, format: CaptureFormat, language: String?, events: SpeechRecognizerAdapter.Events) =
        startWithPolicy(sessionId, format, language, false, events)

    @android.annotation.TargetApi(33)
    override fun startWithPolicy(sessionId: String, format: CaptureFormat, language: String?, networkFallback: Boolean,
                                 events: SpeechRecognizerAdapter.Events) {
        require(Build.VERSION.SDK_INT >= 33) { "external-audio-speech-api-unavailable" }
        require(format in capabilities().formats) { "unsupported-external-audio-format" }
        require(supportsPolicy(networkFallback)) { "external-audio-provider-unavailable" }
        check(context.checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED) {
            "speech-recognizer-requires-record-audio-permission"
        }
        onMain {
            if (!events.isCurrent()) return@onMain
            active.entries.filter { it.value.retiring.get() || !it.value.events.isCurrent() }.forEach { dispose(it.key, it.value) }
            check(active.isEmpty()) { "recognition-session-already-active" }
            val probeRetirement = PhoneCaptureCoordinator.probeRetirementObserver(sessionId)
            val (readEnd, writeEnd) = ParcelFileDescriptor.createPipe()
            var allocated: Active? = null
            try {
            val output = ParcelFileDescriptor.AutoCloseOutputStream(writeEnd)
            val recognizer = factory.createSuppliedRecognizer(networkFallback)
            val current = Active(recognizer, readEnd, output, events, probeRetirement)
            allocated = current
            active[sessionId] = current
            check(probeRetirement?.allocated(current) != false) { "probe-recognition-owner-not-current" }
            startWriter(sessionId, current)
            recognizer.setRecognitionListener(object : AssistantSegmentedListener() {
                private var finalSent = false
                private val transcript = SegmentedTranscriptAssembler()

                override fun onReadyForSpeech(params: android.os.Bundle?) {
                    if (owns(sessionId, current)) events.onAttemptState("provider-ready")
                }
                override fun onBeginningOfSpeech() {
                    if (owns(sessionId, current)) events.onAttemptState("provider-speech-begin")
                }
                override fun onRmsChanged(rmsdB: Float) = Unit
                override fun onBufferReceived(buffer: ByteArray?) = Unit
                override fun onEndOfSpeech() {
                    if (owns(sessionId, current)) events.onAttemptState("provider-speech-end")
                }
                override fun onEvent(eventType: Int, params: android.os.Bundle?) = Unit
                override fun onPartialResults(partialResults: android.os.Bundle?) {
                    if (finalSent || !owns(sessionId, current)) return
                    events.onAttemptState("provider-partial-callback")
                    val text = partialResults?.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)?.firstOrNull() ?: return
                    if (text.isNotBlank()) events.onAttemptState("provider-first-partial")
                    else events.onAttemptState("provider-empty-partial")
                    events.onHypothesis(transcript.hypothesis(text), false)
                }
                override fun segment(segmentResults: android.os.Bundle?) {
                    if (finalSent || !owns(sessionId, current)) return
                    events.onAttemptState("provider-segment-callback")
                    val segment = segmentResults?.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)?.firstOrNull() ?: return
                    if (segment.isNotBlank()) {
                        events.onAttemptState("provider-first-segment")
                        events.onHypothesis(transcript.commit(segment), false)
                    } else events.onAttemptState("provider-empty-segment")
                }
                override fun segmentedEnd() {
                    if (finalSent || !owns(sessionId, current)) return
                    finalSent = true
                    events.onAttemptState("provider-segmented-end")
                    if (!current.completion.canAcceptFinal()) {
                        events.onFailure("speech-provider-ended-before-source")
                        retireAfterCallback(sessionId, current)
                        return
                    }
                    events.onHypothesis(transcript.finish(""), true)
                    retireAfterCallback(sessionId, current)
                }
                override fun onResults(results: android.os.Bundle?) {
                    if (finalSent || !owns(sessionId, current)) return
                    finalSent = true
                    events.onAttemptState("provider-final")
                    if (!current.completion.canAcceptFinal()) {
                        events.onFailure("speech-provider-ended-before-source")
                        retireAfterCallback(sessionId, current)
                        return
                    }
                    val text = results?.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)?.firstOrNull().orEmpty()
                    events.onHypothesis(transcript.finish(text), true)
                    retireAfterCallback(sessionId, current)
                }
                override fun onError(error: Int) {
                    if (finalSent || !owns(sessionId, current)) return
                    finalSent = true
                    events.onFailure("speech-recognizer-$error")
                    retireAfterCallback(sessionId, current)
                }
            })
            val intent = Intent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH).apply {
                putExtra(RecognizerIntent.EXTRA_LANGUAGE_MODEL, RecognizerIntent.LANGUAGE_MODEL_FREE_FORM)
                putExtra(RecognizerIntent.EXTRA_PARTIAL_RESULTS, true)
                putExtra(RecognizerIntent.EXTRA_LANGUAGE, language?.takeIf(String::isNotBlank) ?: Locale.getDefault().toLanguageTag())
                putExtra(RecognizerIntent.EXTRA_PREFER_OFFLINE, true)
                putExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE, readEnd)
                putExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE_CHANNEL_COUNT, format.channels)
                putExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE_ENCODING, android.media.AudioFormat.ENCODING_PCM_16BIT)
                putExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE_SAMPLING_RATE, format.sampleRate)
                putExtra(RecognizerIntent.EXTRA_SEGMENTED_SESSION, RecognizerIntent.EXTRA_AUDIO_SOURCE)
            }
            recognizer.startListening(intent)
            } catch (failure: Throwable) {
                allocated?.let { runCatching { dispose(sessionId, it) } }
                runCatching { readEnd.close() }
                runCatching { writeEnd.close() }
                events.onFailure("external-audio-rejected")
            }
        }
    }

    override fun append(sessionId: String, pcm: ByteArray) {
        require(pcm.size % 2 == 0) { "unaligned-pcm-frame" }
        val current = active[sessionId] ?: return
        appendCurrent(sessionId, current, pcm)
    }

    override fun appendOwned(sessionId: String, owner: SpeechRecognizerAdapter.Events, pcm: ByteArray): Boolean {
        val current = active[sessionId] ?: return false
        if (current.events !== owner || !owns(sessionId, current)) return false
        appendCurrent(sessionId, current, pcm)
        return true
    }

    private fun appendCurrent(sessionId: String, current: Active, pcm: ByteArray) {
        require(pcm.size % 2 == 0) { "unaligned-pcm-frame" }
        check(!current.closed.get()) { "recognition-session-closed" }
        if (!current.chunks.offer(PcmChunk(pcm.copyOf(), SystemClock.elapsedRealtime()), 250, TimeUnit.MILLISECONDS)) {
            current.events.onFailure("external-audio-provider-backpressure")
            onMain { dispose(sessionId, current) }
            throw IllegalStateException("external-audio-provider-backpressure")
        }
    }

    override fun finish(sessionId: String) {
        active[sessionId]?.closed?.set(true)
        // The writer drains queued frames before closing the pipe, which signals source EOF.
    }

    override fun finishOwned(sessionId: String, owner: SpeechRecognizerAdapter.Events): Boolean {
        val current = active[sessionId] ?: return false
        if (current.events !== owner || !owns(sessionId, current)) return false
        current.closed.set(true)
        return true
    }

    override fun cancelOwned(sessionId: String, owner: SpeechRecognizerAdapter.Events) = onMain {
        val current = active[sessionId]
        if (current?.events === owner) dispose(sessionId, current)
    }

    override fun cancel(sessionId: String) = onMain { dispose(sessionId) }

    private fun owns(sessionId: String, current: Active) =
        active[sessionId] === current && !current.retiring.get() && current.events.isCurrent()

    private fun retireAfterCallback(sessionId: String, current: Active) {
        if (current.retiring.get()) return
        runCatching { dispose(sessionId, current) }
            .onFailure { current.events.onFailure("speech-recognizer-retirement-failed") }
    }

    private fun dispose(sessionId: String, current: Active? = active[sessionId]) {
        if (current == null || active[sessionId] !== current) return
        current.retiring.set(true)
        current.closed.set(true)
        current.chunks.clear()
        runCatching { current.output.close() }
        runCatching { current.readEnd.close() }
        runCatching { current.recognizer.cancel() }
        try { destroyRecognizer(current.recognizer) }
        catch (failure: Throwable) {
            current.probeRetirement?.failed(current) {
                runCatching { onMain { dispose(sessionId, current) } }.isSuccess
            }
            throw failure
        }
        active.remove(sessionId, current)
        current.probeRetirement?.retired(current)
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
            var firstWrite = true
            try {
                while (true) {
                    val next = current.chunks.poll(100, TimeUnit.MILLISECONDS)
                    if (next == null) {
                        if (current.closed.get()) break
                        continue
                    }
                    val started = SystemClock.elapsedRealtime()
                    if (started - next.queuedAt > 1_000) current.events.onAttemptState("provider-input-backlog-over-1s")
                    current.output.write(next.bytes)
                    if (SystemClock.elapsedRealtime() - started > 250) current.events.onAttemptState("provider-write-blocked-250ms")
                    if (firstWrite && owns(sessionId, current)) {
                        firstWrite = false
                        current.events.onAttemptState("provider-pipe-first-write")
                    }
                }
                current.completion.closeAfterDrain {
                    current.output.flush()
                    current.output.close()
                }
            } catch (failure: Exception) {
                if (owns(sessionId, current) && !current.completion.canAcceptFinal()) {
                    current.events.onFailure("external-audio-write-failed")
                    main.post { retireAfterCallback(sessionId, current) }
                }
            }
        }, "capture-audio-$sessionId").apply { isDaemon = true; start() }
    }

}
