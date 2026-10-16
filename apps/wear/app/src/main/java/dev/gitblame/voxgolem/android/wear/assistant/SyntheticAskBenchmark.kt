package dev.gitblame.voxgolem.android.wear.assistant

import android.Manifest
import android.app.Instrumentation
import android.content.Context
import android.content.ContextWrapper
import android.content.SharedPreferences
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.os.Looper
import android.os.Process
import android.os.SystemClock
import dev.gitblame.voxgolem.assistant.AssistantTiming
import dev.gitblame.voxgolem.assistant.AssistantDiagnosticJournal
import dev.gitblame.voxgolem.assistant.AssistantTiming.Kind
import dev.gitblame.voxgolem.assistant.AssistantTiming.Operation
import dev.gitblame.voxgolem.assistant.recognition.RecognitionResourceLease
import dev.gitblame.voxgolem.assistant.recognition.SharedDictationOwner
import dev.gitblame.voxgolem.assistant.runtime.*
import dev.gitblame.voxgolem.assistant.speech.AndroidListeningCue
import dev.gitblame.voxgolem.assistant.speech.LocalSpeechOutput
import java.io.File
import java.lang.ref.WeakReference
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger

/**
 * Same-signer, in-process instrumentation only. No exported component, intent extra, provider,
 * persistent flag or file can enable this seam. Public methods deliberately use only SDK types
 * so the debug test APK can reflect into the optimized, independently R8-minified target.
 */
object SyntheticAskBenchmark {
    private const val TARGET = "dev.gitblame.voxgolem.android"
    @Volatile private var owner: Instrumentation? = null
    @Volatile private var trial: Trial? = null
    private var consumed = false
    private var trials = 0
    private var pcm: ByteArray? = null
    private var expiresAt = 0L
    private var model = "custom-luna-none"
    private var trialLimit = 3

    internal fun parseArguments(arguments: Bundle): Pair<String, Int> {
        val model = AssistantRuntime.validateSyntheticModel(arguments.getString("model") ?: "custom-luna-none")
        val trials = arguments.getString("trials") ?: "3"
        require(trials == "1" || trials == "3")
        return model to trials.toInt()
    }

    @JvmStatic fun install(instrumentation: Instrumentation, arguments: Bundle): Bundle {
        check(Looper.myLooper() != Looper.getMainLooper())
        verify(instrumentation)
        check(arguments.getString("liveProvider") == "true" && arguments.getString("syntheticAudio") == "true")
        val selection = parseArguments(arguments)
        synchronized(this) {
            check(!consumed && owner == null && trial == null)
            check(AssistantRuntime.idleForSyntheticSession() && RecognitionResourceLease.process.current() == null)
            consumed = true
            model = selection.first
            trialLimit = selection.second
            owner = instrumentation
            expiresAt = SystemClock.elapsedRealtime() + TimeUnit.MINUTES.toMillis(10)
        }
        try { pcm = SyntheticAskFixture.create(instrumentation, instrumentation.targetContext.applicationContext) }
        catch (failure: Exception) { owner = null; throw IllegalStateException("synthetic_fixture_failed") }
        return Bundle().apply { putString("stage", "fixture_ready"); putInt("trials", trialLimit); putString("model", model) }
    }

    @JvmStatic fun prepareTrial(instrumentation: Instrumentation): Bundle {
        authorize(instrumentation)
        check(Looper.myLooper() != Looper.getMainLooper())
        check(trial == null && trials < trialLimit && RecognitionResourceLease.process.current() == null)
        val application = instrumentation.targetContext.applicationContext
        val current = Trial(application, requireNotNull(pcm), model)
        trial = current
        trials++
        return Bundle().apply { putString("stage", "armed"); putInt("trial", trials) }
    }

    @JvmStatic fun status(instrumentation: Instrumentation): Bundle {
        authorize(instrumentation)
        val current = requireNotNull(trial)
        var idle = false
        instrumentation.runOnMainSync {
            val voice = current.voice?.operationStatus()
            val state = current.handle.engine.snapshot()
            if (voice?.failed == true || state.error != null) current.failed = true
            idle = voice != null && !voice.active && !state.busy
        }
        return current.result(idle)
    }

