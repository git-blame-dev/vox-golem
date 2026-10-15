package dev.gitblame.voxgolem.assistant.recognition

import android.Manifest
import android.annotation.SuppressLint
import android.annotation.TargetApi
import android.content.Context
import android.content.Intent
import android.content.SharedPreferences
import android.content.pm.PackageManager
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.os.Build
import android.os.ParcelFileDescriptor
import android.speech.RecognizerIntent
import dev.gitblame.voxgolem.assistant.speech.SpeechServiceRequirement
import org.json.JSONArray
import org.json.JSONObject
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicBoolean

interface PreRollAudioSource : AutoCloseable {
    val readDescriptor: ParcelFileDescriptor
    val isRecording: Boolean
    fun startStreaming()
    fun configureIntent(intent: Intent): Intent
    fun stopCapture()
    override fun close()
}

fun interface PreRollAudioSourceFactory {
    fun open(): PreRollAudioSource?
}

/**
 * Captures 16 kHz mono PCM16 audio from VOICE_RECOGNITION immediately on utterance start,
 * buffering up to 4 seconds in a bounded ring buffer and streaming into an EXTRA_AUDIO_SOURCE
 * pipe. Graceful stop drains buffered audio before EOF; cancellation discards it.
 */
class AndroidPreRollAudioSource private constructor(
    private val audioRecord: AudioRecord,
    override val readDescriptor: ParcelFileDescriptor,
    private val writeDescriptor: ParcelFileDescriptor,
) : PreRollAudioSource {
    private val outputStream = ParcelFileDescriptor.AutoCloseOutputStream(writeDescriptor)
    private val closed = AtomicBoolean(false)
    private val pump = PreRollPcmPump(
        object : PreRollPcmPump.Capture {
            override fun read(buffer: ByteArray) = audioRecord.read(buffer, 0, buffer.size)
            override fun stop() {
                if (audioRecord.recordingState == AudioRecord.RECORDSTATE_RECORDING) audioRecord.stop()
            }
            override fun release() = audioRecord.release()
        }, outputStream, FRAME_BYTES, MAX_PRE_ROLL_FRAMES,
    )

    override val isRecording: Boolean get() = pump.isRecording && !closed.get()

    override fun startStreaming() {
        pump.startStreaming()
    }

    @TargetApi(33)
    override fun configureIntent(intent: Intent): Intent = intent
        .putExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE, readDescriptor)
        .putExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE_CHANNEL_COUNT, 1)
        .putExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE_ENCODING, AUDIO_FORMAT)
        .putExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE_SAMPLING_RATE, SAMPLE_RATE)

    override fun stopCapture() {
        pump.stopCapture()
    }

    override fun close() {
        if (closed.get()) return
        // Closing the consumer first also unblocks a writer whose recognizer has retired.
        runCatching { readDescriptor.close() }
        pump.close()
        closed.set(true)
    }

    companion object {
        const val SAMPLE_RATE = 16_000
        const val CHANNEL_CONFIG = AudioFormat.CHANNEL_IN_MONO
        const val AUDIO_FORMAT = AudioFormat.ENCODING_PCM_16BIT
        const val FRAME_BYTES = 640 // 20 ms at 16 kHz PCM16 mono
        const val MAX_PRE_ROLL_FRAMES = 200 // 4 seconds bounded pre-roll ring buffer

        fun defaultFactory(context: Context): PreRollAudioSourceFactory = PreRollAudioSourceFactory {
            if (isRobolectricRuntime()) null else open(context)
        }

        @SuppressLint("MissingPermission")
        fun open(context: Context): AndroidPreRollAudioSource? {
            if (Build.VERSION.SDK_INT < 33) return null
            if (context.checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
                return null
            }
            val minBuffer = runCatching {
                AudioRecord.getMinBufferSize(SAMPLE_RATE, CHANNEL_CONFIG, AUDIO_FORMAT)
            }.getOrDefault(-1)
            if (minBuffer <= 0) return null
            val bufferSize = maxOf(minBuffer * 2, FRAME_BYTES * 10)
            val pipe = runCatching { ParcelFileDescriptor.createPipe() }.getOrNull() ?: return null
            if (pipe.size < 2) return null
            val readPfd = pipe[0]
            val writePfd = pipe[1]
            val record = try {
                AudioRecord(
                    MediaRecorder.AudioSource.VOICE_RECOGNITION,
                    SAMPLE_RATE,
                    CHANNEL_CONFIG,
                    AUDIO_FORMAT,
                    bufferSize,
                )
            } catch (_: Throwable) {
                runCatching { readPfd.close() }
                runCatching { writePfd.close() }
                return null
            }
            if (record.state != AudioRecord.STATE_INITIALIZED) {
                runCatching { record.release() }
                runCatching { readPfd.close() }
                runCatching { writePfd.close() }
                return null
            }
            val started = runCatching {
                record.startRecording()
                record.recordingState == AudioRecord.RECORDSTATE_RECORDING
            }.getOrDefault(false)
            if (!started) {
                runCatching { record.release() }
                runCatching { readPfd.close() }
                runCatching { writePfd.close() }
                return null
            }
            return try {
                AndroidPreRollAudioSource(record, readPfd, writePfd)
            } catch (failure: Throwable) {
                // Capture has already started; constructor/thread-start failure must not leak it
                // into the direct-microphone fallback, which takes the same admission lease.
                runCatching { record.stop() }
                runCatching { record.release() }
                runCatching { readPfd.close() }
                runCatching { writePfd.close() }
                if (failure !is RuntimeException) throw failure
                null
            }
        }

        internal fun isRobolectricRuntime(): Boolean =
            Build.FINGERPRINT == null || Build.FINGERPRINT.equals("robolectric", ignoreCase = true)
    }
}

