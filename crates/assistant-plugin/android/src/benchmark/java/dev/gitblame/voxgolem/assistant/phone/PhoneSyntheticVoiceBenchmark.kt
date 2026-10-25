package dev.gitblame.voxgolem.assistant.phone

import android.Manifest
import android.app.Activity
import android.app.ActivityManager
import android.app.Instrumentation
import android.content.Context
import android.content.ContextWrapper
import android.content.SharedPreferences
import android.content.pm.ApplicationInfo
import android.content.pm.PackageManager
import android.os.*
import android.webkit.WebView
import dev.gitblame.voxgolem.assistant.AssistantTiming
import dev.gitblame.voxgolem.assistant.AssistantDiagnosticJournal
import dev.gitblame.voxgolem.assistant.recognition.*
import dev.gitblame.voxgolem.assistant.runtime.*
import dev.gitblame.voxgolem.assistant.speech.*
import dev.gitblame.voxgolem.benchmark.android.*
import org.json.JSONObject
import java.io.File
import java.lang.ref.WeakReference
import java.security.MessageDigest
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.CountDownLatch

/** SDK-only reflection facade. This entire source set is absent from ordinary shipping builds. */
@android.annotation.TargetApi(35)
object PhoneSyntheticVoiceBenchmark {
    private const val TARGET = "dev.gitblame.voxgolem.android"
    private const val COMPONENT = "dev.gitblame.voxgolem.android.speechtest.PhoneSyntheticVoiceInstrumentation"
    private var consumed = false
    @Volatile private var owner: Instrumentation? = null
    private var expires = 0L
    @Volatile private var trial: Trial? = null
    private var pcm: ByteArray? = null
    private var contract: BenchmarkVoiceContract? = null
    private var contractHash = ""
    private var token = ""
    private var profile: PhoneBenchmarkProfile? = null
    private var readinessOnly = false
    private var constructionFailed = false
    @Volatile private var processOwner: Instrumentation? = null

    /** Called synchronously before Instrumentation.start/Application.onCreate, not from an Intent. */
    @JvmStatic fun reserveProcess(instrumentation: Instrumentation) = reserveProcess(instrumentation, "trial")

    @JvmStatic fun reserveProcess(instrumentation: Instrumentation, mode: String) {
        verifyIdentity(instrumentation)
        require(mode in setOf("trial", "readiness", "cleanup", "cleanup-pending"))
        check(processOwner == null && !consumed && AssistantRuntime.uninitializedForSyntheticSession())
        if (mode == "cleanup" || mode == "cleanup-pending") {
            PhoneActivityBindings.install { error("cleanup_activity_denied") }
        }
        processOwner = instrumentation
    }

    internal fun processReserved() = processOwner != null

    @JvmStatic fun install(instrumentation: Instrumentation, arguments: Bundle): Bundle {
        val diagnostic = arguments.getString("mode") == "readiness"
        verify(instrumentation, requireMicrophone = !diagnostic)
        check(Looper.myLooper() != Looper.getMainLooper() && processOwner === instrumentation)
        instrumentation.runOnMainSync { check(instrumentation.targetContext.applicationContext is PhoneBenchmarkApplication) }
        if (diagnostic) require(arguments.keySet() == setOf("mode", "contractSha")) else {
            require(arguments.keySet() == setOf("liveProvider", "syntheticAudio", "contractSha", "model", "trials"))
            require(arguments.getString("liveProvider") == "true" && arguments.getString("syntheticAudio") == "true")
            require(arguments.getString("model") == "custom-luna-none" && arguments.getString("trials") == "1")
        }
        val expected = arguments.getString("contractSha") ?: error("missing_contract")
        require(expected.matches(Regex("[a-f0-9]{64}")))
        check(!consumed && owner == null && trial == null && AssistantRuntime.uninitializedForSyntheticSession())
        check(RecognitionResourceLease.process.current() == null)
        consumed = true
        readinessOnly = diagnostic
        val context = instrumentation.targetContext.applicationContext
        val bytes = context.assets.open("voxgolem-benchmark/arithmetic-voice.json").use { it.readNBytes(4097) }
        require(bytes.size in 1..4096 && sha(bytes) == expected)
        contract = BenchmarkVoiceContract.parse(JSONObject(bytes.toString(Charsets.UTF_8)))
        contractHash = expected
        token = UUID.randomUUID().toString()
        profile = PhoneBenchmarkProfile(context)
        val suffix = profile!!.reserve(token)
        // Never recover from an already initialized WebView by opening the user profile.
        WebView.setDataDirectorySuffix(suffix)
        check(profile!!.nativeDirectory(token).mkdir())
        owner = instrumentation
        expires = SystemClock.elapsedRealtime() + TimeUnit.MINUTES.toMillis(10)
        PhoneActivityBindings.install { activity -> attach(activity) }
        pcm = if (diagnostic) ByteArray(0) else LocalFixtureSynthesis.create(instrumentation,
            ScopedContext(context, profile!!.nativeDirectory(token)), contract!!.phrase,
            profile!!.nativeDirectory(token)).also {
            require(it.isNotEmpty() && it.size % 2 == 0 && it.size <= 336_000)
        }
        return Bundle().apply {
            putInt("synthetic_protocol", 2); putString("stage", if (diagnostic) "readiness_ready" else "fixture_ready")
            putString("contract_sha256", contractHash); putString("model", "custom-luna-none"); putInt("trials", if (diagnostic) 0 else 1)
            putString("cleanup_token", token)
        }
    }

