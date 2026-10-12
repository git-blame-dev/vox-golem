package dev.gitblame.voxgolem.assistant.recognition

import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.provider.Settings
import android.speech.RecognitionListener
import android.speech.RecognitionService
import android.speech.RecognitionSupport
import android.speech.RecognitionSupportCallback
import android.speech.RecognizerIntent
import android.speech.SpeechRecognizer
import dev.gitblame.voxgolem.assistant.AssistantTiming
import dev.gitblame.voxgolem.assistant.AssistantTiming.Operation
import dev.gitblame.voxgolem.assistant.AssistantTiming.Stage
import java.util.concurrent.Executor
import dev.gitblame.voxgolem.assistant.inputtransport.AndroidLocalSuppliedAudioFactory
import dev.gitblame.voxgolem.assistant.inputtransport.SuppliedAudioProviderGate

class AndroidLocalRecognizerFactory(
    private val context: Context,
    private val allowProviderFallback: Boolean = true,
    private val fallbackPolicy: () -> Boolean = { allowProviderFallback },
    private val recordedAudio: RecordedAudioFactory? = null,
    private val suppliedAudioGate: SuppliedAudioProviderGate = SuppliedAudioProviderGate.UNPROVEN,
    private val enableRecording: () -> Boolean = { false },
) : LocalRecognizerFactory {
    override fun recordingEnabled() = enableRecording()
    override fun recordingAvailable(language: String, networkFallback: Boolean): Boolean =
        recordedAudio != null && suppliedAudioGate !== SuppliedAudioProviderGate.UNPROVEN &&
            suppliedCandidates(networkFallback).any { it.available(language) }
    override fun availableForEntry(language: String, networkFallback: Boolean, recording: Boolean): Boolean =
        if (recording) recordingAvailable(language, networkFallback) else availableForPolicy(networkFallback)
    override fun createForEntry(purpose: VoiceCapturePurpose, language: String, networkFallback: Boolean,
        timing: AssistantTiming, recording: Boolean): LocalRecognizerSession {
        if (!recording) return createForPolicy(networkFallback, timing)
        requireMain()
        val selected = suppliedCandidates(networkFallback).firstOrNull { it.available(language) }
            ?: throw UnsupportedOperationException("recorded_input_provider_unproven")
        val request = selected.request(language)
        val audio = requireNotNull(recordedAudio).open(RecordedAudioRequest(purpose, language, networkFallback))
        try {
            val input = selected.open(language)
            return RecordedRecognizerSession(input, audio, RecognitionRoute(request.provider, language,
                selectedLanguage = language, onDeviceOnly = request.onDeviceOnly))
        } catch (failure: Exception) { audio.close(); throw failure }
    }
    private fun suppliedCandidates(networkFallback: Boolean): List<AndroidLocalSuppliedAudioFactory> {
        requireMain()
        if (Build.VERSION.SDK_INT < 33) return emptyList()
        return selectCandidates(networkFallback).ordered.distinctBy { it.provider to it.dedicated }
            .filter { it.dedicated || it.component != null }.map {
                AndroidLocalSuppliedAudioFactory(context, it.component, it.dedicated, networkFallback, suppliedAudioGate)
            }
    }
    override fun available(): Boolean {
        requireMain()
        if (!fallbackPolicy()) return localOnlyAvailable()
        return googleService() != null || localOnlyAvailable() || SpeechRecognizer.isRecognitionAvailable(context)
    }
    override fun localOnlyAvailable(): Boolean {
        requireMain()
        return Build.VERSION.SDK_INT >= 31 && SpeechRecognizer.isOnDeviceRecognitionAvailable(context)
    }

    override fun create(): LocalRecognizerSession {
        return create(AssistantTiming.begin(null, Operation.RECOGNITION))
    }
    override fun create(timing: AssistantTiming): LocalRecognizerSession = createForPolicy(fallbackPolicy(), timing)
    override fun availableForPolicy(networkFallback: Boolean): Boolean {
        requireMain()
        return if (networkFallback) googleService() != null || localOnlyAvailable() || SpeechRecognizer.isRecognitionAvailable(context)
            else localOnlyAvailable()
    }
    override fun createForPolicy(networkFallback: Boolean): LocalRecognizerSession =
        createForPolicy(networkFallback, AssistantTiming.begin(null, Operation.RECOGNITION))
    override fun createForPolicy(networkFallback: Boolean, timing: AssistantTiming): LocalRecognizerSession {
        requireMain()
        timing.mark(Stage.CREATE_REQUESTED)
        val selected = selectCandidates(networkFallback)
        if (selected.ordered.isEmpty()) throw UnsupportedOperationException("on-device-speech-api-unavailable")
        return Session(selected.ordered.distinctBy { it.provider }, selected.fallback, networkFallback, timing)
    }

    /** Construction only. Caller owns PFD acceptance/support, listener, lease and actual retirement. */
    fun createSuppliedRecognizer(networkFallback: Boolean): SpeechRecognizer {
        requireMain()
        return selectCandidates(networkFallback).ordered.firstOrNull()?.create?.invoke()
            ?: throw UnsupportedOperationException("on-device-speech-api-unavailable")
    }

    private fun selectCandidates(networkFallback: Boolean): CandidateSelection {
        requireMain()
        val allowFallback = networkFallback
        val google = if (allowFallback) googleService()?.let { component ->
            Candidate(component.flattenToShortString(), false, component) { SpeechRecognizer.createSpeechRecognizer(context, component) }
        } else null
        val dedicated = if (Build.VERSION.SDK_INT >= 31 && SpeechRecognizer.isOnDeviceRecognitionAvailable(context)) {
            val component = onDeviceComponent()
            Candidate(component?.flattenToShortString() ?: "android.on-device", true, component) {
                SpeechRecognizer.createOnDeviceSpeechRecognizer(context)
            }
        } else null
        val configured = if (allowFallback && SpeechRecognizer.isRecognitionAvailable(context)) {
            val component = Settings.Secure.getString(context.contentResolver, "voice_recognition_service")
                ?.let(ComponentName::unflattenFromString)
            Candidate(component?.flattenToShortString() ?: "android.system-default", false, component) {
                SpeechRecognizer.createSpeechRecognizer(context, component)
            }
        } else null
        return CandidateSelection(listOfNotNull(google, dedicated,
            configured?.takeUnless { google != null && it.provider == "android.system-default" }), google ?: configured)
    }

    private fun googleService(): ComponentName? = context.packageManager
        .queryIntentServices(Intent(RecognitionService.SERVICE_INTERFACE), 0)
        .firstOrNull { it.serviceInfo?.packageName == "com.google.android.tts" }
        ?.serviceInfo?.let { ComponentName(it.packageName, it.name) }

    private fun onDeviceComponent(): ComponentName? {
        val resource = context.resources.getIdentifier("config_defaultOnDeviceSpeechRecognitionService", "string", "android")
        return if (resource == 0) null else runCatching {
            ComponentName.unflattenFromString(context.resources.getString(resource))
        }.getOrNull()
    }

    private data class Candidate(val provider: String, val dedicated: Boolean, val component: ComponentName?,
        val create: () -> SpeechRecognizer)
    private data class CandidateSelection(val ordered: List<Candidate>, val fallback: Candidate?)

    private class Session(
        private val localCandidates: List<Candidate>,
        private val providerCandidate: Candidate?,
        private val allowProviderFallback: Boolean,
        private val timing: AssistantTiming,
    ) : LocalRecognizerSession {
        private var candidate = localCandidates.first()
        private var supportTiming: AssistantTiming? = timing.child(Operation.SUPPORT_CHECK).also { it.mark(Stage.CREATE_REQUESTED) }
        private var sdk = try { candidate.create().also { timing.mark(Stage.SDK_CREATED); supportTiming?.mark(Stage.SDK_CREATED) } }
            catch (error: RuntimeException) { supportTiming?.finish(Stage.FAILED); throw error }
        private var nextLocalCandidate = 1
        private val main = Handler(Looper.getMainLooper())
        private var languageCheck = 0L
        private var languageDeadline: Runnable? = null
        private var recognitionEpoch = 0L
        private var destroyed = false
        private var admitted = false
        private var listeningStarted = false
        private var admissionFallbackReason: String? = null
        private var explicitLanguage = false
        override var route: RecognitionRoute? = null
            private set

        override fun checkLanguage(language: String, result: (Boolean) -> Unit) {
            checkSelectedLanguage(language, false, result)
        }
        override fun checkSelectedLanguage(language: String, explicit: Boolean, result: (Boolean) -> Unit) {
            requireMain()
            check(!destroyed && !listeningStarted)
            admitted = false
            explicitLanguage = explicit
            checkCandidate(language, result, if (languageCheck == 0L) supportTiming else null)
        }

        private fun checkCandidate(language: String, result: (Boolean) -> Unit, createdTiming: AssistantTiming? = null) {
            invalidateLanguageCheck(keepTiming = createdTiming)
            val supportTiming = createdTiming ?: timing.child(Operation.SUPPORT_CHECK)
            this.supportTiming = supportTiming
            val checkingSdk = sdk
            route = RecognitionRoute(candidate.provider, language, fallbackReason = admissionFallbackReason,
                onDeviceOnly = candidate.dedicated)
            val ticket = languageCheck
            var settled = false
            var cannotCheck = false
            var unavailableReason = "support_timeout"
            fun finish(supported: Boolean) {
                if (ticket != languageCheck || settled) return
                settled = true
                clearLanguageDeadline()
                admitted = supported
                supportTiming.finish(if (supported) Stage.SUPPORTED else Stage.UNSUPPORTED)
                result(supported)
            }
            fun providerControlled(reason: String) {
                if (ticket != languageCheck || settled) return
                supportTiming.finish(Stage.UNSUPPORTED)
                route = route?.copy(fallbackReason = reason)
                val preferred = providerCandidate
                if (!allowProviderFallback || preferred == null) { finish(false); return }
                if (explicitLanguage && (candidate.dedicated || candidate.provider != preferred.provider)) {
                    settled = true
                    clearLanguageDeadline()
                    if (!replace(preferred, ticket)) { if (ticket == languageCheck) result(false); return }
                    checkCandidate(language, result, this.supportTiming)
                    return
                }
                if (explicitLanguage && SpeechLocales.installedMatch(language, route?.supportedLanguages.orEmpty()) == null) {
                    route = route?.copy(fallbackReason = "requested_language_support_unavailable")
                    finish(false); return
                }
                if (candidate.dedicated || candidate.provider != preferred.provider) {
                    settled = true
                    clearLanguageDeadline()
                    if (!replace(preferred, ticket)) {
                        if (ticket == languageCheck) result(false)
                        return
                    }
                    route = RecognitionRoute(candidate.provider, language, localPreferred = false, fallbackReason = reason)
                    this.supportTiming?.finish(Stage.UNSUPPORTED)
                    if (ticket == languageCheck) { admitted = true; result(true) }
                    return
                }
                route = route?.copy(localPreferred = false, fallbackReason = reason)
                finish(true)
            }
            fun unavailable(reason: String) {
                if (ticket != languageCheck || settled) return
                supportTiming.finish(Stage.UNSUPPORTED)
                route = route?.copy(fallbackReason = reason)
                if (nextLocalCandidate >= localCandidates.size) { providerControlled(reason); return }
                settled = true
                clearLanguageDeadline()
                admissionFallbackReason = if (candidate.dedicated) "dedicated_$reason" else reason
                if (!replace(localCandidates[nextLocalCandidate++], ticket)) {
                    if (ticket == languageCheck) result(false)
                    return
                }
                checkCandidate(language, result, this.supportTiming)
            }
            if (Build.VERSION.SDK_INT < 33) {
                if (candidate.dedicated) { finish(!explicitLanguage); return }
                providerControlled("support_metadata_unavailable")
                return
            }
            languageDeadline = Runnable {
                if (ticket == languageCheck && !settled) supportTiming.mark(Stage.TIMED_OUT)
                unavailable(unavailableReason)
            }.also { main.postDelayed(it, 3_000) }
            fun supportError(error: Int) {
                if (ticket != languageCheck || settled || destroyed || checkingSdk !== sdk || listeningStarted) return
                supportTiming.mark(Stage.SUPPORT_CALLBACK); supportTiming.mark(Stage.ON_ERROR)
                when (error) {
                    // Google emits error 14 before two valid installed-language replies ~100ms later.
                    SpeechRecognizer.ERROR_CANNOT_CHECK_SUPPORT -> {
                        cannotCheck = true; unavailableReason = "support_metadata_unavailable"
                    }
                    SpeechRecognizer.ERROR_LANGUAGE_NOT_SUPPORTED, SpeechRecognizer.ERROR_LANGUAGE_UNAVAILABLE ->
                        unavailable("installed_language_unavailable")
                    else -> { supportTiming.finish(Stage.FAILED); route = route?.copy(fallbackReason = "support_error_$error"); finish(false) }
                }
            }
            val support = object : RecognitionSupportCallback {
                override fun onSupportResult(support: RecognitionSupport) {
                    if (ticket != languageCheck || settled) return
                    supportTiming.mark(Stage.SUPPORT_CALLBACK)
                    route = route?.copy(installedLanguages = support.installedOnDeviceLanguages.filter { it.length <= 64 }.take(128),
                        supportedLanguages = (support.installedOnDeviceLanguages + if (allowProviderFallback) support.onlineLanguages else emptyList())
                            .filter { it.length <= 64 }.distinct().take(128))
                    val selected = SpeechLocales.installedMatch(language, support.installedOnDeviceLanguages)
                    if (selected != null) {
                        route = route?.copy(selectedLanguage = selected)
                        finish(true)
                    } else {
                        unavailableReason = "installed_language_unavailable"
                        if (!cannotCheck) unavailable(unavailableReason)
                    }
                }
                override fun onError(error: Int) {
                    supportError(error)
                }
            }
            try {
                // SDK service setup/binding failures use RecognitionListener, even for a support request.
                // This candidate-scoped listener is replaced by the listening-phase listener at start.
                checkingSdk.setRecognitionListener(object : RecognitionListener {
                    override fun onReadyForSpeech(params: Bundle?) {}
                    override fun onBeginningOfSpeech() {}
                    override fun onRmsChanged(rmsdB: Float) {}
                    override fun onBufferReceived(buffer: ByteArray?) {}
                    override fun onEndOfSpeech() {}
                    override fun onError(error: Int) { supportError(error) }
                    override fun onResults(results: Bundle?) {}
                    override fun onPartialResults(partialResults: Bundle?) {}
                    override fun onEvent(eventType: Int, params: Bundle?) {}
                })
                supportTiming.mark(Stage.SUPPORT_REQUESTED)
                checkingSdk.checkRecognitionSupport(intent(language), Executor { supportTiming.mark(Stage.SUPPORT_CALLBACK); main.post(it) }, support)
            }
            catch (_: UnsupportedOperationException) { unavailable("support_metadata_unavailable") }
            catch (_: RuntimeException) { supportTiming.finish(Stage.FAILED); route = route?.copy(fallbackReason = "support_request_failed"); finish(false) }
        }

        private fun replace(next: Candidate, ticket: Long): Boolean {
            if (runCatching { sdk.destroy() }.isFailure) {
                timing.mark(Stage.RETIREMENT_FAILED)
                route = route?.copy(fallbackReason = "recognition_cleanup_failed")
                return false
            }
            if (ticket != languageCheck || destroyed) return false
            val createdTiming = timing.child(Operation.SUPPORT_CHECK).also { it.mark(Stage.CREATE_REQUESTED) }
            val replacement = runCatching { next.create() }.getOrNull()
            if (replacement == null) {
                createdTiming.finish(Stage.FAILED)
                route = route?.copy(fallbackReason = "recognition_candidate_unavailable")
                return false
            }
            createdTiming.mark(Stage.SDK_CREATED)
            supportTiming = createdTiming
            sdk = replacement; candidate = next
            timing.mark(Stage.SDK_CREATED)
            return ticket == languageCheck && !destroyed
        }

        override fun start(language: String, events: LocalRecognizerEvents) {
            requireMain()
            check(!destroyed && admitted && !listeningStarted)
            listeningStarted = true
            val ticket = ++recognitionEpoch
            var terminal = false
            var providerFinal = ""
            fun active() = ticket == recognitionEpoch && !destroyed && !terminal
            fun reported(bundle: Bundle?) {
                val value = bundle?.getString("current_locale")?.let(SpeechLocales::parse)?.toLanguageTag()
                if (value != null) route = route?.copy(reportedLanguage = value)
            }
            sdk.setRecognitionListener(object : RecognitionListener {
                override fun onReadyForSpeech(params: Bundle?) { if (active()) { timing.mark(Stage.READY_CALLBACK); events.ready() } }
                override fun onBeginningOfSpeech() {}
                override fun onRmsChanged(rmsdB: Float) {}
                override fun onBufferReceived(buffer: ByteArray?) {}
                override fun onEndOfSpeech() { if (active()) timing.mark(Stage.END_OF_SPEECH) }
                override fun onError(error: Int) {
                    if (!active()) return
                    timing.mark(Stage.ON_ERROR)
                    terminal = true; events.error("recognition_error_$error")
                }
                override fun onResults(results: Bundle?) {
                    if (!active()) return
                    timing.mark(Stage.FINAL_CALLBACK)
                    reported(results); terminal = true
                    events.final(text(results).ifBlank { providerFinal })
                }
                override fun onPartialResults(partialResults: Bundle?) {
                    if (!active()) return
                    reported(partialResults)
                    val value = text(partialResults)
                    if (value.isNotBlank()) timing.mark(Stage.FIRST_PARTIAL)
                    if (partialResults?.getBoolean("final_result", false) == true && value.isNotBlank()) providerFinal = value
                    events.partial(value)
                }
                override fun onEvent(eventType: Int, params: Bundle?) {}
            })
            timing.mark(Stage.LISTENING_REQUESTED)
            sdk.startListening(intent(route?.selectedLanguage ?: language, route?.localPreferred ?: true))
        }
        override fun stop() { requireMain(); timing.mark(Stage.FINISH_REQUESTED); sdk.stopListening() }
        override fun cancel() { requireMain(); recognitionEpoch++; invalidateLanguageCheck(); sdk.cancel() }
        override fun destroy() { requireMain(); recognitionEpoch++; invalidateLanguageCheck(); sdk.destroy(); destroyed = true }
        private fun invalidateLanguageCheck(keepTiming: AssistantTiming? = null) {
            languageCheck++
            if (supportTiming !== keepTiming) supportTiming?.finish(Stage.CANCELLED)
            supportTiming = null
            admitted = false
            clearLanguageDeadline()
        }
        private fun clearLanguageDeadline() {
            languageDeadline?.let(main::removeCallbacks); languageDeadline = null
        }
    }
    companion object {
        private fun requireMain() { check(Looper.myLooper() == Looper.getMainLooper()) }
        private fun text(bundle: Bundle?) = bundle?.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)?.firstOrNull().orEmpty()
        internal fun intent(language: String, preferOffline: Boolean = true) = Intent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH)
            .putExtra(RecognizerIntent.EXTRA_LANGUAGE_MODEL, RecognizerIntent.LANGUAGE_MODEL_FREE_FORM)
            .putExtra(RecognizerIntent.EXTRA_LANGUAGE, language)
            .putExtra(RecognizerIntent.EXTRA_PARTIAL_RESULTS, true)
            .putExtra(RecognizerIntent.EXTRA_PREFER_OFFLINE, preferOffline)
    }
}