    @JvmStatic fun finishTrial(instrumentation: Instrumentation): Bundle {
        authorize(instrumentation, allowExpired = true)
        check(Looper.myLooper() != Looper.getMainLooper())
        val current = requireNotNull(trial)
        runSyntheticCleanup(listOf(
            { instrumentation.runOnMainSync { current.activity?.get()?.finish() } },
            { instrumentation.runOnMainSync { current.voice?.close() } },
            { instrumentation.runOnMainSync { current.audio?.close() } },
            {
                instrumentation.waitForIdleSync()
                val deadline = SystemClock.elapsedRealtime() + 5_000
                var destroyed = false
                while (!destroyed && SystemClock.elapsedRealtime() < deadline) {
                    instrumentation.runOnMainSync { destroyed = current.activity?.get()?.isDestroyed != false }
                    if (!destroyed) Thread.sleep(10)
                }
                check(destroyed) { "synthetic_activity_retirement_timeout" }
            },
            {
                val deadline = SystemClock.elapsedRealtime() + 2_000
                while (current.audio?.retired() == false && SystemClock.elapsedRealtime() < deadline) Thread.sleep(10)
                check(current.audio?.retired() != false && RecognitionResourceLease.process.current() == null) {
                    "synthetic_audio_retirement_failed"
                }
            },
            { current.handle.close() },
            { current.context.cleanup() },
            { awaitJournal(instrumentation.targetContext) },
        ))
        trial = null
        return Bundle().apply { putString("stage", "trial_cleaned"); putInt("trial", trials) }
    }

    @JvmStatic fun close(instrumentation: Instrumentation): Bundle {
        authorize(instrumentation, allowExpired = true)
        if (trial != null) finishTrial(instrumentation)
        awaitJournal(instrumentation.targetContext)
        pcm?.fill(0); pcm = null
        owner = null
        return Bundle().apply { putString("stage", "cleaned"); putInt("trials", trials) }
    }

    internal fun client(activity: AssistantActivity): AssistantClient? {
        if (owner == null) return null
        check(SystemClock.elapsedRealtime() < expiresAt)
        val current = requireNotNull(trial)
        check(current.activity?.get() == null) { "synthetic_activity_already_attached" }
        current.activity = WeakReference(activity)
        return current.handle.engine
    }

    internal fun active(client: AssistantClient): Boolean = trial?.handle?.engine === client
    internal fun requireAttached(client: AssistantClient) { check(owner == null || active(client)) }
    internal fun context(client: AssistantClient): Context? = trial?.takeIf { it.handle.engine === client }?.context
    internal fun timing(context: Context, operation: Operation): AssistantTiming? = trial?.let {
        AssistantTiming.begin(context, operation, sink = it.sink)
    }

    internal fun voice(activity: AssistantActivity, client: AssistantClient): AssistantVoiceController? {
        val current = trial?.takeIf { it.handle.engine === client } ?: return null
        check(current.activity?.get() === activity && current.voice == null)
        val engine = current.handle.engine
        val factory = SyntheticAskRecognizerFactory(current.context, current.pcm,
            onFinal = { current.finals.incrementAndGet() }, onRejected = { current.failed = true },
            onObservedFinal = { matched, eof, words ->
                current.counts["fixture_words_match"] = AtomicInteger(if (matched) 1 else 0)
                current.counts["fixture_eof_observed"] = AtomicInteger(if (eof) 1 else 0)
                current.counts["fixture_word_count"] = AtomicInteger(words)
            })
        current.audio = factory
        val dictation = SharedDictationOwner(factory,
            microphoneGranted = { activity.checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED },
            configuration = { engine.snapshot().speechAdmission() },
            beginTiming = { AssistantTiming.begin(current.context, Operation.RECOGNITION, sink = current.sink) })
        return WearAssistantVoice(engine, dictation,
            LocalSpeechOutput.configured(current.context) { engine.snapshot().speechAdmission() },
            AssistantEnginePort(engine), admissionReady = { true }, language = "en-AU",
            listeningCue = AndroidListeningCue(current.context), context = current.context).also { current.voice = it }
    }

    private fun authorize(instrumentation: Instrumentation, allowExpired: Boolean = false) {
        check(owner === instrumentation && (allowExpired || SystemClock.elapsedRealtime() < expiresAt))
    }

    private fun awaitJournal(context: Context) {
        check(Looper.myLooper() != Looper.getMainLooper())
        val journal = AssistantDiagnosticJournal.forContext(context)
        val deadline = SystemClock.elapsedRealtime() + 5_000
        while (!journal.isIdle() && SystemClock.elapsedRealtime() < deadline) Thread.sleep(10)
        check(journal.isIdle()) { "synthetic_journal_drain_timeout" }
    }

