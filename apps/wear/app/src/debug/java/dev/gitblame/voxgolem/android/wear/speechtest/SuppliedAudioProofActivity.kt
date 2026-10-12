package dev.gitblame.voxgolem.android.wear.speechtest

import android.app.Activity
import android.content.ComponentName
import android.content.Intent
import android.content.pm.ApplicationInfo
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.speech.RecognitionService
import android.speech.tts.TextToSpeech
import android.speech.tts.UtteranceProgressListener
import android.util.Log
import dev.gitblame.voxgolem.assistant.inputtransport.AndroidLocalSuppliedAudioFactory
import dev.gitblame.voxgolem.assistant.inputtransport.SuppliedAudioProviderGate
import dev.gitblame.voxgolem.assistant.inputtransport.SuppliedInputEvents
import dev.gitblame.voxgolem.assistant.inputtransport.SuppliedInputSession
import dev.gitblame.voxgolem.assistant.recognition.RecognitionResourceLease
import dev.gitblame.voxgolem.capture.WavPcmReader
import java.io.ByteArrayOutputStream
import java.io.File
import java.util.Locale
import java.util.UUID
import kotlin.math.roundToInt

data class SuppliedAudioProofResult(val supportAccepted: Boolean, val sourceRecognized: Boolean,
    val finalAfterEof: Boolean, val partialObserved: Boolean, val retired: Boolean, val code: String)

/** Main-thread ownership survives result delivery until the supplied session actually closes. */
internal class SuppliedAudioProofRetirement(
    private val lease: RecognitionResourceLease = RecognitionResourceLease.process,
) {
    @Volatile var completed = false
        private set
    private var input: SuppliedInputSession? = null
    private var token: RecognitionResourceLease.Token? = null

    fun acquire(): Boolean {
        checkMain()
        check(!completed && token == null)
        token = lease.tryAcquire(RecognitionResourceLease.Kind.CAPTURE_PROBE)
        return token != null
    }

    fun attach(session: SuppliedInputSession) {
        checkMain()
        check(!completed && token != null && input == null)
        input = session
    }

    fun settle(deliver: (Boolean) -> Unit) {
        checkMain()
        if (completed) return
        completed = true
        deliver(retryRetirement())
    }

    fun retryRetirement(): Boolean {
        checkMain()
        check(completed)
        if (runCatching { input?.close() }.isFailure) {
            token?.onRetirementRetry {
                // Off-main callers cannot touch the SDK; they must retry from main explicitly.
                Looper.myLooper() == Looper.getMainLooper() && retryRetirement()
            }
            return false
        }
        input = null
        val retiredToken = token
        token = null
        retiredToken?.close()
        return true
    }

    private fun checkMain() = check(Looper.myLooper() == Looper.getMainLooper())
}

