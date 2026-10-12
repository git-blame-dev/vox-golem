package dev.gitblame.voxgolem.android.wear

import android.content.Context
import android.content.Intent
import android.media.AudioAttributes
import android.os.Handler
import android.os.Build
import android.os.Looper
import android.os.ParcelFileDescriptor
import android.speech.RecognitionSupport
import android.speech.RecognitionSupportCallback
import android.speech.RecognizerIntent
import android.speech.SpeechRecognizer
import android.speech.tts.TextToSpeech
import android.util.Log
import java.util.Locale
import java.util.concurrent.atomic.AtomicBoolean
import org.json.JSONObject
import org.json.JSONArray

/** Availability/support inspection only. Never starts recognition or synthesizes speech. */
@android.annotation.TargetApi(33)
class StandaloneCapabilityProbe(private val context: Context) {
    private val main = Handler(Looper.getMainLooper())
    private val speechDone = AtomicBoolean(false)
    private val ttsDone = AtomicBoolean(false)
    private var recognizer: SpeechRecognizer? = null
    private var pipe: Array<ParcelFileDescriptor>? = null
    private var tts: TextToSpeech? = null

    fun run() {
        if (Build.VERSION.SDK_INT < 33) {
            report(JSONObject().put("stage", "availability").put("supportedProbeApi", false))
            return
        }
        val attributes = AudioAttributes.Builder().setUsage(AudioAttributes.USAGE_ASSISTANT)
            .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH).build()
        report(JSONObject().put("stage", "availability")
            .put("onDeviceRecognition", SpeechRecognizer.isOnDeviceRecognitionAvailable(context))
            .put("defaultRecognition", SpeechRecognizer.isRecognitionAvailable(context))
            .put("assistantVolumeStream", attributes.volumeControlStream))
        if (SpeechRecognizer.isRecognitionAvailable(context)) runCatching { querySpeechSupport() }
            .onFailure { finishSpeech(JSONObject().put("stage", "recognition-support").put("error", it.javaClass.simpleName)) }
        tts = TextToSpeech(context) { status -> main.post { inspectVoices(status) } }
        main.postDelayed({
            if (!speechDone.get()) finishSpeech(JSONObject().put("stage", "recognition-support").put("error", "timeout"))
            if (!ttsDone.get()) inspectVoices(TextToSpeech.ERROR)
        }, 5_000)
    }

    private fun querySpeechSupport() {
        pipe = ParcelFileDescriptor.createPipe()
        recognizer = if (SpeechRecognizer.isOnDeviceRecognitionAvailable(context))
            SpeechRecognizer.createOnDeviceSpeechRecognizer(context) else SpeechRecognizer.createSpeechRecognizer(context)
        val request = Intent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH).apply {
            putExtra(RecognizerIntent.EXTRA_LANGUAGE_MODEL, RecognizerIntent.LANGUAGE_MODEL_FREE_FORM)
            putExtra(RecognizerIntent.EXTRA_LANGUAGE, Locale.getDefault().toLanguageTag())
            putExtra(RecognizerIntent.EXTRA_PREFER_OFFLINE, true)
            putExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE, pipe!![0])
            putExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE_CHANNEL_COUNT, 1)
            putExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE_ENCODING, android.media.AudioFormat.ENCODING_PCM_16BIT)
            putExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE_SAMPLING_RATE, 16_000)
            putExtra(RecognizerIntent.EXTRA_SEGMENTED_SESSION, RecognizerIntent.EXTRA_AUDIO_SOURCE)
        }
        recognizer!!.checkRecognitionSupport(request, context.mainExecutor, object : RecognitionSupportCallback {
            override fun onSupportResult(support: RecognitionSupport) = finishSpeech(JSONObject()
                .put("stage", "recognition-support").put("installedOfflineLanguages", JSONArray(support.installedOnDeviceLanguages))
                .put("supportedOfflineLanguages", JSONArray(support.supportedOnDeviceLanguages))
                .put("onlineLanguages", JSONArray(support.onlineLanguages)))
            override fun onError(error: Int) = finishSpeech(JSONObject().put("stage", "recognition-support").put("error", error))
        })
    }

    private fun finishSpeech(result: JSONObject) {
        if (!speechDone.compareAndSet(false, true)) return
        report(result)
        recognizer?.destroy(); recognizer = null
        pipe?.forEach { runCatching { it.close() } }; pipe = null
    }

    private fun inspectVoices(status: Int) {
        if (!ttsDone.compareAndSet(false, true)) return
        val voices = if (status == TextToSpeech.SUCCESS) tts?.voices.orEmpty() else emptySet()
        report(JSONObject().put("stage", "tts-support").put("initialized", status == TextToSpeech.SUCCESS)
            .put("voices", voices.size).put("offlineVoices", voices.count { !it.isNetworkConnectionRequired })
            .put("offlineLocaleVoices", voices.count { !it.isNetworkConnectionRequired && it.locale.language == Locale.getDefault().language }))
        tts?.shutdown(); tts = null
    }

    private fun report(value: JSONObject) { Log.i(TAG, value.toString()) }

    companion object {
        const val ACTION = "dev.gitblame.voxgolem.PROBE_STANDALONE_CAPABILITIES"
        const val TAG = "VoxGolemCapabilities"
    }
}
