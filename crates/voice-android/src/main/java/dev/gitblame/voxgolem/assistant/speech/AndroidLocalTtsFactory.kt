package dev.gitblame.voxgolem.assistant.speech

import android.content.Context
import android.content.Intent
import android.icu.util.ULocale
import android.media.AudioAttributes
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.speech.tts.TextToSpeech
import android.speech.tts.UtteranceProgressListener
import android.speech.tts.Voice
import dev.gitblame.voxgolem.assistant.AssistantTiming
import dev.gitblame.voxgolem.assistant.AssistantTiming.Operation
import dev.gitblame.voxgolem.assistant.AssistantTiming.Stage
import dev.gitblame.voxgolem.assistant.AssistantDiagnosticJournal.Metadata
import dev.gitblame.voxgolem.assistant.recognition.AndroidPreRollAudioSource
import java.io.File
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicBoolean

class AndroidLocalTtsFactory(
    private val context: Context,
    private val strictGoogleOnly: Boolean = !AndroidPreRollAudioSource.isRobolectricRuntime(),
    private val beginTiming: (Operation) -> AssistantTiming = { AssistantTiming.begin(context, it) },
) : LocalTtsFactory {
    constructor(
        context: Context,
        beginTiming: (Operation) -> AssistantTiming,
    ) : this(
        context = context,
        strictGoogleOnly = !AndroidPreRollAudioSource.isRobolectricRuntime(),
        beginTiming = beginTiming,
    )

    private fun isStrictGoogleMode(): Boolean =
        strictGoogleOnly || SpeechServiceRequirement.isTtsInstalled(context)

    override val localOnly: Boolean get() = isStrictGoogleMode()

    override fun providers(): List<String?> {
        if (isStrictGoogleMode()) {
            return if (SpeechServiceRequirement.isTtsInstalled(context)) {
                listOf(SpeechServiceRequirement.GOOGLE_SPEECH_PACKAGE)
            } else {
                emptyList()
            }
        }
        return listOf(null) + context.packageManager
            .queryIntentServices(Intent(TextToSpeech.Engine.INTENT_ACTION_TTS_SERVICE), 0)
            .mapNotNull { it.serviceInfo?.packageName }
            .filter { it != SAMSUNG_SPEECH_PACKAGE }
            .distinct()
    }

    override fun create(onReady: (Boolean) -> Unit): LocalTtsSession = create(null, onReady)
    override fun create(provider: String?, onReady: (Boolean) -> Unit): LocalTtsSession = create(provider, null, onReady)
    override fun create(provider: String?, timing: AssistantTiming?, onReady: (Boolean) -> Unit): LocalTtsSession {
        requireMain()
        val initialization = timing?.child(Operation.TTS_INITIALIZATION) ?: beginTiming(Operation.TTS_INITIALIZATION)
        initialization.mark(Stage.CREATE_REQUESTED)
        if (isStrictGoogleMode()) require(provider == null || provider == SpeechServiceRequirement.GOOGLE_SPEECH_PACKAGE)
        val requestedProvider = provider ?: if (isStrictGoogleMode()) SpeechServiceRequirement.GOOGLE_SPEECH_PACKAGE else null
        val main = Handler(Looper.getMainLooper())
        val retired = AtomicBoolean(false)
        lateinit var tts: TextToSpeech
        try {
            val selectedContext = requestedProvider?.let { ExplicitTtsProviderContext.create(context, it) } ?: context
            tts = TextToSpeech(selectedContext, { status ->
                initialization.mark(Stage.INIT_CALLBACK)
                main.post {
                    initialization.mark(Stage.INIT_MAIN_DELIVERED)
                    if (retired.get()) tts.shutdown() else {
                        initialization.finish(if (status == TextToSpeech.SUCCESS) Stage.INIT_READY else Stage.INIT_FAILED)
                        onReady(status == TextToSpeech.SUCCESS)
                    }
                }
            }, requestedProvider)
        } catch (error: RuntimeException) { initialization.finish(Stage.INIT_FAILED); throw error }
        return object : LocalTtsSession {
            private var utterance = 0L
            private var selectedVoice: String? = null
            private var attributesConfigured = false
            private var nativeVoices: Map<String, Voice>? = null
            private var cachedLocalVoices: List<LocalVoice>? = null
            private var warmedUp = false
            private var primeTiming: AssistantTiming? = null
            // A retired client's late callbacks must not delete a replacement session's prime.
            private val warmupFile = File(context.cacheDir, "${UUID.randomUUID()}-$WARMUP_FILE_NAME")
            private val activeUtterances = ConcurrentHashMap<String, ActiveUtterance>()
            private var listenerInstalled = false

            private inner class ActiveUtterance(
                ticket: Long,
                chunks: List<String>,
                val playback: AssistantTiming,
                val onTerminal: (String?) -> Unit,
            ) {
                val ids = chunks.indices.map { "local-final-$ticket-$it" }
                val pendingIds = ids.toMutableSet()
                val completed = AtomicBoolean(false)
                fun complete(error: String?, stage: Stage) {
                    if (completed.compareAndSet(false, true)) {
                        ids.forEach(activeUtterances::remove)
                        playback.finish(stage)
                        onTerminal(error)
                    }
                }
            }

            private fun installedVoices(): Map<String, Voice> = nativeVoices
                ?: tts.voices.orEmpty().associateBy { it.name }.also { nativeVoices = it }
            override val provider: String? get() = requestedProvider ?: tts.defaultEngine
            override val providerIsPreference: Boolean get() = true
            override val defaultVoiceName: String? get() = tts.defaultVoice?.name
            override fun voices(): List<LocalVoice> {
                requireMain()
                cachedLocalVoices?.let { return it }
                val inventory = initialization.fork(Operation.VOICE_INVENTORY).also { it.mark(Stage.VOICE_INVENTORY_REQUESTED) }
                return try { installedVoices().values.filter { !localOnly || !it.isNetworkConnectionRequired }.map {
                    LocalVoice(
                        it.name,
                        ULocale(ULocale.forLocale(it.locale).name).toLocale(),
                        it.isNetworkConnectionRequired,
                        TextToSpeech.Engine.KEY_FEATURE_NOT_INSTALLED !in it.features.orEmpty(),
                    )
                }.also {
                    cachedLocalVoices = it
                    inventory.metadata(Metadata(engine = requestedProvider, count = it.size))
                    inventory.finish(Stage.VOICE_INVENTORY_READY)
                } } catch (error: RuntimeException) { inventory.finish(Stage.FAILED); throw error }
            }

            private fun nativeVoice(voice: LocalVoice): Voice? = installedVoices()[voice.name]?.takeIf {
                (!isStrictGoogleMode() || !it.isNetworkConnectionRequired) &&
                    it.isNetworkConnectionRequired == voice.networkRequired &&
                    TextToSpeech.Engine.KEY_FEATURE_NOT_INSTALLED !in it.features.orEmpty()
            }

            private fun selectNativeVoice(voice: LocalVoice): Boolean {
                val installed = nativeVoice(voice) ?: return false
                if (selectedVoice != installed.name) {
                    if (tts.setVoice(installed) != TextToSpeech.SUCCESS) return false
                    selectedVoice = installed.name
                    if (!voice.networkRequired) {
                        cacheVoiceName(context, voice.locale.toLanguageTag(), installed.name)
                    }
                }
                return true
            }

            override fun warmup(voice: LocalVoice) = warmup(voice, null)
            override fun warmup(voice: LocalVoice, timing: AssistantTiming?) {
                requireMain()
                if (retired.get() || warmedUp || voice.networkRequired || !selectNativeVoice(voice)) return
                val prime = (timing ?: initialization).fork(Operation.TTS_WARMUP).also { primeTiming = it; it.mark(Stage.PRIME_REQUESTED) }
                warmedUp = true
                ensureListenerInstalled()
                val accepted = runCatching {
                    tts.synthesizeToFile(".", Bundle(), warmupFile, WARMUP_UTTERANCE_ID) == TextToSpeech.SUCCESS
                }.getOrDefault(false)
                if (accepted) prime.mark(Stage.PRIME_ACCEPTED)
                else { prime.finish(Stage.PRIME_REJECTED); cleanWarmupFile() }
            }

            private fun cleanWarmupFile() { runCatching { warmupFile.delete() } }

            private fun ensureListenerInstalled() {
                if (listenerInstalled) return
                listenerInstalled = true
                tts.setOnUtteranceProgressListener(object : UtteranceProgressListener() {
                    override fun onStart(utteranceId: String?) {
                        if (utteranceId == null || utteranceId == WARMUP_UTTERANCE_ID || retired.get()) return
                        val active = activeUtterances[utteranceId] ?: return
                        if (!active.completed.get()) {
                            active.playback.mark(Stage.ON_START)
                        }
                    }
                    override fun onDone(utteranceId: String?) {
                        if (utteranceId == WARMUP_UTTERANCE_ID) {
                            main.post { primeTiming?.finish(Stage.PRIME_COMPLETED); cleanWarmupFile() }
                            return
                        }
                        handleEvent(utteranceId, null, Stage.ON_DONE)
                    }
                    @Deprecated("Android legacy callback")
                    override fun onError(utteranceId: String?) {
                        if (utteranceId == WARMUP_UTTERANCE_ID) { main.post { primeTiming?.finish(Stage.PRIME_FAILED); cleanWarmupFile() }; return }
                        handleEvent(utteranceId, "speech_failed", Stage.ON_ERROR)
                    }
                    override fun onError(utteranceId: String?, errorCode: Int) {
                        if (utteranceId == WARMUP_UTTERANCE_ID) { main.post { primeTiming?.finish(Stage.PRIME_FAILED); cleanWarmupFile() }; return }
                        handleEvent(utteranceId, "speech_error_$errorCode", Stage.ON_ERROR)
                    }
                    override fun onStop(utteranceId: String?, interrupted: Boolean) {
                        if (utteranceId == WARMUP_UTTERANCE_ID) { main.post { primeTiming?.finish(Stage.CANCELLED); cleanWarmupFile() }; return }
                        handleEvent(utteranceId, "speech_interrupted", Stage.ON_STOP)
                    }
                })
            }

            private fun handleEvent(utteranceId: String?, error: String?, stage: Stage) {
                if (utteranceId == null || utteranceId == WARMUP_UTTERANCE_ID) return
                main.post {
                    if (retired.get()) return@post
                    val active = activeUtterances.remove(utteranceId) ?: return@post
                    if (active.completed.get() || !active.pendingIds.remove(utteranceId)) return@post
                    if (error != null) {
                        active.complete(error, stage)
                    } else if (active.pendingIds.isEmpty()) {
                        active.complete(null, Stage.ON_DONE)
                    }
                }
            }

            override fun speak(text: String, voice: LocalVoice, onTerminal: (String?) -> Unit): Boolean =
                speak(text, voice, TextToSpeech.QUEUE_FLUSH, null, onTerminal)
            override fun speak(text: String, voice: LocalVoice, timing: AssistantTiming?, onTerminal: (String?) -> Unit): Boolean =
                speak(text, voice, TextToSpeech.QUEUE_FLUSH, timing, onTerminal)
            override fun speak(
                text: String,
                voice: LocalVoice,
                queueMode: Int,
                timing: AssistantTiming?,
                onTerminal: (String?) -> Unit,
            ): Boolean {
                requireMain()
                if (retired.get()) return false
                if (!selectNativeVoice(voice)) return false
                if (!attributesConfigured) {
                    tts.setAudioAttributes(speechAttributes())
                    attributesConfigured = true
                }
                val ticket = ++utterance
                val chunks = speechChunks(text, TextToSpeech.getMaxSpeechInputLength())
                if (chunks.isEmpty()) return false
                if (queueMode == TextToSpeech.QUEUE_FLUSH) {
                    val replaced = activeUtterances.values.distinct()
                    activeUtterances.clear()
                    replaced.forEach {
                        if (it.completed.compareAndSet(false, true)) {
                            it.playback.finish(Stage.REPLACED)
                        }
                    }
                }
                val playback = timing?.child(Operation.TTS_UTTERANCE) ?: beginTiming(Operation.TTS_UTTERANCE)
                val active = ActiveUtterance(ticket, chunks, playback, onTerminal)
                try {
                    ensureListenerInstalled()
                    active.ids.forEach { activeUtterances[it] = active }
                    for ((index, chunk) in chunks.withIndex()) {
                        playback.mark(Stage.QUEUE_REQUESTED)
                        val accepted = runCatching {
                            tts.speak(chunk, if (index == 0) queueMode else TextToSpeech.QUEUE_ADD, Bundle(), active.ids[index]) == TextToSpeech.SUCCESS
                        }.getOrDefault(false)
                        if (!accepted) {
                            active.ids.forEach(activeUtterances::remove)
                            active.completed.set(true)
                            playback.finish(Stage.QUEUE_REJECTED)
                            return false
                        }
                        playback.mark(Stage.QUEUE_ACCEPTED)
                    }
                    return true
                } catch (error: RuntimeException) {
                    active.ids.forEach(activeUtterances::remove)
                    active.completed.set(true)
                    playback.finish(Stage.FAILED)
                    throw error
                }
            }
            override fun stop() {
                requireMain(); utterance++
                primeTiming?.finish(Stage.CANCELLED)
                if (retired.get()) { cleanWarmupFile(); return }
                val activeList = activeUtterances.values.distinct()
                activeUtterances.clear()
                try { tts.stop() } finally {
                    cleanWarmupFile()
                    activeList.forEach {
                        if (it.completed.compareAndSet(false, true)) {
                            it.playback.finish(Stage.CANCELLED)
                        }
                    }
                }
            }
            override fun shutdown() {
                requireMain(); retired.set(true); utterance++
                primeTiming?.finish(Stage.CANCELLED)
                val activeList = activeUtterances.values.distinct()
                activeUtterances.clear()
                initialization.finish(Stage.CANCELLED)
                try {
                    tts.shutdown()
                    activeList.forEach {
                        if (it.completed.compareAndSet(false, true)) {
                            it.playback.finish(Stage.SESSION_RETIRED)
                        }
                    }
                } catch (error: RuntimeException) {
                    activeList.forEach {
                        if (it.completed.compareAndSet(false, true)) {
                            it.playback.finish(Stage.RETIREMENT_FAILED)
                        }
                    }
                    throw error
                } finally {
                    cleanWarmupFile()
                }
            }
        }
    }
    companion object {
        internal const val WARMUP_UTTERANCE_ID = "vox_warmup"
        internal const val WARMUP_FILE_NAME = "vox_tts_warmup.wav"
        internal const val VOICE_CACHE_PREFS = "voxgolem_google_tts_voice_cache"
        private const val SAMSUNG_SPEECH_PACKAGE = "com.samsung.SMT"
        private val memoryVoiceCache = ConcurrentHashMap<String, String>()

        internal fun getCachedVoiceName(context: Context, languageTag: String): String? {
            memoryVoiceCache[languageTag]?.let { return it }
            val prefs = runCatching {
                context.applicationContext.getSharedPreferences(VOICE_CACHE_PREFS, Context.MODE_PRIVATE)
            }.getOrNull() ?: return null
            return prefs.getString(languageTag, null)?.also { memoryVoiceCache[languageTag] = it }
        }

        internal fun cacheVoiceName(context: Context, languageTag: String, voiceName: String) {
            memoryVoiceCache[languageTag] = voiceName
            runCatching {
                context.applicationContext
                    .getSharedPreferences(VOICE_CACHE_PREFS, Context.MODE_PRIVATE)
                    .edit()
                    .putString(languageTag, voiceName)
                    .apply()
            }
        }

        internal fun clearVoiceCache(context: Context? = null) {
            memoryVoiceCache.clear()
            if (context != null) {
                runCatching {
                    context.applicationContext
                        .getSharedPreferences(VOICE_CACHE_PREFS, Context.MODE_PRIVATE)
                        .edit()
                        .clear()
                        .apply()
                }
            }
        }

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
