package dev.gitblame.voxgolem.assistant.inputtransport

import android.Manifest
import android.content.Context
import android.content.ComponentName
import android.content.Intent
import android.content.pm.PackageManager
import android.media.AudioFormat
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.ParcelFileDescriptor
import android.speech.RecognitionListener
import android.speech.RecognitionSupport
import android.speech.RecognitionSupportCallback
import android.speech.RecognizerIntent
import android.speech.SpeechRecognizer
import android.system.ErrnoException
import android.system.Os
import android.system.OsConstants
import android.system.StructPollfd
import java.util.Locale
import java.util.concurrent.Executor
import java.util.concurrent.atomic.AtomicBoolean
import java.io.FileDescriptor

/**
 * Public SDK support results do NOT attest EXTRA_AUDIO_SOURCE or absence of phone-mic use.
 * Owner must invalidate this approval on provider/package/OS changes and verify the actual
 * on-device provider, exact language/format/segmentation, offline operation and no phone mic
 * on a physical device. Default is unavailable; this must never be inferred from API level.
 */
fun interface SuppliedAudioDeviceGate { fun permits(language: String): Boolean }

enum class SuppliedAudioMode { SEGMENTED, UTTERANCE }
data class SuppliedAudioRequest(val provider: String, val onDeviceOnly: Boolean, val language: String,
    val networkFallback: Boolean, val mode: SuppliedAudioMode = SuppliedAudioMode.UTTERANCE,
    val sampleRate: Int = 16_000, val channels: Int = 1)

/** Thread-safe approval, invalidated on OS/provider changes; SDK support metadata is not this proof. */
fun interface SuppliedAudioProviderGate {
    fun permits(request: SuppliedAudioRequest): Boolean
    companion object { val UNPROVEN = SuppliedAudioProviderGate { false } }
}

/** SDK-only local input. No peer transport, microphone, history, model download or provider request. */
class AndroidLocalSuppliedAudioFactory(
    context: Context,
    private val component: ComponentName?,
    private val onDeviceOnly: Boolean,
    private val networkFallback: Boolean,
    private val gate: SuppliedAudioProviderGate = SuppliedAudioProviderGate.UNPROVEN,
) : SuppliedInputFactory {
    private val context = context.applicationContext
    fun request(language: String) = SuppliedAudioRequest(component?.flattenToShortString() ?: "android.on-device",
        onDeviceOnly, VoiceAudioContract.language(language), networkFallback)
    override fun available(language: String): Boolean {
        requireInputMain()
        return Build.VERSION.SDK_INT >= 33 && runCatching {
            gate.permits(request(language)) && context.checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED &&
                (if (onDeviceOnly) SpeechRecognizer.isOnDeviceRecognitionAvailable(context) else component != null)
        }.getOrDefault(false)
    }
    @android.annotation.TargetApi(33)
    override fun open(language: String): SuppliedInputSession {
        requireInputMain(); check(available(language)) { "supplied_audio_provider_unproven" }
        val pipe = AndroidInputPipe()
        try {
            val sdk = if (onDeviceOnly) SpeechRecognizer.createOnDeviceSpeechRecognizer(context)
                else SpeechRecognizer.createSpeechRecognizer(context, requireNotNull(component))
            return AndroidSuppliedAudioSession(language, AndroidSuppliedRecognizerSdk(sdk, networkFallback), pipe,
                SuppliedAudioMode.UTTERANCE) { gate.permits(request(language)) }
        } catch (error: Exception) { pipe.close(); throw error }
    }
}

class AndroidSuppliedAudioFactory(
    context: Context,
    private val gate: SuppliedAudioDeviceGate = SuppliedAudioDeviceGate { false },
) : SuppliedInputFactory {
    private val context = context.applicationContext
    override fun available(language: String): Boolean {
        requireInputMain()
        return Build.VERSION.SDK_INT >= 33 && runCatching {
            VoiceAudioContract.language(language)
            gate.permits(language) && context.checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED &&
                SpeechRecognizer.isOnDeviceRecognitionAvailable(context)
        }.getOrDefault(false)
    }
    @android.annotation.TargetApi(33)
    override fun open(language: String): SuppliedInputSession {
        requireInputMain(); check(available(language)) { "supplied-audio-device-gate-unavailable" }
        val pipe = AndroidInputPipe()
        try {
            return AndroidSuppliedAudioSession(language,
                AndroidSuppliedRecognizerSdk(SpeechRecognizer.createOnDeviceSpeechRecognizer(context)), pipe,
                { gate.permits(language) })
        } catch (error: Exception) { pipe.close(); throw error }
    }
}