    @JvmStatic fun prepareTrial(instrumentation: Instrumentation): Bundle {
        authorize(instrumentation)
        check(trial == null && AssistantRuntime.uninitializedForSyntheticSession() && RecognitionResourceLease.process.current() == null)
        val current = try { Trial(instrumentation.targetContext.applicationContext, requireNotNull(pcm), requireNotNull(contract), readinessOnly) }
            catch (failure: Exception) { constructionFailed = true; throw failure }
        trial = current
        current.prepare()
        if (readinessOnly) {
            // Prepare the empty context while backgrounded: createNewConversation otherwise prewarms.
            val ready = CountDownLatch(1)
            current.requestProjection { ready.countDown() }
            check(ready.await(10, TimeUnit.SECONDS) && current.projectionObserved && !current.failed)
            current.projectionObserved = false // Reobserve the production projection after the WebView attaches.
        }
        return Bundle().apply { putString("stage", "armed") }
    }

    private fun attach(activity: Activity): PhoneActivityBinding {
        check(Looper.myLooper() == Looper.getMainLooper())
        val current = requireNotNull(trial)
        check(current.isRetiring() || owner != null && SystemClock.elapsedRealtime() < expires)
        check(activity.javaClass.name == "$TARGET.MainActivity")
        current.attachActivity(activity)
        return current
    }

    @JvmStatic fun status(instrumentation: Instrumentation): Bundle {
        authorize(instrumentation)
        val current = requireNotNull(trial)
        if (!current.projectionObserved && current.webView?.get() != null) current.requestProjection()
        if (!current.profileObserved && current.webView?.get() != null) {
            // Runtime mapping proof precedes the only Ask click and provider admission.
            current.profileObserved = profile!!.observe(token)
        }
        var idle = false
        instrumentation.runOnMainSync {
            val speech = current.voice
            val state = current.handle.engine.snapshot()
            if (state.error != null || (!current.diagnostic && speech?.status()?.isNull("error") == false)) current.failed = true
            idle = speech != null && !speech.active() && !state.busy
            if (current.profileObserved && !current.failed) current.observeDom()
        }
        return current.result(idle)
    }

