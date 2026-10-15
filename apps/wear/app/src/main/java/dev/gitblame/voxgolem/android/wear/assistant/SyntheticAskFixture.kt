package dev.gitblame.voxgolem.android.wear.assistant

import android.app.Instrumentation
import android.content.Context
import android.content.ContextWrapper
import android.content.Intent
import android.content.ServiceConnection
import android.os.Bundle
import android.speech.tts.TextToSpeech
import android.speech.tts.UtteranceProgressListener
import dev.gitblame.voxgolem.capture.WavPcmReader
import java.io.ByteArrayOutputStream
import java.io.File
import java.util.Locale
import java.util.UUID
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executor
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.math.roundToInt

/** Generated once before trials, from an already installed local Google voice; never requests downloads. */
@android.annotation.TargetApi(34)
internal object SyntheticAskFixture {
    fun create(instrumentation: Instrumentation, context: Context): ByteArray {
        val file = File(context.cacheDir, "synthetic-ask-fixture-${UUID.randomUUID()}.wav")
        val done = CountDownLatch(1)
        val succeeded = AtomicBoolean()
        val closed = AtomicBoolean()
        var engine: TextToSpeech? = null
        val googleOnly = object : ContextWrapper(context) {
            override fun getApplicationContext(): Context = this
            private fun allowed(intent: Intent) = (intent.component?.packageName ?: intent.`package`) == GOOGLE
            override fun bindService(intent: Intent, connection: ServiceConnection, flags: Int): Boolean =
                allowed(intent) && super.bindService(intent, connection, flags)
            override fun bindService(intent: Intent, flags: Int, executor: Executor, connection: ServiceConnection): Boolean =
                allowed(intent) && super.bindService(intent, flags, executor, connection)
            override fun bindService(intent: Intent, flags: Context.BindServiceFlags, executor: Executor, connection: ServiceConnection): Boolean =
                allowed(intent) && super.bindService(intent, flags, executor, connection)
        }
        try {
            instrumentation.runOnMainSync {
                engine = TextToSpeech(googleOnly, { status ->
                    android.os.Handler(android.os.Looper.getMainLooper()).post {
                        if (closed.get()) return@post
                        val tts = engine
                        if (status != TextToSpeech.SUCCESS || tts == null) { done.countDown(); return@post }
                        val voice = tts.voices?.filter { !it.isNetworkConnectionRequired &&
                            TextToSpeech.Engine.KEY_FEATURE_NOT_INSTALLED !in it.features.orEmpty() }
                            ?.firstOrNull { it.locale == Locale.forLanguageTag("en-AU") }
                        if (voice == null || tts.setVoice(voice) != TextToSpeech.SUCCESS) { done.countDown(); return@post }
                        tts.setOnUtteranceProgressListener(object : UtteranceProgressListener() {
                            override fun onStart(utteranceId: String?) = Unit
                            override fun onDone(utteranceId: String?) { succeeded.set(true); done.countDown() }
                            override fun onError(utteranceId: String?) { done.countDown() }
                            override fun onStop(utteranceId: String?, interrupted: Boolean) { done.countDown() }
                        })
                        if (tts.synthesizeToFile(SyntheticAskContract.PHRASE, Bundle(), file, "synthetic-fixture") != TextToSpeech.SUCCESS)
                            done.countDown()
                    }
                }, GOOGLE)
            }
            check(done.await(45, TimeUnit.SECONDS) && succeeded.get()) { "synthetic_fixture_unavailable" }
            return pcm(file)
        } finally {
            closed.set(true)
            instrumentation.runOnMainSync { engine?.stop(); engine?.shutdown(); engine = null }
            file.delete()
        }
    }

    private fun pcm(file: File): ByteArray = WavPcmReader(file).use { reader ->
        val buffer = ByteArrayOutputStream()
        while (true) {
            val chunk = reader.read(3_200) ?: break
            check(buffer.size() + chunk.size <= 2 * 1024 * 1024) { "synthetic_fixture_too_large" }
            buffer.write(chunk)
        }
        val original = buffer.toByteArray()
        val channels = reader.format.channels
        val frames = original.size / (2 * channels)
        check(reader.format.bitsPerSample == 16 && frames > 0)
        fun sample(frame: Int): Double = (0 until channels).sumOf { channel ->
            val at = (frame.coerceIn(0, frames - 1) * channels + channel) * 2
            ((original[at].toInt() and 255) or (original[at + 1].toInt() shl 8)).toShort().toDouble()
        } / channels
        val outputFrames = (frames.toLong() * 16_000 / reader.format.sampleRate).toInt()
        check(outputFrames in 1..160_000)
        // 200 ms leading / 300 ms trailing silence, all finite and frozen before any measured launch.
        ByteArray((outputFrames + 8_000) * 2).also { output ->
            repeat(outputFrames) { frame ->
                val position = frame.toDouble() * reader.format.sampleRate / 16_000
                val left = position.toInt()
                val value = (sample(left) + (sample(left + 1) - sample(left)) * (position - left))
                    .roundToInt().coerceIn(-32768, 32767)
                output[(frame + 3_200) * 2] = value.toByte()
                output[(frame + 3_200) * 2 + 1] = (value shr 8).toByte()
            }
        }
    }

    private const val GOOGLE = "com.google.android.tts"
}
