package dev.gitblame.voxgolem.assistant.speech

import android.content.Context
import android.content.Intent
import android.icu.util.ULocale
import android.media.AudioAttributes
import android.os.Bundle
import android.os.Looper
import android.os.Build
import android.os.Handler
import android.speech.tts.TextToSpeech
import android.speech.tts.UtteranceProgressListener
import android.speech.tts.Voice
import dev.gitblame.voxgolem.assistant.AssistantTiming
import dev.gitblame.voxgolem.assistant.AssistantTiming.Operation
import dev.gitblame.voxgolem.assistant.AssistantTiming.Stage
import java.util.concurrent.atomic.AtomicBoolean

class AndroidLocalTtsFactory(private val context: Context,
    private val beginTiming: (Operation) -> AssistantTiming = { AssistantTiming.begin(context, it) },
) : LocalTtsFactory {
    override fun providers(): List<String?> = listOf(null) + context.packageManager
        .queryIntentServices(Intent(TextToSpeech.Engine.INTENT_ACTION_TTS_SERVICE), 0)
        .mapNotNull { it.serviceInfo?.packageName }.distinct()
    override fun create(onReady: (Boolean) -> Unit): LocalTtsSession = create(null, onReady)
    override fun create(provider: String?, onReady: (Boolean) -> Unit): LocalTtsSession = create(provider, null, onReady)
    override fun create(provider: String?, timing: AssistantTiming?, onReady: (Boolean) -> Unit): LocalTtsSession {
        requireMain()
        val initialization = timing?.child(Operation.TTS_INITIALIZATION) ?: beginTiming(Operation.TTS_INITIALIZATION)
        initialization.mark(Stage.CREATE_REQUESTED)
        val requestedProvider = provider
        val main = Handler(Looper.getMainLooper())
        var retired = false
        lateinit var tts: TextToSpeech
        try {
            val selectedContext = requestedProvider?.let { ExplicitTtsProviderContext.create(context, it) } ?: context
            tts = TextToSpeech(selectedContext, { status ->
                initialization.mark(Stage.INIT_CALLBACK)
                main.post {
                    initialization.mark(Stage.INIT_MAIN_DELIVERED)
                    if (retired) tts.shutdown() else {
                        initialization.finish(if (status == TextToSpeech.SUCCESS) Stage.INIT_READY else Stage.INIT_FAILED)
                        onReady(status == TextToSpeech.SUCCESS)
                    }
                }
            }, requestedProvider)
        } catch (error: RuntimeException) { initialization.finish(Stage.INIT_FAILED); throw error }
        return object : LocalTtsSession {
            private var utterance = 0L
            private var selectedVoice: String? = null
            private var nativeVoices: Map<String, Voice>? = null
            @Volatile private var playbackTiming: AssistantTiming? = null
            private fun installedVoices(): Map<String, Voice> = nativeVoices
                ?: tts.voices.orEmpty().associateBy { it.name }.also { nativeVoices = it }
            override val provider: String? get() = requestedProvider ?: tts.defaultEngine
            override val providerIsPreference: Boolean get() = true
            override val defaultVoiceName: String? get() = tts.defaultVoice?.name
            override fun voices(): List<LocalVoice> {
                requireMain()
                return installedVoices().values.map { LocalVoice(it.name, ULocale(ULocale.forLocale(it.locale).name).toLocale(), it.isNetworkConnectionRequired,
                    TextToSpeech.Engine.KEY_FEATURE_NOT_INSTALLED !in it.features.orEmpty()) }
            }
            override fun speak(text: String, voice: LocalVoice, onTerminal: (String?) -> Unit): Boolean =
                speak(text, voice, null, onTerminal)
            override fun speak(text: String, voice: LocalVoice, timing: AssistantTiming?, onTerminal: (String?) -> Unit): Boolean {
                requireMain()
                if (retired) return false
                val installed = installedVoices()[voice.name]?.takeIf {
                    it.isNetworkConnectionRequired == voice.networkRequired &&
                        TextToSpeech.Engine.KEY_FEATURE_NOT_INSTALLED !in it.features.orEmpty()
                } ?: return false
                if (selectedVoice != installed.name) {
                    if (tts.setVoice(installed) != TextToSpeech.SUCCESS) return false
                    selectedVoice = installed.name
                }
                tts.setAudioAttributes(speechAttributes())
                val ticket = ++utterance
                val chunks = speechChunks(text, TextToSpeech.getMaxSpeechInputLength())
                if (chunks.isEmpty()) return false
                playbackTiming?.finish(Stage.REPLACED)
                val playback = timing?.child(Operation.TTS_UTTERANCE) ?: beginTiming(Operation.TTS_UTTERANCE)
                playbackTiming = playback
                var index = 0
                fun id() = "local-final-$ticket-$index"
                val completed = AtomicBoolean()
                fun complete(error: String?, stage: Stage) {
                    if (completed.compareAndSet(false, true)) {
                        playback.finish(stage)
                        onTerminal(error)
                    }
                }
                fun enqueue(): Boolean {
                    playback.mark(Stage.QUEUE_REQUESTED)
                    val accepted = runCatching { tts.speak(chunks[index], TextToSpeech.QUEUE_FLUSH, Bundle(), id()) == TextToSpeech.SUCCESS }.getOrDefault(false)
                    if (accepted) playback.mark(Stage.QUEUE_ACCEPTED) else playback.finish(Stage.QUEUE_REJECTED)
                    return accepted
                }
                fun event(utteranceId: String?, error: String?, stage: Stage) { main.post {
                    if (retired || utterance != ticket || completed.get() || utteranceId != id()) return@post
                    if (error != null) complete(error, stage)
                    else if (++index == chunks.size) complete(null, Stage.ON_DONE)
                    else if (!enqueue()) complete("speech_failed", Stage.QUEUE_REJECTED)
                } }
                try {
                    tts.setOnUtteranceProgressListener(object : UtteranceProgressListener() {
                        override fun onStart(utteranceId: String?) {
                            if (!retired && utterance == ticket && !completed.get() && utteranceId == id()) playback.mark(Stage.ON_START)
                        }
                        override fun onDone(utteranceId: String?) { event(utteranceId, null, Stage.ON_DONE) }
                        @Deprecated("Android legacy callback")
                        override fun onError(utteranceId: String?) { event(utteranceId, "speech_failed", Stage.ON_ERROR) }
                        override fun onError(utteranceId: String?, errorCode: Int) { event(utteranceId, "speech_error_$errorCode", Stage.ON_ERROR) }
                        override fun onStop(utteranceId: String?, interrupted: Boolean) { event(utteranceId, "speech_interrupted", Stage.ON_STOP) }
                    })
                    return enqueue().also { if (!it) completed.set(true) }
                } catch (error: RuntimeException) { playback.finish(Stage.FAILED); throw error }
            }
            override fun stop() {
                requireMain(); utterance++
                try { tts.stop() } finally { playbackTiming?.finish(Stage.CANCELLED) }
            }
            override fun shutdown() {
                requireMain(); retired = true; utterance++
                initialization.finish(Stage.CANCELLED)
                try { tts.shutdown(); playbackTiming?.finish(Stage.SESSION_RETIRED) }
                catch (error: RuntimeException) { playbackTiming?.finish(Stage.RETIREMENT_FAILED); throw error }
            }
        }
    }
    companion object {
        /** Android's limit counts UTF-16 units. Keep every chunk strictly below it, without
         * splitting a surrogate pair or changing the original answer's punctuation/whitespace. */
        internal fun speechChunks(text: String, inputLimit: Int): List<String> {
            require(inputLimit > 2)
            val chunks = mutableListOf<String>()
            var start = 0
            while (start < text.length) {
                var end = minOf(start + inputLimit - 1, text.length)
                if (end < text.length && text[end - 1].isHighSurrogate() && text[end].isLowSurrogate()) end--
                chunks += text.substring(start, end)
                start = end
            }
            return chunks
        }
        internal fun requireMain() { check(Looper.myLooper() == Looper.getMainLooper()) }
        internal fun speechAttributes(): AudioAttributes = AudioAttributes.Builder()
            .setUsage(if (Build.VERSION.SDK_INT >= 26) AudioAttributes.USAGE_ASSISTANT else AudioAttributes.USAGE_MEDIA)
            .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH).build()
    }
}