data class CachedSttSupport(
    val selectedLanguage: String,
    val installedLanguages: List<String>,
    val supportedLanguages: List<String>,
)

/**
 * In-memory and SharedPreferences cache for Google on-device STT language support checks,
 * keyed by (googlePackageVersionCode, normalizedLanguageTag).
 */
class GoogleSttSupportCache(
    context: Context,
    private val versionCodeProvider: () -> Long? = { defaultGoogleVersionCode(context) },
    private val preferencesProvider: () -> SharedPreferences? = {
        runCatching { context.applicationContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE) }.getOrNull()
    },
) {
    private val memory = ConcurrentHashMap<String, CachedSttSupport>()

    private fun key(provider: String, language: String, networkFallback: Boolean): String? {
        val version = versionCodeProvider() ?: return null
        if (version <= 0L) return null
        val normalized = SpeechLocales.parse(language)?.toLanguageTag() ?: language.trim()
        return "v2|$provider|$version|$networkFallback|$normalized"
    }

    fun get(provider: String, language: String, networkFallback: Boolean = true): CachedSttSupport? {
        val cacheKey = key(provider, language, networkFallback) ?: return null
        memory[cacheKey]?.let { return it }
        val prefs = preferencesProvider() ?: return null
        val raw = prefs.getString(cacheKey, null) ?: return null
        return runCatching {
            val json = JSONObject(raw)
            val selected = json.getString("selectedLanguage")
            val installedArr = json.optJSONArray("installedLanguages") ?: JSONArray()
            val supportedArr = json.optJSONArray("supportedLanguages") ?: JSONArray()
            val installed = (0 until installedArr.length()).mapNotNull { installedArr.optString(it).takeIf(String::isNotEmpty) }
            val supported = (0 until supportedArr.length()).mapNotNull { supportedArr.optString(it).takeIf(String::isNotEmpty) }
            CachedSttSupport(selected, installed, supported).also { memory[cacheKey] = it }
        }.getOrNull()
    }

    fun put(provider: String, language: String, entry: CachedSttSupport, networkFallback: Boolean = true) {
        // Regional fallback is usable for this turn, but must not hide a newly installed exact pack.
        if (SpeechLocales.parse(language) != SpeechLocales.parse(entry.selectedLanguage)) return
        val cacheKey = key(provider, language, networkFallback) ?: return
        memory[cacheKey] = entry
        val prefs = preferencesProvider() ?: return
        runCatching {
            val json = JSONObject()
                .put("selectedLanguage", entry.selectedLanguage)
                .put("installedLanguages", JSONArray(entry.installedLanguages))
                .put("supportedLanguages", JSONArray(entry.supportedLanguages))
            prefs.edit().putString(cacheKey, json.toString()).apply()
        }
    }

    fun invalidate(provider: String, language: String) {
        val normalized = SpeechLocales.parse(language)?.toLanguageTag() ?: language.trim()
        val prefix = "v2|$provider|"
        val suffix = "|$normalized"
        memory.keys.removeIf { it.startsWith(prefix) && it.endsWith(suffix) }
        preferencesProvider()?.let { preferences ->
            val editor = preferences.edit()
            preferences.all.keys.filter { it.startsWith(prefix) && it.endsWith(suffix) }.forEach(editor::remove)
            editor.apply()
        }
    }

    fun clear() {
        memory.clear()
        runCatching { preferencesProvider()?.edit()?.clear()?.apply() }
    }

    companion object {
        const val PREFS_NAME = "voxgolem_google_stt_support_cache"

        @Suppress("DEPRECATION")
        fun defaultGoogleVersionCode(context: Context): Long? = runCatching {
            val info = context.packageManager.getPackageInfo(SpeechServiceRequirement.GOOGLE_SPEECH_PACKAGE, 0)
            if (Build.VERSION.SDK_INT >= 28) info.longVersionCode else info.versionCode.toLong()
        }.getOrNull()?.takeIf { it > 0L }
    }
}
