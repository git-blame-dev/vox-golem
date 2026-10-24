package dev.gitblame.voxgolem.benchmark.android

import android.content.Context
import android.content.Intent
import android.media.AudioFormat
import android.os.ParcelFileDescriptor
import android.speech.RecognizerIntent
import dev.gitblame.voxgolem.assistant.AssistantTiming
import dev.gitblame.voxgolem.assistant.recognition.*
import java.util.concurrent.atomic.AtomicBoolean

/** Finite test-owned source. There is deliberately no AudioRecord or microphone fallback here. */
interface FiniteFixtureInput : PreRollAudioSource {
    val eof: Boolean
    val writerRetired: Boolean
}

class FiniteFixtureAudio(private val pcm: ByteArray) : FiniteFixtureInput {
    private val pipe = ParcelFileDescriptor.createPipe()
    override val readDescriptor: ParcelFileDescriptor = pipe[0]
    private val output = ParcelFileDescriptor.AutoCloseOutputStream(pipe[1])
    private val closed = AtomicBoolean()
    private val started = AtomicBoolean()
    @Volatile override var eof = false
        private set
    @Volatile override var writerRetired = false
        private set
    override val isRecording: Boolean get() = !closed.get()

    @android.annotation.TargetApi(33)
    override fun configureIntent(intent: Intent): Intent {
        check(!closed.get() && readDescriptor.fileDescriptor.valid())
        return intent.putExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE, readDescriptor)
            .putExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE_CHANNEL_COUNT, 1)
            .putExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE_ENCODING, AudioFormat.ENCODING_PCM_16BIT)
            .putExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE_SAMPLING_RATE, 16_000)
    }

    override fun startStreaming() {
        check(!closed.get() && started.compareAndSet(false, true))
        val worker = Thread({
            try {
                output.use {
                    var offset = 0
                    while (!closed.get() && offset < pcm.size) {
                        val count = minOf(640, pcm.size - offset)
                        it.write(pcm, offset, count)
                        offset += count
                        // Representative real-time supplied input, not an unbounded burst/benchmark mode.
                        Thread.sleep(20)
                    }
                    if (!closed.get() && offset == pcm.size) eof = true
                }
            } catch (_: Exception) {
                // A failed/aborted pipe can never be promoted into authoritative final text.
                eof = false
            } finally { writerRetired = true }
        }, "synthetic-ask-pcm").apply { isDaemon = true }
        try { worker.start() } catch (failure: RuntimeException) { writerRetired = true; close(); throw failure }
    }

    // Finite PCM owns EOF; an early provider endpoint cannot substitute a live microphone.
    override fun stopCapture() = Unit
    override fun close() {
        if (!closed.compareAndSet(false, true)) return
        runCatching { readDescriptor.close() }
        runCatching { output.close() }
        if (!started.get()) writerRetired = true
    }
}

/** Wraps real Google support/start/callbacks, rejecting missing pipes before SDK startListening. */
class FixtureRecognizerFactory(
    context: Context,
    private val pcm: ByteArray,
    private val contract: BenchmarkVoiceContract,
    private val onFinal: () -> Unit,
    private val onRejected: () -> Unit,
    private val onObservedFinal: (Boolean, Boolean, Int) -> Unit = { _, _, _ -> },
) : LocalRecognizerFactory {
    private var source: FiniteFixtureAudio? = null
    private var opened = false
    private val real = AndroidLocalRecognizerFactory(context, allowProviderFallback = false,
        preRollFactory = PreRollAudioSourceFactory {
            check(!opened)
            opened = true
            FiniteFixtureAudio(pcm).also { source = it }
        }, strictGoogleOnly = true)
    override fun available() = real.available()
    override fun localOnlyAvailable() = real.localOnlyAvailable()
    override fun availableForPolicy(networkFallback: Boolean) = real.availableForPolicy(false)
    override fun create(): LocalRecognizerSession = wrap(real.createForPolicy(false))
    override fun create(timing: AssistantTiming) = wrap(real.createForPolicy(false, timing))
    override fun createForPolicy(networkFallback: Boolean, timing: AssistantTiming) = create(timing)
    fun retired() = source?.writerRetired != false
    fun close() { source?.close() }

    private fun wrap(delegate: LocalRecognizerSession): LocalRecognizerSession =
        FixtureRecognizerSession(delegate, { source }, contract, onFinal, onRejected, onObservedFinal)
}

class FixtureRecognizerSession(
    private val delegate: LocalRecognizerSession,
    private val source: () -> FiniteFixtureInput?,
    private val contract: BenchmarkVoiceContract,
    private val onFinal: () -> Unit,
    private val onRejected: () -> Unit,
    private val onObservedFinal: (Boolean, Boolean, Int) -> Unit = { _, _, _ -> },
) : LocalRecognizerSession by delegate {
    override fun start(language: String, events: LocalRecognizerEvents) {
        val supplied = source()
        // The production factory catches open failures; this fence prevents its direct-mic branch.
        check(supplied != null && supplied.isRecording && supplied.readDescriptor.fileDescriptor.valid())
        var terminal = false
        delegate.start(language, object : LocalRecognizerEvents {
            override fun ready() = events.ready()
            override fun partial(text: String) {
                if (!terminal && contract.isPrefix(text)) events.partial(text)
            }
            override fun final(text: String) {
                if (terminal) return
                terminal = true
                val matched = contract.matches(text)
                val eof = supplied.eof
                onObservedFinal(matched, eof, contract.wordCount(text))
                if (eof && matched) {
                    onFinal()
                    events.final(text)
                } else {
                    onRejected()
                    events.error("synthetic_input_rejected")
                }
            }
            override fun error(code: String) { if (!terminal) { terminal = true; events.error(code) } }
        })
    }
}