    @JvmStatic fun close(instrumentation: Instrumentation): Bundle {
        authorize(instrumentation, expiredAllowed = true)
        val current = trial
        var failure: Exception? = null
        fun stage(action: () -> Unit) { try { action() } catch (error: Exception) { if (failure == null) failure = error } }
        stage { instrumentation.runOnMainSync { current?.retireIngress() } }
        stage { instrumentation.runOnMainSync { current?.voice?.close() } }
        stage { instrumentation.runOnMainSync { current?.dictationOwner?.close() } }
        stage { instrumentation.runOnMainSync { current?.output?.close() } }
        stage { await(5_000) {
            var retired = false
            instrumentation.runOnMainSync { retired = current?.output?.isRetired() != false }
            retired
        } }
        stage { instrumentation.runOnMainSync { current?.audio?.close() } }
        stage { await(5_000) { current?.audio?.retired() != false && RecognitionResourceLease.process.current() == null } }
        stage { current?.handle?.close() }
        stage { current?.context?.cleanup() }
        // A failed close retains the binding and resource leases. It never authorizes another trial.
        if (failure != null || constructionFailed) throw IllegalStateException("native_retirement_failed")
        if (current != null && !current.profileObserved && current.webView?.get() != null) {
            current.profileObserved = profile!!.observe(token)
        }
        // Retain the sealed binding and lifetime file lock until process exit; never fall back to ordinary startup.
        // Activity.finish() destroys Tauri's last window and can exit before Instrumentation.finish().
        pcm?.fill(0); pcm = null
        owner = null
        return Bundle().apply {
            putInt("synthetic_protocol", 2); putString("stage", "native_resources_retired")
            putBoolean("native_resources_retired", true); putBoolean("tts_retired", true)
            putBoolean("activity_webview_retirement_pending", true); putBoolean("native_cleanup_verified", false)
            putBoolean("profile_cleanup_pending", true); putBoolean("cleanup_verified", false)
            putString("cleanup_token", token)
        }
    }

    @JvmStatic fun cleanup(instrumentation: Instrumentation, arguments: Bundle): Bundle = phoneCleanupReceipt {
        verify(instrumentation, requireMicrophone = false)
        check(Looper.myLooper() != Looper.getMainLooper() && processOwner === instrumentation)
        instrumentation.runOnMainSync { check(instrumentation.targetContext.applicationContext is PhoneBenchmarkApplication) }
        val pending = arguments.getString("mode") == "cleanup-pending"
        require(if (pending) arguments.keySet() == setOf("mode") else
            arguments.keySet() == setOf("mode", "cleanupToken") && arguments.getString("mode") == "cleanup")
        check(!consumed && owner == null && trial == null && AssistantRuntime.uninitializedForSyntheticSession())
        check(RecognitionResourceLease.process.current() == null)
        consumed = true
        if (pending) return@phoneCleanupReceipt PhoneBenchmarkProfile(instrumentation.targetContext).cleanupPending().apply {
            putBoolean("activity_webview_retired", getBoolean("producer_exit_verified"))
        }
        PhoneBenchmarkProfile(instrumentation.targetContext).cleanup(arguments.getString("cleanupToken") ?: error("missing_token"))
    }

    private fun authorize(instrumentation: Instrumentation, expiredAllowed: Boolean = false) {
        check(Looper.myLooper() != Looper.getMainLooper())
        check(owner === instrumentation && (expiredAllowed || SystemClock.elapsedRealtime() < expires))
    }

    private fun verifyIdentity(instrumentation: Instrumentation) {
        val context = instrumentation.targetContext
        val test = instrumentation.context
        val pm = context.packageManager
        check(context.packageName == TARGET && context.applicationInfo.uid == android.os.Process.myUid())
        check(test.packageName == "$TARGET.test" && instrumentation.componentName.packageName == test.packageName && instrumentation.componentName.className == COMPONENT)
        check(pm.getInstrumentationInfo(instrumentation.componentName, 0).targetPackage == TARGET)
        check(pm.checkSignatures(TARGET, test.packageName) == PackageManager.SIGNATURE_MATCH)
    }

    private fun verify(instrumentation: Instrumentation, requireMicrophone: Boolean = true) {
        verifyIdentity(instrumentation)
        val context = instrumentation.targetContext
        val pm = context.packageManager
        check(context.applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE == 0)
        check(Build.MODEL == "SM-A176B" && Build.MANUFACTURER.equals("samsung", true) && Build.VERSION.SDK_INT >= 35)
        check("arm64-v8a" in Build.SUPPORTED_ABIS && !pm.hasSystemFeature(PackageManager.FEATURE_WATCH) && !Build.FINGERPRINT.contains("generic", true))
        check(!requireMicrophone || context.checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED)
        val manager = context.getSystemService(Context.ACTIVITY_SERVICE) as ActivityManager
        check(manager.runningAppProcesses.orEmpty().none { it.uid == android.os.Process.myUid() && it.pid != android.os.Process.myPid() })
    }

