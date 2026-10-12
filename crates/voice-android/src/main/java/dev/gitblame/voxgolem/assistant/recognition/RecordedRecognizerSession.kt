package dev.gitblame.voxgolem.assistant.recognition

import android.os.Handler
import android.os.Looper
import dev.gitblame.voxgolem.assistant.inputtransport.SuppliedInputEvents
import dev.gitblame.voxgolem.assistant.inputtransport.SuppliedInputSession

/** One entry lease is held by SharedDictationOwner across both consumers and actual retirement. */
internal class RecordedRecognizerSession(
    private val input: SuppliedInputSession,
    private val audio: RecordedAudioCapture,
    override val route: RecognitionRoute,
    private val main: Handler = Handler(Looper.getMainLooper()),
) : LocalRecognizerSession {
    override val archiveExpected = true
    override var recordedAudio: RecordedAudioResult? = null
        private set
    private var events: LocalRecognizerEvents? = null
    @Volatile private var cancelled = false
    private var terminal = false
    private var audioEnded = false
    private var finalText: String? = null
    private var ending = false
    private var captureStarted = false
    @Volatile private var failureCode: String? = null
    override fun checkLanguage(language: String, result: (Boolean) -> Unit) = input.checkSupport(result)
    override fun start(language: String, events: LocalRecognizerEvents) {
        check(!cancelled && this.events == null)
        this.events = events
        input.start(object : SuppliedInputEvents {
            override fun endOfSpeech() { stop() }
            override fun partial(text: String) { if (!cancelled && !terminal && failureCode == null) events.partial(text) }
            override fun final(text: String) { if (!cancelled && !terminal && failureCode == null) { finalText = text; settle() } }
            override fun error(code: String) { fail(code) }
        })
        if (terminal || cancelled) return
        captureStarted = true
        audio.start({ pcm ->
            // append only touches the thread-safe bounded pipe queue, never SDK methods.
            if (cancelled || failureCode != null) true else input.appendFromWorker(pcm)
        }, { main.post { if (!cancelled && !terminal && !ending && failureCode == null) events.ready() } }, { result -> main.post {
            if (cancelled || terminal) return@post
            audioEnded = true
            result.fold({
                recordedAudio = it
                val error = failureCode
                if (error != null) deliverFailure(error) else { input.finish(); settle() }
            }, { deliverFailure("recording_failed") })
        } })
    }
    private fun settle() {
        val text = finalText ?: return
        if (!audioEnded || recordedAudio == null || cancelled || terminal || failureCode != null) return
        try { audio.recognition(text) } catch (_: Exception) { fail("recording_metadata_failed"); return }
        terminal = true; events?.final(text)
    }
    private fun fail(code: String) {
        if (cancelled || terminal) return
        if (failureCode == null) failureCode = code
        runCatching { audio.finish() }
        if (!captureStarted || audioEnded) deliverFailure(failureCode!!)
    }
    private fun deliverFailure(code: String) {
        if (cancelled || terminal) return
        terminal = true
        runCatching { audio.recognition(null, code) }
        events?.error(code)
    }
    override fun stop() { if (!cancelled && !terminal && !ending) { ending = true; audio.finish() } }
    override fun cancel() {
        cancelled = true; events = null
        if (!terminal) runCatching { audio.recognition(null, "recognition_cancelled") }
        audio.finish()
    }
    override fun destroy() {
        cancelled = true; events = null
        // Retry both owners if either teardown throws. Caller keeps the lease until both complete.
        try { input.close() } finally { audio.close() }
    }
}