internal interface SuppliedRecognizerSdk {
    fun listener(listener: RecognitionListener)
    fun support(intent: Intent, result: (List<String>?) -> Unit)
    fun supportError(error: Int) {}
    fun start(intent: Intent)
    fun close()
}
@android.annotation.TargetApi(33)
private class AndroidSuppliedRecognizerSdk(private val sdk: SpeechRecognizer,
    private val networkFallback: Boolean = false) : SuppliedRecognizerSdk {
    private val main = Handler(Looper.getMainLooper())
    private var supportDeadline: Runnable? = null
    private var onSupportError: ((Int) -> Unit)? = null
    override fun listener(listener: RecognitionListener) { sdk.setRecognitionListener(listener) }
    override fun support(intent: Intent, result: (List<String>?) -> Unit) {
        var settled = false
        var cannotCheck = false
        fun settle(languages: List<String>?) {
            if (settled) return
            settled = true; supportDeadline?.let(main::removeCallbacks); supportDeadline = null
            onSupportError = null; result(languages)
        }
        onSupportError = { error -> if (error == SpeechRecognizer.ERROR_CANNOT_CHECK_SUPPORT) cannotCheck = true else settle(null) }
        supportDeadline = Runnable { settle(null) }.also { main.postDelayed(it, 3_000) }
        sdk.checkRecognitionSupport(intent, Executor { main.post(it) }, object : RecognitionSupportCallback {
            override fun onSupportResult(support: RecognitionSupport) {
                val languages = support.installedOnDeviceLanguages + if (networkFallback) support.onlineLanguages else emptyList()
                val requested = intent.getStringExtra(RecognizerIntent.EXTRA_LANGUAGE)
                if (!cannotCheck || languages.any { it.equals(requested, ignoreCase = true) }) settle(languages)
            }
            override fun onError(error: Int) {
                // Some providers report error14 before valid installed-language replies.
                onSupportError?.invoke(error)
            }
        })
    }
    override fun start(intent: Intent) { sdk.startListening(intent) }
    override fun supportError(error: Int) { onSupportError?.invoke(error) }
    override fun close() {
        supportDeadline?.let(main::removeCallbacks); supportDeadline = null
        onSupportError = null
        try { sdk.cancel() } finally { sdk.destroy() }
    }
}

internal enum class InputEofState { OPEN, CLOSING, CLOSED, FAILED }

internal interface InputPipe : AutoCloseable {
    val readEnd: ParcelFileDescriptor
    val sourceClosed: Boolean
    val eofClosing: Boolean get() = false
    val eofState: InputEofState get() = when {
        sourceClosed -> InputEofState.CLOSED
        eofClosing -> InputEofState.CLOSING
        else -> InputEofState.OPEN
    }
    fun onSourceEnd(observer: (Boolean) -> Unit) {}
    fun start(error: () -> Unit)
    fun append(bytes: ByteArray): Boolean
    fun finish()
}