    private class Trial(application: Context, val pcm: ByteArray, val contract: BenchmarkVoiceContract,
        val diagnostic: Boolean) : PhoneActivityBinding {
        val counts = ConcurrentHashMap<String, AtomicInteger>()
        val observed = ConcurrentHashMap<String, Long>()
        @Volatile var failed = false
        @Volatile var activity: WeakReference<Activity>? = null
        @Volatile var webView: WeakReference<WebView>? = null
        @Volatile var profileObserved = false
        var audio: FixtureRecognizerFactory? = null
        var output: LocalSpeechOutput? = null
        var voice: PhoneSpeechSession? = null
        var dictationOwner: SharedDictationOwner? = null
        private val admission = PhoneBenchmarkAdmission(contract, liveAllowed = !diagnostic)
        private val provenance = if (diagnostic) Bundle() else fixturePcmProvenance(pcm)
        @Volatile var projectionObserved = false
        @Volatile private var modelAvailable = false
        @Volatile private var credentialReason = "unknown"
        private val projectionPending = AtomicBoolean()
        private var domObserved = false
        private var askCount = 0
        private var askEnabled = false
        private var documentVisible = false
        private var frontendReady = false
        private var uiModelMatches = false
        private var uiModelAvailable = false
        @Volatile private var requestId: String? = null
        @Volatile private var canonicalAnswer: String? = null
        private var uiEqual = false
        private var uiRows = 0
        private var clicked = false
        private var domPending = false
        private val directory = requireNotNull(profile).nativeDirectory(token)
        private val applicationContext = application
        private val journal = AssistantDiagnosticJournal.forContext(application)
        private val sink = object : AssistantTiming.MetadataSink {
            override fun emit(event: AssistantTiming.Event) {
                journal.offer(event)
                if (event.kind == AssistantTiming.Kind.BEGIN && event.operation == AssistantTiming.Operation.GENERATION) increment("generation")
                val stage = event.stage
                if (event.kind == AssistantTiming.Kind.MARK && stage != null) {
                    if (stage == AssistantTiming.Stage.RETIREMENT_FAILED) failed = true
                    val key = "${event.operation.name}_${stage.name}"
                    increment(key); observed.putIfAbsent(key, event.observedNanos)
                }
            }
            override fun emitMetadata(operation: AssistantTiming.Operation, run: Int, parentRun: Int?, observedNanos: Long,
                metadata: AssistantDiagnosticJournal.Metadata) = journal.offerMetadata(operation, run, parentRun, observedNanos, metadata)
        }
        val handle = AssistantRuntime.registeredSyntheticSession(application, directory) { timing(it) }
        val context = ScopedContext(application, directory)
        private lateinit var preparedBackend: EnginePhoneBackend
        override val backend: EnginePhoneBackend get() = preparedBackend
        override val speech: PhoneSpeechSession get() = requireNotNull(voice)

        fun prepare() {
            check(!::preparedBackend.isInitialized)
            PhonePreferences(context).apply { initialize() }.write(PhonePreferences.defaults().put("instant", "custom-luna-none").put("tts_enabled", !diagnostic))
            preparedBackend = EnginePhoneBackend(context, handle.engine, prewarm = {}, enginePrewarm = {}, reconcile = {}, commandAdmission = { name ->
                check(name in setOf("get_app_snapshot", "save_assistant_draft", "cancel_prompt"))
            })
        }
        fun requestProjection(done: () -> Unit = {}) {
            if (admission.isRetiring()) return
            if (!projectionPending.compareAndSet(false, true)) return
            backend.command("get_app_snapshot", JSONObject()) { result ->
                try {
                    val snapshot = result.getOrThrow() as JSONObject
                    modelAvailable = selectedModelAvailable(snapshot)
                    credentialReason = safeCredentialReason(handle.engine.snapshot())
                    projectionObserved = true
                } catch (_: Exception) { failed = true }
                finally { projectionPending.set(false); done() }
            }
        }
        fun retireIngress() {
            check(Looper.myLooper() == Looper.getMainLooper())
            admission.retire()
            if (::preparedBackend.isInitialized) preparedBackend.retireIngress()
        }
        fun isRetiring() = admission.isRetiring()
        fun attachActivity(value: Activity) {
            check(Looper.myLooper() == Looper.getMainLooper())
            val active = admission.attachResources {
                check(activity == null)
                activity = WeakReference(value)
                mark("ACTIVITY_ACTIVITY_CREATED")
                constructSpeech()
            }
            if (!active && activity == null) activity = WeakReference(value)
        }
        fun timing(operation: AssistantTiming.Operation) = AssistantTiming.begin(contextOrApplication(), operation, sink = sink)
        // Called while the isolated engine is being constructed, before scoped preferences exist.
        private fun contextOrApplication(): Context = applicationContext

        fun constructSpeech() {
            check(Looper.myLooper() == Looper.getMainLooper() && !admission.isRetiring() && voice == null)
            val factory = FixtureRecognizerFactory(context, pcm, contract,
                onFinal = { increment("finals") }, onRejected = { failed = true },
                onObservedFinal = { match, eof, words ->
                    counts["fixture_words_match"] = AtomicInteger(if (match) 1 else 0)
                    counts["fixture_eof_observed"] = AtomicInteger(if (eof) 1 else 0)
                    counts["fixture_word_count"] = AtomicInteger(words)
                })
            audio = factory
            val dictation = SharedDictationOwner(factory, { context.checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED },
                configuration = { handle.engine.snapshot().speechAdmission() }, beginTiming = { timing(AssistantTiming.Operation.RECOGNITION) })
            dictationOwner = dictation
            val playback = LocalSpeechOutput(AndroidLocalTtsFactory(context), AndroidSpeechAudioFocus(context),
                configuration = { handle.engine.snapshot().speechAdmission() }, beginTiming = { timing(it) })
            output = playback
            voice = PhoneSpeechSession(context, backend, suppliedDictation = dictation, suppliedOutput = playback,
                beginFlow = { timing(AssistantTiming.Operation.VOICE_FLOW) })
        }

        override fun webViewCreated(webView: WebView) {
            check(admission.isRetiring() || this.webView == null)
            if (this.webView == null) this.webView = WeakReference(webView)
        }
        override fun admitVoice(policy: String, draft: String, loop: Boolean, shortcut: String?) {
            admission.voice(profileObserved && modelAvailable && !failed && SystemClock.elapsedRealtime() < expires, policy, draft, loop, shortcut)
        }
        override fun admitPrompt(requestId: String, prompt: String) {
            admission.prompt(profileObserved && modelAvailable && !failed && SystemClock.elapsedRealtime() < expires, count("finals"), requestId, prompt)
            this.requestId = requestId
            increment("frontend_submit_observed"); mark("FRONTEND_SUBMIT_OBSERVED")
        }
        override fun terminal(requestId: String, result: Result<Any?>) {
            if (this.requestId != requestId) { failed = true; return }
            val value = result.getOrNull() as? JSONObject
            if (value == null || value.optString("kind") != "completed" || value.has("warning")) { failed = true; return }
            canonicalAnswer = runCatching { value.getJSONObject("turn").getJSONObject("answer").getJSONObject("content").getString("text").trim() }.getOrNull()
            if (canonicalAnswer != contract.answer) failed = true
        }
        fun observeDom() {
            if (admission.isRetiring()) return
            if (domPending) return
            val view = webView?.get() ?: return
            domPending = true
            val id = requestId
            val inspectOnly = !clicked && (diagnostic || !admission.allowsClick(modelAvailable, frontendReady))
            val script = if (inspectOnly) READINESS_PROGRAM else if (!clicked) ASK_PROGRAM else if (id != null && canonicalAnswer != null) answerProgram(id, contract.answer) else { domPending = false; return }
            view.evaluateJavascript(script) { raw ->
                domPending = false
                if (admission.isRetiring()) return@evaluateJavascript
                try {
                    check(raw != null && raw.length <= 512)
                    val value = JSONObject(raw)
                    if (inspectOnly) {
                        check(value.keys().asSequence().toSet() == setOf("count", "enabled", "visible", "ready", "modelMatches", "modelAvailable"))
                        check(value.opt("count") is Int && value.getInt("count") in 0..2)
                        for (key in listOf("enabled", "visible", "ready", "modelMatches", "modelAvailable")) check(value.opt(key) is Boolean)
                        askCount = value.getInt("count"); askEnabled = value.getBoolean("enabled")
                        documentVisible = value.getBoolean("visible"); frontendReady = value.getBoolean("ready")
                        uiModelMatches = value.getBoolean("modelMatches"); uiModelAvailable = value.getBoolean("modelAvailable")
                        domObserved = true
                    } else if (!clicked) {
                        check(value.keys().asSequence().toSet() == setOf("clicked", "count"))
                        check(value.opt("clicked") is Boolean && value.opt("count") is Int && value.getInt("count") in 0..2)
                        if (value.getBoolean("clicked")) { check(value.getInt("count") == 1); clicked = true; mark("FRONTEND_ASK_CLICKED") }
                    } else {
                        check(value.keys().asSequence().toSet() == setOf("rows", "exact"))
                        check(value.opt("exact") is Boolean && value.opt("rows") is Int)
                        uiRows = value.getInt("rows"); check(uiRows in 0..2)
                        uiEqual = value.getBoolean("exact") && uiRows == 1
                        if (uiEqual) mark("PRESENTATION_DOM_OBSERVED")
                    }
                } catch (_: Exception) { failed = true }
            }
        }
        private fun increment(key: String) { counts.computeIfAbsent(key) { AtomicInteger() }.incrementAndGet() }
        fun mark(key: String) { observed.putIfAbsent(key, SystemClock.elapsedRealtimeNanos()) }
        private fun count(key: String) = counts[key]?.get() ?: 0
        fun result(idle: Boolean): Bundle {
            val complete = !failed && count("finals") == 1 && count("generation") == 1 && count("frontend_submit_observed") == 1 &&
                count("GENERATION_DURABLE_COMMITTED") == 1 && count("GENERATION_SETTLED") == 1 && count("VOICE_FLOW_SETTLED") == 1 &&
                count("TTS_UTTERANCE_ON_START") >= 1 && count("TTS_UTTERANCE_ON_DONE") >= 1 && canonicalAnswer == contract.answer && uiEqual && idle
            return Bundle().apply {
                val diagnosed = diagnostic && projectionObserved && domObserved && askCount == 1 && uiModelMatches && profileObserved
                putInt("synthetic_protocol", 2)
                if (!diagnostic) putInt("synthetic_trial", 1)
                putString("stage", if (failed) "failed" else if (diagnosed) "readiness_observed" else if (complete) "complete" else "running")
                putString("model", "custom-luna-none"); putString("contract_sha256", contractHash)
                putBoolean("profile_mapping_verified", profileObserved); putBoolean("frontend_ask_clicked", clicked)
                putString("credential_reason", credentialReason); putBoolean("model_projection_observed", projectionObserved)
                putBoolean("private_model_available", modelAvailable); putBoolean("frontend_ready_to_send", frontendReady)
                putBoolean("ui_selected_model_matches", uiModelMatches); putBoolean("ui_selected_model_available", uiModelAvailable)
                putInt("ask_present_count", askCount); putBoolean("ask_enabled", askEnabled); putBoolean("document_visible", documentVisible)
                putInt("voice_admission_budget", if (diagnostic) 0 else 1); putInt("model_admission_budget", if (diagnostic) 0 else 1)
                putInt("voice_admissions", admission.voiceAdmissions()); putInt("prompt_admissions", admission.promptAdmissions())
                putInt("generation", count("generation")); putInt("frontend_submit_observed", count("frontend_submit_observed"))
                counts.forEach { (key, value) -> putInt(key, value.get()) }
                putBoolean("canonical_answer_exact", canonicalAnswer == contract.answer); putBoolean("canonical_ui_equal", uiEqual)
                putInt("ui_correlated_rows", uiRows); putBoolean("idle", idle)
                putAll(provenance)
                observed["ACTIVITY_ACTIVITY_CREATED"]?.let { origin -> observed.forEach { (key, time) -> putLong("${key}_ms", TimeUnit.NANOSECONDS.toMillis(time - origin)) } }
            }
        }
    }

