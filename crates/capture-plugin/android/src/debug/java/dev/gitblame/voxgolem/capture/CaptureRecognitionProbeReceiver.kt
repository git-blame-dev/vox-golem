package dev.gitblame.voxgolem.capture

import android.Manifest
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import org.json.JSONObject
import java.io.File
import java.util.Locale
import java.util.UUID
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference

/** Shell-only synthetic replay. No capture records, microphone, playback, or transcript storage. */
class CaptureRecognitionProbeReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent?) {
        if (intent?.action != ACTION) return
        Probe(context.applicationContext, goAsync()).start()
    }

    private class Probe(private val context: Context, private val pending: PendingResult) {
        private val main = Handler(Looper.getMainLooper())
        private val active = AtomicBoolean(true)
        private val source = AtomicReference<WavPcmReader?>()
        private val inputEof = AtomicBoolean(false)
        private val sessionId = UUID.randomUUID().toString()
        private val format = CaptureFormat(sampleRate = 16_000, channels = 1, bitsPerSample = 16)
        private val startedAt = SystemClock.elapsedRealtime()
        private var coordinator: PhoneCaptureCoordinator? = null
        private val adapter = SuppliedAudioSpeechRecognizer(context)
        private val directory by lazy { File(context.cacheDir.canonicalFile, DIRECTORY) }
        private var reserved = false
        private var feeder: Thread? = null
        private var partialCallbacks = 0
        private var segmentCallbacks = 0
        private var hypotheses = 0
        private var emptyPartials = 0
        private var emptySegments = 0
        private var readyMillis = -1L
        private var firstWriteMillis = -1L
        private var firstTextMillis = -1L
        private var firstPartialMillis = -1L
        private var firstSegmentMillis = -1L
        private var finalMillis = -1L
        private var inputEofMillis = -1L
        private var finalObserved = false
        private var expectedMatch = false
        private var anyExpectedMatch = false
        private var onDeviceAvailable = false
        private var externalAudioAvailable = false
        private var formatSupported = false
        private var validatedFormat = false
        private var foregroundReady = false
        private var frames = 0L
        private var backlogEvents = 0
        private var blockedWrites = 0
        private val deadline = Runnable { finish(TIMEOUT) }

        fun start() {
            main.postDelayed(deadline, (DEADLINE_MILLIS - elapsed()).coerceAtLeast(0))
            foregroundReady = PhoneCaptureRuntimeService.isForegroundReady()
            if (!foregroundReady) { finish(RUNTIME_NOT_READY); return }
            if (context.packageManager.hasSystemFeature("android.hardware.type.watch")) {
                finish(NOT_PHONE); return
            }
            if (Build.VERSION.SDK_INT < 33) { finish(SDK_UNAVAILABLE); return }
            if (context.checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
                finish(PERMISSION_MISSING); return
            }
            try {
                val capabilities = adapter.capabilities()
                onDeviceAvailable = capabilities.onDevice
                externalAudioAvailable = capabilities.externalAudio
                formatSupported = format in capabilities.formats
                if (!onDeviceAvailable || !externalAudioAvailable || !formatSupported) {
                    finish(PROVIDER_UNAVAILABLE); return
                }
                val owner = PhoneCaptureCoordinator.shared(context)
                coordinator = owner
                reserved = owner.tryBeginRecognitionProbe(sessionId) { finish(RUNTIME_NOT_READY) }
                if (!reserved) { finish(OWNER_BUSY); return }
            } catch (_: Exception) {
                finish(SETUP_FAILED); return
            }
            feeder = Thread({
                try {
                    val file = File(directory, "input.wav")
                    check(directory.canonicalFile == directory && file.canonicalFile == file)
                    check(file.isFile && file.length() in 44..MAX_FILE_BYTES)
                    val reader = WavPcmReader(file, format)
                    source.set(reader)
                    if (!active.get()) { closeSource(); return@Thread }
                    check(reader.frameCount in 1..MAX_AUDIO_FRAMES)
                    val frameCount = reader.frameCount
                    main.post { beginRecognition(frameCount) }
                } catch (_: Exception) {
                    main.post { finish(INPUT_INVALID) }
                    closeSource()
                }
            }, "capture-recognition-probe-validate").apply { isDaemon = true; start() }
        }

        private fun beginRecognition(frameCount: Long) {
            if (!active.get()) return
            frames = frameCount
            validatedFormat = true
            if (!PhoneCaptureRuntimeService.isForegroundReady()) { finish(RUNTIME_NOT_READY); return }
            try {
                adapter.start(sessionId, format, null, object : SpeechRecognizerAdapter.Events {
                    override fun isCurrent() = active.get() && PhoneCaptureRuntimeService.isForegroundReady()

                    override fun onAttemptState(state: String) {
                        main.post {
                            if (!active.get()) return@post
                            when (state) {
                                "provider-ready" -> if (readyMillis < 0) readyMillis = elapsed()
                                "provider-pipe-first-write" -> if (firstWriteMillis < 0) firstWriteMillis = elapsed()
                                "provider-partial-callback" -> partialCallbacks++
                                "provider-segment-callback" -> segmentCallbacks++
                                "provider-empty-partial" -> emptyPartials++
                                "provider-empty-segment" -> emptySegments++
                                "provider-first-partial" -> if (firstPartialMillis < 0) firstPartialMillis = elapsed()
                                "provider-first-segment" -> if (firstSegmentMillis < 0) firstSegmentMillis = elapsed()
                                "provider-input-backlog-over-1s" -> backlogEvents++
                                "provider-write-blocked-250ms" -> blockedWrites++
                            }
                        }
                    }

                    override fun onHypothesis(text: String, isFinal: Boolean) {
                        val nonempty = text.isNotBlank()
                        val matches = expectedMatches(text)
                        main.post {
                            if (!active.get()) return@post
                            hypotheses++
                            if (nonempty && firstTextMillis < 0) firstTextMillis = elapsed()
                            anyExpectedMatch = anyExpectedMatch || matches
                            if (isFinal) {
                                finalObserved = true
                                expectedMatch = matches
                                finalMillis = elapsed()
                                finish(if (inputEof.get()) COMPLETED else EARLY_FINAL)
                            }
                        }
                    }

                    override fun onFailure(code: String) {
                        val number = providerErrorNumber(code)
                        val failure = when (code) {
                            "speech-provider-ended-before-source" -> EARLY_FINAL
                            "external-audio-provider-backpressure" -> BACKPRESSURE
                            "external-audio-write-failed" -> WRITE_FAILED
                            else -> if (number >= 0) PROVIDER_ERROR else SETUP_FAILED
                        }
                        main.post { finish(failure, number) }
                    }
                })
                if (!active.get()) return
                feeder = Thread({ feed() }, "capture-recognition-probe-feed").apply { isDaemon = true; start() }
            } catch (_: Exception) {
                finish(SETUP_FAILED)
            }
        }

        private fun feed() {
            try {
                val reader = source.get() ?: return
                while (active.get()) {
                    val chunk = reader.read(3_200) ?: break
                    adapter.append(sessionId, chunk)
                    val duration = chunk.size.toLong() * 1_000 / (format.sampleRate * format.channels * format.bitsPerSample / 8)
                    Thread.sleep(duration)
                }
                if (!active.get()) return
                inputEof.set(true)
                main.post { if (active.get()) inputEofMillis = elapsed() }
                adapter.finish(sessionId)
            } catch (_: Exception) {
                main.post { finish(WRITE_FAILED) }
            } finally {
                closeSource()
            }
        }

        private fun finish(status: Int, providerError: Int = -1) {
            check(Looper.myLooper() == Looper.getMainLooper())
            if (!active.compareAndSet(true, false)) return
            main.removeCallbacks(deadline)
            feeder?.interrupt()
            closeSource()
            val cancelled = runCatching { adapter.cancel(sessionId) }.isSuccess
            // Keep the reservation if native retirement failed; never admit overlapping SDK owners.
            val released = if (reserved && cancelled) {
                runCatching { coordinator?.endRecognitionProbe(sessionId) }.isSuccess
            } else !reserved
            val result = JSONObject()
                .put("status", status)
                .put("sdk", Build.VERSION.SDK_INT)
                .put("foregroundReady", foregroundReady)
                .put("foregroundReadyAtEnd", PhoneCaptureRuntimeService.isForegroundReady())
                .put("ownerReserved", reserved)
                .put("nativeRetired", cancelled)
                .put("ownerReleased", released)
                .put("onDeviceAvailable", onDeviceAvailable)
                .put("externalAudioAvailable", externalAudioAvailable)
                .put("formatSupported", formatSupported)
                .put("validatedFormat", validatedFormat)
                .put("sampleRate", format.sampleRate)
                .put("channels", format.channels)
                .put("bitsPerSample", format.bitsPerSample)
                .put("frames", frames)
                .put("partialCallbacks", partialCallbacks)
                .put("segmentCallbacks", segmentCallbacks)
                .put("emptyPartials", emptyPartials)
                .put("emptySegments", emptySegments)
                .put("hypotheses", hypotheses)
                .put("readyMillis", readyMillis)
                .put("firstWriteMillis", firstWriteMillis)
                .put("firstTextMillis", firstTextMillis)
                .put("firstPartialMillis", firstPartialMillis)
                .put("firstSegmentMillis", firstSegmentMillis)
                .put("inputEofMillis", inputEofMillis)
                .put("finalMillis", finalMillis)
                .put("elapsedMillis", elapsed())
                .put("inputEof", inputEof.get())
                .put("finalObserved", finalObserved)
                .put("expectedMatch", expectedMatch)
                .put("anyExpectedMatch", anyExpectedMatch)
                .put("success", status == COMPLETED && expectedMatch && inputEof.get() && cancelled && released &&
                    PhoneCaptureRuntimeService.isForegroundReady())
                .put("providerError", providerError)
                .put("backlogEvents", backlogEvents)
                .put("blockedWrites", blockedWrites)
            Thread({
                var stored = false
                try {
                    val output = File(directory, "result.json")
                    if (directory.canonicalFile == directory && output.canonicalFile == output &&
                        (directory.isDirectory || directory.mkdir())) {
                        output.writeText(result.toString(), Charsets.UTF_8)
                        stored = true
                    }
                } catch (_: Exception) {
                    // The ordered broadcast still returns the content-free result.
                } finally {
                    result.put("resultStored", stored)
                    pending.resultCode = status
                    pending.resultData = result.toString()
                    pending.finish()
                }
            }, "capture-recognition-probe-result").apply { isDaemon = true; start() }
        }

        private fun closeSource() { runCatching { source.getAndSet(null)?.close() } }
        private fun elapsed() = SystemClock.elapsedRealtime() - startedAt
    }

    internal companion object {
        const val ACTION = "dev.gitblame.voxgolem.capture.RECOGNITION_PROBE"
        private const val DIRECTORY = "capture-recognition-probe"
        private const val DEADLINE_MILLIS = 8_000L
        private const val MAX_FILE_BYTES = 1_048_576L
        private const val MAX_AUDIO_FRAMES = 16_000L * 5
        private const val COMPLETED = 0
        private const val RUNTIME_NOT_READY = 1
        private const val NOT_PHONE = 2
        private const val SDK_UNAVAILABLE = 3
        private const val PERMISSION_MISSING = 4
        private const val PROVIDER_UNAVAILABLE = 5
        private const val OWNER_BUSY = 6
        private const val INPUT_INVALID = 7
        private const val SETUP_FAILED = 8
        private const val TIMEOUT = 9
        private const val EARLY_FINAL = 10
        private const val BACKPRESSURE = 11
        private const val WRITE_FAILED = 12
        private const val PROVIDER_ERROR = 13

        internal fun expectedMatches(text: String): Boolean {
            val normalized = text.lowercase(Locale.ROOT)
                .replace(Regex("[^a-z0-9]+"), " ").trim().split(Regex("\\s+"))
                .map { when (it) { "alfa" -> "alpha"; "one" -> "1"; "two" -> "2"; "three" -> "3"; else -> it } }
                .joinToString(" ")
            return normalized == "alpha bravo charlie 1 2 3" || normalized == "alpha bravo charlie 123"
        }

        internal fun providerErrorNumber(code: String): Int =
            Regex("^speech-recognizer-([0-9]+)$").matchEntire(code)?.groupValues?.get(1)?.toIntOrNull() ?: -1
    }
}