@android.annotation.TargetApi(33)
internal class AndroidSuppliedAudioSession(
    language: String,
    private val sdk: SuppliedRecognizerSdk,
    private val pipe: InputPipe,
    private val mode: SuppliedAudioMode,
    private val gate: () -> Boolean,
) : SuppliedInputSession {
    constructor(language: String, sdk: SuppliedRecognizerSdk, pipe: InputPipe, gate: () -> Boolean) :
        this(language, sdk, pipe, SuppliedAudioMode.SEGMENTED, gate)
    private val main = Handler(Looper.getMainLooper())
    internal val exactIntent = suppliedAudioIntent(language, pipe.readEnd, mode)
    private val languageTag = Locale.forLanguageTag(language).toLanguageTag()
    private var events: SuppliedInputEvents? = null
    @Volatile private var closed = false
    private var supported = false
    private var queried = false
    @Volatile private var started = false
    private var completed = ""
    private var hypothesis = ""
    @Volatile private var terminal = false
    private var pendingFinal: String? = null
    private var pendingTimeout: Runnable? = null
    private var supportFailure: (() -> Unit)? = null
    private fun installListener() {
        requireInputMain()
        sdk.listener(object : AssistantSegmentedListener() {
            override fun onReadyForSpeech(params: Bundle?) { callback { events?.ready() } }
            override fun onBeginningOfSpeech() {}
            override fun onRmsChanged(rmsdB: Float) {}
            override fun onBufferReceived(buffer: ByteArray?) {}
            override fun onEndOfSpeech() { callback { if (mode == SuppliedAudioMode.UTTERANCE) events?.endOfSpeech() } }
            override fun onEvent(eventType: Int, params: Bundle?) {}
            override fun onError(error: Int) {
                dispatch {
                    if (!closed && queried && !started) {
                        sdk.supportError(error)
                        if (error != SpeechRecognizer.ERROR_CANNOT_CHECK_SUPPORT) supportFailure?.invoke()
                    } else callback { fail("recognition_error_$error") }
                }
            }
            override fun onPartialResults(partialResults: Bundle?) { callback {
                if (pendingFinal != null) return@callback
                hypothesis = join(completed, text(partialResults)); events?.partial(hypothesis)
            } }
            override fun segment(segmentResults: Bundle?) { callback {
                if (pendingFinal != null) return@callback
                completed = join(completed, text(segmentResults)); hypothesis = completed; events?.partial(completed)
            } }
            override fun segmentedEnd() { callback { final(completed) } }
            override fun onResults(results: Bundle?) { callback {
                val result = text(results)
                final(if (result.isBlank()) completed else result)
            } }
        })
    }
    override fun checkSupport(result: (Boolean) -> Unit) {
        requireInputMain(); check(!closed && !started && !queried)
        queried = true
        var answered = false
        supportFailure = { if (!closed && !answered) { answered = true; supported = false; result(false) } }
        // Install after the controller owns this session so setup errors have an owned retirement path.
        installListener()
        if (answered) return
        sdk.support(exactIntent) { installed ->
            dispatch {
                if (closed || answered) return@dispatch
                answered = true
                supportFailure = null
                supported = runCatching(gate).getOrDefault(false) && installed?.any { Locale.forLanguageTag(it).toLanguageTag() == languageTag } == true
                result(supported)
            }
        }
    }
    override fun start(events: SuppliedInputEvents) {
        requireInputMain(); check(!closed && !started && supported && gate())
        this.events = events; started = true
        pipe.onSourceEnd { success -> dispatch {
            if (!closed && !terminal && pendingFinal != null) {
                if (!success) fail("audio_pipe_failed")
                else callback { final(pendingFinal!!) }
            }
        } }
        pipe.start { dispatch { if (!closed && !terminal) fail("audio_pipe_failed") } }
        sdk.start(exactIntent) // Same live descriptor and all extras as the support query.
    }
    override fun append(pcm: ByteArray): Boolean { requireInputMain(); return !closed && started && !terminal && gate() && pipe.append(pcm) }
    override fun appendFromWorker(pcm: ByteArray): Boolean = !closed && started && !terminal && gate() && pipe.append(pcm)
    override fun finish() { requireInputMain(); check(!closed && started); pipe.finish() }
    override fun close() {
        requireInputMain(); closed = true; events = null
        supportFailure = null
        clearPending()
        // Repeated close retries failed teardown; the controller retains CHAT admission until success.
        try { pipe.close() } finally { sdk.close() }
    }
    private fun callback(action: () -> Unit) { dispatch {
        if (!closed && !terminal && started) {
            if (runCatching(gate).getOrDefault(false)) action() else fail("supplied_audio_approval_lost")
        }
    } }
    private fun dispatch(action: () -> Unit) {
        if (Looper.myLooper() == Looper.getMainLooper()) action() else main.post(action)
    }
    private fun clearPending() {
        pendingTimeout?.let { main.removeCallbacks(it) }; pendingTimeout = null; pendingFinal = null
    }
    private fun fail(code: String) { clearPending(); terminal = true; events?.error(code) }
    private fun final(value: String) {
        // One atomic pipe-state snapshot avoids racing separate closing/closed flag reads.
        when (pipe.eofState) {
            InputEofState.OPEN -> {
                if (mode != SuppliedAudioMode.UTTERANCE) { fail("provider_ended_before_source"); return }
                if (pendingFinal != null) return
                if (value.toByteArray(Charsets.UTF_8).size > VoiceAudioContract.MAX_TEXT_BYTES) { fail("transcript_limit"); return }
                pendingFinal = value
                events?.endOfSpeech()
                pendingTimeout = Runnable { if (!closed && !terminal && pendingFinal != null) fail("source_eof_timeout") }
                    .also { main.postDelayed(it, 3_000) }
                return
            }
            InputEofState.FAILED -> { fail("audio_pipe_failed"); return }
            InputEofState.CLOSING -> {
                if (pendingFinal != null) return
                if (value.toByteArray(Charsets.UTF_8).size > VoiceAudioContract.MAX_TEXT_BYTES) { fail("transcript_limit"); return }
                pendingFinal = value
                pendingTimeout = Runnable {
                    if (!closed && !terminal && pendingFinal != null) fail("source_eof_timeout")
                }.also { main.postDelayed(it, 250) }
                return
            }
            InputEofState.CLOSED -> Unit
        }
        val finalValue = pendingFinal ?: value
        if (finalValue.isBlank()) { fail("recognition_empty_final"); return }
        clearPending(); terminal = true; events?.final(finalValue)
    }
    private fun join(left: String, right: String) = listOf(left, right).filter(String::isNotBlank).joinToString(" ")
    private fun text(bundle: Bundle?) = bundle?.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)?.firstOrNull().orEmpty()
}