    internal fun selectedModelAvailable(snapshot: JSONObject): Boolean {
        if (snapshot.getJSONObject("preferences").getString("instant") != "custom-luna-none") return false
        val options = snapshot.getJSONArray("model_options")
        require(options.length() <= 128)
        val selected = (0 until options.length()).map { options.getJSONObject(it) }.filter { it.getString("id") == "custom-luna-none" }
        return selected.size == 1 && selected.single().opt("available") == true
    }
    internal fun safeCredentialReason(state: AssistantState): String =
        state.credential?.reason?.name?.lowercase(java.util.Locale.ROOT) ?: "unknown"

    internal class ScopedContext(base: Context, private val directory: File) : ContextWrapper(base),
        dev.gitblame.voxgolem.voice.AndroidBackgroundAdmissionOwner {
        override val backgroundServicesEnabled = false
        private val names = mutableSetOf<String>()
        override fun getApplicationContext(): Context = this
        override fun getCacheDir(): File = directory
        override fun getNoBackupFilesDir(): File = directory
        @Synchronized override fun getSharedPreferences(name: String, mode: Int): SharedPreferences {
            require(name in PhoneBenchmarkProfile.PREFERENCE_NAMES)
            val scoped = "${directory.name}-$name"; names += scoped
            return super.getSharedPreferences(scoped, Context.MODE_PRIVATE)
        }
        fun cleanup() { names.forEach { check(baseContext.deleteSharedPreferences(it)) }; names.clear() }
    }