    private fun verify(instrumentation: Instrumentation) {
        val context = instrumentation.targetContext
        val test = instrumentation.context
        check(context.packageName == TARGET && context.applicationInfo.uid == Process.myUid())
        check(test.packageName == "$TARGET.test" && instrumentation.componentName.packageName == test.packageName)
        val pm = context.packageManager
        check(pm.getInstrumentationInfo(instrumentation.componentName, 0).targetPackage == TARGET)
        check(pm.checkSignatures(TARGET, test.packageName) == PackageManager.SIGNATURE_MATCH)
        check(Build.MODEL == "SM-L315F" && Build.MANUFACTURER.equals("samsung", true) && Build.VERSION.SDK_INT == 36)
        check(pm.hasSystemFeature(PackageManager.FEATURE_WATCH) && !Build.FINGERPRINT.contains("generic", true))
        check(context.checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED)
    }

    private class Trial(application: Context, val pcm: ByteArray, val model: String) {
        val counts = ConcurrentHashMap<String, AtomicInteger>()
        val observed = ConcurrentHashMap<String, Long>()
        val finals = AtomicInteger()
        @Volatile var failed = false
        var voice: WearAssistantVoice? = null
        var audio: SyntheticAskRecognizerFactory? = null
        var activity: WeakReference<AssistantActivity>? = null
        private val journal = AssistantDiagnosticJournal.forContext(application)
        val sink = object : AssistantTiming.MetadataSink {
            override fun emit(event: AssistantTiming.Event) {
                journal.offer(event)
                if (event.kind == Kind.BEGIN && event.operation == Operation.GENERATION)
                    counts.computeIfAbsent("generation", { AtomicInteger() }).incrementAndGet()
                val stage = event.stage
                if (event.kind == Kind.MARK && stage != null) {
                    val key = "${event.operation.name}_${stage.name}"
                    counts.computeIfAbsent(key, { AtomicInteger() }).incrementAndGet()
                    observed.putIfAbsent(key, event.observedNanos)
                }
            }
            override fun emitMetadata(operation: Operation, run: Int, parentRun: Int?, observedNanos: Long,
                metadata: AssistantDiagnosticJournal.Metadata) = journal.offerMetadata(operation, run, parentRun, observedNanos, metadata)
        }
        private val directory = File(application.cacheDir, "synthetic-ask-${UUID.randomUUID()}")
        val handle = AssistantRuntime.syntheticSession(application, directory, model) {
            AssistantTiming.begin(application, it, sink = sink)
        }
        val context = ScopedContext(application, directory)
        private fun count(key: String) = counts[key]?.get() ?: 0
        fun result(idle: Boolean): Bundle {
            // Engine SETTLED follows AssistantRequestWorker.onRetired, not just provider outcome receipt.
            val complete = !failed && finals.get() == 1 && count("generation") == 1 &&
                count("GENERATION_DURABLE_COMMITTED") == 1 && count("GENERATION_SETTLED") == 1 &&
                count("PRESENTATION_TEXT_DRAW_SUBMITTED") >= 1 && count("TTS_UTTERANCE_ON_START") >= 1 &&
                count("TTS_UTTERANCE_ON_DONE") >= 1 && count("VOICE_FLOW_SETTLED") == 1 && idle
            return Bundle().apply {
                putString("model", model)
                putString("stage", if (failed) "failed" else if (complete) "complete" else "running")
                putInt("finals", finals.get()); putBoolean("idle", idle)
                counts.forEach { (key, value) -> putInt(key, value.get()) }
                // Same process monotonic observer; no response text, IDs or credentials are returned.
                val origin = observed["ACTIVITY_ACTIVITY_CREATED"]
                if (origin != null) observed.forEach { (key, value) ->
                    putLong("${key}_ms", TimeUnit.NANOSECONDS.toMillis(value - origin))
                }
            }
        }
    }

    /** Only random test-owned preference names/files are ever read or written by voice SDK adapters. */
    private class ScopedContext(base: Context, private val directory: File) : ContextWrapper(base) {
        private val names = mutableSetOf<String>()
        override fun getApplicationContext(): Context = this
        override fun getCacheDir(): File = directory
        override fun getNoBackupFilesDir(): File = directory
        @Synchronized override fun getSharedPreferences(name: String, mode: Int): SharedPreferences {
            require(name.matches(Regex("[a-zA-Z0-9_.-]{1,100}")))
            val isolated = "${directory.name}-$name"
            names += isolated
            return super.getSharedPreferences(isolated, Context.MODE_PRIVATE)
        }
        @Synchronized fun cleanup() { names.forEach { check(baseContext.deleteSharedPreferences(it)) }; names.clear() }
    }
}

internal fun runSyntheticCleanup(stages: List<() -> Unit>) {
    var failure: Exception? = null
    stages.forEach { stage ->
        try { stage() } catch (error: Exception) { if (failure == null) failure = error }
    }
    failure?.let { throw it }
}