@android.annotation.TargetApi(33)
internal fun suppliedAudioIntent(language: String, source: ParcelFileDescriptor,
    mode: SuppliedAudioMode = SuppliedAudioMode.SEGMENTED) =
    Intent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH)
        .putExtra(RecognizerIntent.EXTRA_LANGUAGE_MODEL, RecognizerIntent.LANGUAGE_MODEL_FREE_FORM)
        .putExtra(RecognizerIntent.EXTRA_LANGUAGE, VoiceAudioContract.language(language))
        .putExtra(RecognizerIntent.EXTRA_PARTIAL_RESULTS, true)
        .putExtra(RecognizerIntent.EXTRA_PREFER_OFFLINE, true)
        .putExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE, source)
        .putExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE_CHANNEL_COUNT, 1)
        .putExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE_ENCODING, AudioFormat.ENCODING_PCM_16BIT)
        .putExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE_SAMPLING_RATE, VoiceAudioContract.SAMPLE_RATE)
        .apply { if (mode == SuppliedAudioMode.SEGMENTED)
            putExtra(RecognizerIntent.EXTRA_SEGMENTED_SESSION, RecognizerIntent.EXTRA_AUDIO_SOURCE) }

fun requireInputMain() { check(Looper.myLooper() == Looper.getMainLooper()) { "assistant-input-main-thread-required" } }

/** Nonblocking pipe writes have a finite budget even if a provider never consumes PCM. */
internal class AndroidInputPipe(
    private val closeWrite: (ParcelFileDescriptor) -> Unit = { it.close() },
    configureWrite: (FileDescriptor) -> Unit = {
        if (Build.VERSION.SDK_INT >= 30) Os.fcntlInt(it, OsConstants.F_SETFL, OsConstants.O_NONBLOCK)
        else throw UnsupportedOperationException("supplied_audio_pipe_unavailable")
    },
) : InputPipe {
    private val descriptors = ParcelFileDescriptor.createPipe()
    override val readEnd = descriptors[0]
    private val writeEnd = descriptors[1]
    private val queue = BoundedPcmQueue()
    private val closing = AtomicBoolean(false)
    @Volatile override var eofState = InputEofState.OPEN
        private set
    override val sourceClosed get() = eofState == InputEofState.CLOSED
    override val eofClosing get() = eofState == InputEofState.CLOSING
    private var worker: Thread? = null
    private var sourceEnd: (Boolean) -> Unit = {}
    init {
        try { configureWrite(writeEnd.fileDescriptor) }
        catch (failure: Exception) { readEnd.close(); writeEnd.close(); throw failure }
    }
    override fun onSourceEnd(observer: (Boolean) -> Unit) { check(worker == null); sourceEnd = observer }
    override fun start(error: () -> Unit) {
        check(worker == null)
        worker = Thread({
            try {
                while (!closing.get()) {
                    val bytes = queue.poll(50)
                    if (bytes == null) { if (queue.drained()) break else continue }
                    write(bytes)
                }
                if (!closing.get()) {
                    eofState = InputEofState.CLOSING
                    closeWrite(writeEnd)
                    eofState = InputEofState.CLOSED
                    sourceEnd(true)
                }
            } catch (_: Exception) {
                eofState = InputEofState.FAILED
                if (!closing.get()) { sourceEnd(false); error() }
            }
        }, "assistant-input-pipe").apply { isDaemon = true; start() }
    }
    private fun write(bytes: ByteArray) {
        val deadline = System.nanoTime() + 250000000L
        var offset = 0
        while (offset < bytes.size && !closing.get()) {
            check(System.nanoTime() < deadline) { "audio-pipe-backpressure" }
            val poll = StructPollfd().apply { fd = writeEnd.fileDescriptor; events = OsConstants.POLLOUT.toShort() }
            if (Os.poll(arrayOf(poll), 25) == 0) continue
            try { offset += Os.write(writeEnd.fileDescriptor, bytes, offset, bytes.size - offset) }
            catch (error: ErrnoException) { if (error.errno != OsConstants.EAGAIN) throw error }
        }
    }
    override fun append(bytes: ByteArray) = queue.offer(bytes)
    override fun finish() { queue.finish() }
    override fun close() {
        closing.set(true); queue.cancel()
        worker?.interrupt(); worker?.join(500)
        check(worker?.isAlive != true) { "audio-pipe-retirement-failed" }
        try { writeEnd.close() } finally { readEnd.close() }
    }
}