/** Isolated calibration only. Never grants production approval or opens an AudioRecord. */
class SuppliedAudioProofActivity : Activity() {
    private val main = Handler(Looper.getMainLooper())
    private var tts: TextToSpeech? = null
    private val retirement = SuppliedAudioProofRetirement()
    private var syntheticFile: File? = null
    private val completed get() = retirement.completed
    @Volatile private var sourceEnded = false
    private var supportAccepted = false
    private var partialObserved = false
    private var finished: ((SuppliedAudioProofResult) -> Unit)? = null
    private var timeout: Runnable? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        check(packageName.endsWith(".inputproof") && applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE != 0) {
            "Supplied-audio calibration requires a separately installed debug inputproof package"
        }
    }

    fun probe(language: String, ordinaryProviderPackage: String, strict: Boolean, cancel: Boolean = false,
        onFinished: (SuppliedAudioProofResult) -> Unit) {
        check(Looper.myLooper() == Looper.getMainLooper() && finished == null && !completed)
        finished = onFinished
        timeout = Runnable { settle(false, "proof_timeout") }.also { main.postDelayed(it, 30_000) }
        val component = if (strict) null else packageManager.queryIntentServices(Intent(RecognitionService.SERVICE_INTERFACE), 0)
            .firstOrNull { it.serviceInfo.packageName == ordinaryProviderPackage }?.serviceInfo?.let { ComponentName(it.packageName, it.name) }
        if (!strict && component == null) { settle(false, "provider_unavailable"); return }
        // This opt-in is restricted to this isolated probe, not evidence of provider support.
        val factory = AndroidLocalSuppliedAudioFactory(this, component, strict, !strict, SuppliedAudioProviderGate { true })
        if (!factory.available(language)) { settle(false, "route_unavailable"); return }
        component?.let {
            val version = packageManager.getPackageInfo(it.packageName, 0).versionName.orEmpty()
            Log.i(TAG, "provider=${it.flattenToShortString()} version=$version")
        }
        val file = File(cacheDir, "synthetic-input-${UUID.randomUUID()}.wav")
        syntheticFile = file
        if (!retirement.acquire()) { settle(false, "recognition_busy"); return }
        try { tts = TextToSpeech(this) { status -> main.post {
            if (completed) return@post
            val engine = tts ?: return@post
            if (status != TextToSpeech.SUCCESS) { settle(false, "synthetic_tts_unavailable"); return@post }
            val requested = Locale.forLanguageTag(language)
            val voice = engine.voices?.firstOrNull { it.locale == requested && !it.isNetworkConnectionRequired &&
                !it.features.orEmpty().contains(TextToSpeech.Engine.KEY_FEATURE_NOT_INSTALLED) }
                ?: engine.voices?.firstOrNull { it.locale.language == requested.language && !it.isNetworkConnectionRequired &&
                    !it.features.orEmpty().contains(TextToSpeech.Engine.KEY_FEATURE_NOT_INSTALLED) }
            if (voice == null || engine.setVoice(voice) != TextToSpeech.SUCCESS) { settle(false, "synthetic_local_voice_unavailable"); return@post }
            engine.setOnUtteranceProgressListener(object : UtteranceProgressListener() {
                override fun onStart(utteranceId: String?) {}
                override fun onError(utteranceId: String?) { main.post { settle(false, "synthetic_tts_failed") } }
                override fun onDone(utteranceId: String?) {
                    Thread({
                        val pcm = runCatching { fixturePcm(file) }
                        file.delete()
                        main.post {
                            if (completed) return@post
                            pcm.fold({ begin(factory, language, it, cancel) }, { settle(false, "synthetic_wav_invalid") })
                        }
                    }, "synthetic-input-fixture").apply { isDaemon = true; start() }
                }
            })
            if (engine.synthesizeToFile(PHRASE, Bundle(), file, "synthetic-input") != TextToSpeech.SUCCESS)
                settle(false, "synthetic_tts_failed")
        } } } catch (_: Exception) { settle(false, "synthetic_tts_failed") }
    }

    private fun begin(factory: AndroidLocalSuppliedAudioFactory, language: String, pcm: ByteArray, cancel: Boolean) {
        runCatching { tts?.shutdown() }; tts = null
        try {
            val session = factory.open(language); retirement.attach(session)
            session.checkSupport { supported ->
                if (completed) return@checkSupport
                supportAccepted = supported
                if (!supported) { settle(false, "support_rejected"); return@checkSupport }
                session.start(object : SuppliedInputEvents {
                    override fun partial(text: String) { if (text.isNotBlank()) partialObserved = true }
                    override fun final(text: String) {
                        // Do not log hypotheses or final text, including unexpectedly captured ambient speech.
                        val tokens = text.lowercase(Locale.ROOT).replace(Regex("[^a-z0-9 ]"), "").split(Regex("\\s+"))
                        settle(listOf("purple", "lantern").all(tokens::contains) && ("seven" in tokens || "7" in tokens), "final")
                    }
                    override fun error(code: String) { settle(false, if (code.matches(Regex("recognition_error_[0-9]+"))) code else "supplied_input_failed") }
                })
                if (cancel) { main.postDelayed({ settle(false, "cancelled") }, 100) }
                Thread({
                    var offset = 0
                    while (!completed && offset < pcm.size) {
                        val count = minOf(640, pcm.size - offset)
                        if (!session.appendFromWorker(pcm.copyOfRange(offset, offset + count))) {
                            if (!completed) main.post { settle(false, "pipe_backpressure") }
                            return@Thread
                        }
                        offset += count
                        Thread.sleep(20)
                    }
                    main.post { if (!completed) { sourceEnded = true; session.finish() } }
                }, "synthetic-input-source").apply { isDaemon = true; start() }
            }
        } catch (_: Exception) { settle(false, "sdk_setup_failed") }
    }

    private fun settle(recognized: Boolean, code: String) {
        retirement.settle { retired ->
            timeout?.let(main::removeCallbacks); timeout = null
            runCatching { tts?.shutdown() }; tts = null
            syntheticFile?.delete(); syntheticFile = null
            val result = SuppliedAudioProofResult(supportAccepted, recognized, sourceEnded && code == "final", partialObserved, retired, code)
            Log.i(TAG, "support=${result.supportAccepted} matched=${result.sourceRecognized} eof=${result.finalAfterEof} partial=${result.partialObserved} retired=$retired code=$code micIsolationProven=false")
            val callback = finished; finished = null
            callback?.invoke(result)
        }
    }
    override fun onStop() {
        if (completed) retirement.retryRetirement() else settle(false, "foreground_lost")
        super.onStop()
    }

    companion object {
        private const val TAG = "VoxGolemInputProof"
        private const val PHRASE = "purple lantern seven"
        /** Fixture-only mono/rate conversion; production recording still uses the exact native PCM16/16k format. */
        private fun fixturePcm(file: File): ByteArray = WavPcmReader(file).use { reader ->
            val bytes = ByteArrayOutputStream()
            while (true) {
                val chunk = reader.read(3_200) ?: break
                check(bytes.size() + chunk.size <= 2 * 1024 * 1024) { "synthetic-fixture-too-large" }
                bytes.write(chunk)
            }
            val original = bytes.toByteArray()
            val channels = reader.format.channels
            val frames = original.size / (2 * channels)
            check(frames > 0)
            fun sample(frame: Int): Double = (0 until channels).sumOf { channel ->
                val at = (frame.coerceIn(0, frames - 1) * channels + channel) * 2
                ((original[at].toInt() and 255) or (original[at + 1].toInt() shl 8)).toShort().toDouble()
            } / channels
            val outputFrames = (frames.toLong() * 16_000 / reader.format.sampleRate).toInt()
            ByteArray(outputFrames * 2).also { output ->
                repeat(outputFrames) { frame ->
                    val position = frame.toDouble() * reader.format.sampleRate / 16_000
                    val left = position.toInt()
                    val value = (sample(left) + (sample(left + 1) - sample(left)) * (position - left)).roundToInt().coerceIn(-32768, 32767)
                    output[frame * 2] = value.toByte(); output[frame * 2 + 1] = (value shr 8).toByte()
                }
            }
        }
    }
}