    private const val ASK_PROGRAM = """(()=>{const a=[...document.querySelectorAll('button[aria-label="Ask"][data-voice-mode="idle"]')].filter(e=>!e.disabled&&e.getClientRects().length>0);const s=[...document.querySelectorAll('select')].filter(e=>e.value==='custom-luna-none');const ready=document.documentElement.dataset.appReadyToSendMs!==undefined&&s.length>0&&s.length<=4&&s.every(e=>e.selectedIndex>=0&&!e.options[e.selectedIndex].disabled);const ok=ready&&document.visibilityState==='visible'&&a.length===1;if(ok)a[0].click();return {clicked:ok,count:a.length};})()"""
    private const val READINESS_PROGRAM = """(()=>{const a=[...document.querySelectorAll('button[aria-label="Ask"][data-voice-mode="idle"]')].filter(e=>e.getClientRects().length>0);const s=[...document.querySelectorAll('select')].filter(e=>e.value==='custom-luna-none');return {count:a.length,enabled:a.length===1&&!a[0].disabled,visible:document.visibilityState==='visible',ready:document.documentElement.dataset.appReadyToSendMs!==undefined,modelMatches:s.length>0&&s.length<=4,modelAvailable:s.length>0&&s.length<=4&&s.every(e=>e.selectedIndex>=0&&!e.options[e.selectedIndex].disabled)};})()"""
    private fun answerProgram(id: String, expected: String): String {
        require(id.matches(Regex("voice-[a-f0-9-]{36}")) && expected.matches(Regex("[0-9]{1,8}")))
        return """(()=>{const r=[...document.querySelectorAll('[data-turn-role="assistant"][data-turn-outcome="completed"]')].filter(e=>e.getAttribute('data-request-id')===${JSONObject.quote(id)});const p=r.length===1?r[0].querySelector('.message__content,.answer-stage__answer'):null;return {rows:r.length,exact:p!==null&&p.getClientRects().length>0&&p.textContent.trim()===${JSONObject.quote(expected)}};})()"""
    }
    private fun sha(bytes: ByteArray) = MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it.toInt() and 255) }
    private fun await(timeout: Long, condition: () -> Boolean) {
        val deadline = SystemClock.elapsedRealtime() + timeout
        while (!condition()) { check(SystemClock.elapsedRealtime() < deadline); Thread.sleep(10) }
    }
}
