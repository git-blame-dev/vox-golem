package dev.gitblame.voxgolem.assistant.runtime

import android.content.Context
import android.content.ContextWrapper
import android.os.Looper
import dev.gitblame.voxgolem.assistant.AssistantStore
import dev.gitblame.voxgolem.assistant.AssistantTiming
import java.io.File
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.Future
import java.util.concurrent.ExecutorService
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeoutException

/** Phone and Wear use this process owner; native listeners must not create another store/engine. */
object AssistantRuntime {
    @Volatile private var engine: AssistantEngine? = null
    @Volatile private var captureAdmissionReady = false
    @Volatile private var agyRouter: AgyProviderRouter? = null
    @Volatile private var optionalReadiness: Map<String, OptionalProviderReadiness> = emptyMap()
    fun optionalProviderReadiness(): Map<String, OptionalProviderReadiness> = optionalReadiness
    internal fun publishOptionalProviderReadiness(context: Context) { optionalReadiness = AndroidProviderReadiness.inspect(context) }

    fun prewarm(model: String) {
        val currentEngine = engine
        // Without a hydrated engine, wait for policy-aware worker prewarming.
        if (currentEngine != null) {
            currentEngine.prewarmSelectionIfCurrent(model)
        }
    }

    internal fun prewarmRouter(model: String, webEnabled: Boolean) {
        agyRouter?.prewarmSelection(model, webEnabled)
    }

    /** The single provider router for this process: the engine and the watch relay must share its Agy owner. */
    @Synchronized internal fun router(application: Context): AgyProviderRouter =
        agyRouter ?: AgyProviderRouter(application).also { agyRouter = it }

    /** Enable only after Note/live/replay/probe owners all use RecognitionResourceLease.process. */
    fun configureCaptureResourceAdmission(ready: Boolean) { captureAdmissionReady = ready }
    fun captureResourceAdmissionReady(): Boolean = captureAdmissionReady

    fun refreshIfStarted() { engine?.refresh() }
    /** Explicit logout retires this request, not the foreground/lifecycle owner. */
    internal fun cancelActiveForSignOut() { engine?.stop() }
    fun idleForSyntheticSession(): Boolean = engine?.snapshot()?.let { !it.busy && !it.foreground } ?: true

    /** Fixed synthetic routes only; validate before allocating storage or provider resources. */
    fun validateSyntheticModel(model: String = "custom-luna-none"): String {
        require(model == "custom-luna-none" || model == "gemini-nano" || model == "agy-flash-low")
        return model
    }

    /** In-process instrumentation owner only; never installs itself as the process runtime. */
    fun syntheticSession(context: Context, directory: File, model: String = "custom-luna-none",
        beginTiming: (AssistantTiming.Operation) -> AssistantTiming): SyntheticSession {
        validateSyntheticModel(model)
        return isolatedSession(context, directory, model, model == "agy-flash-low", beginTiming)
    }

    /** Typed instrumentation uses real providers with test-owned policy, never user selections. */
    fun typedBenchmarkSession(context: Context, directory: File, model: String, webEnabled: Boolean,
        beginTiming: (AssistantTiming.Operation) -> AssistantTiming): SyntheticSession {
        require(model == "custom-luna-none")
        return isolatedSession(context, directory, model, webEnabled, beginTiming)
    }

    private fun isolatedSession(context: Context, directory: File, model: String, webEnabled: Boolean,
        beginTiming: (AssistantTiming.Operation) -> AssistantTiming): SyntheticSession {
        require(directory.parentFile?.canonicalFile == context.cacheDir.canonicalFile &&
            directory.name.matches(Regex("synthetic-ask-[a-f0-9-]{36}")))
        check(directory.mkdir())
        val isolated = object : ContextWrapper(context.applicationContext) {
            override fun getApplicationContext(): Context = this
            override fun getNoBackupFilesDir(): File = directory
        }
        val store = AssistantStore(isolated)
        val io = Executors.newSingleThreadExecutor { Thread(it, "synthetic-assistant-store") }
        val provider = Executors.newSingleThreadExecutor { Thread(it, "synthetic-assistant-provider") }
        // Optional providers must use the same process router as real Wear requests (phone relay).
        val worker = if (model == "custom-luna-none") AssistantRequestWorker(provider)
            else AssistantRequestWorker(provider, router(context.applicationContext))
        val credentials = ProviderAuthSource(context.applicationContext)
        val models = object : AssistantModelStorage {
            override fun read() = model
            override fun write(value: String) { require(value == read()) }
        }
        val policies = object : AssistantPolicyStorage {
            // Agy cannot enforce tool denial; use its real supported policy, never pretend web is disabled.
            private val value = AssistantPolicy(webEnabled = webEnabled, speechNetworkFallback = false,
                speechLanguage = "en-AU", speechTtsProvider = "com.google.android.tts")
            override fun read() = value
            override fun write(value: AssistantPolicy) { require(value == this.value) }
        }
        val isolatedEngine = AssistantEngine(store, io, worker, credentials::pathForProvider,
            models, credentials, policies, beginTiming = beginTiming, prewarmOnBootstrap = false)
        var retirementRequested = false
        var storeClose: Future<*>? = null
        return SyntheticSession(isolatedEngine) {
            check(Looper.myLooper() != Looper.getMainLooper())
            if (!retirementRequested) { isolatedEngine.background(); retirementRequested = true }
            worker.close()
            provider.shutdown()
            check(provider.awaitTermination(15, TimeUnit.SECONDS)) { "synthetic_provider_retirement_timeout" }
            if (storeClose == null) {
                awaitSyntheticRetirement(isolatedEngine, io, 15, TimeUnit.SECONDS)
                // Retirement publication follows final actor/store work; serialize close behind it.
                storeClose = io.submit { store.close() }
            }
            storeClose!!.get(15, TimeUnit.SECONDS)
            io.shutdown()
            check(io.awaitTermination(15, TimeUnit.SECONDS)) { "synthetic_store_retirement_timeout" }
            check(directory.deleteRecursively()) { "synthetic_store_cleanup_failed" }
        }
    }

    internal fun awaitSyntheticRetirement(engine: AssistantEngine, io: ExecutorService,
        timeout: Long, unit: TimeUnit) {
        val deadline = System.nanoTime() + unit.toNanos(timeout)
        fun remaining() = (deadline - System.nanoTime()).coerceAtLeast(0)
        try {
            // Drain hydration, queued send admission and background before trusting an idle
            // publication. The initial snapshot alone can predate all three.
            io.submit {}.get(remaining(), TimeUnit.NANOSECONDS)
            val settled = CountDownLatch(1)
            val subscription = engine.subscribe { if (!it.busy && it.contextReady) settled.countDown() }
            try {
                check(settled.await(remaining(), TimeUnit.NANOSECONDS)) { "synthetic_retirement_unconfirmed" }
            } finally { subscription.close() }
        } catch (_: TimeoutException) {
            error("synthetic_retirement_unconfirmed")
        }
    }

    class SyntheticSession internal constructor(val engine: AssistantEngine, private val cleanup: () -> Unit) : AutoCloseable {
        @Synchronized override fun close() = cleanup()
    }

    @Synchronized fun get(context: Context, startupTiming: AssistantTiming? = null, preparePhoneContext: Boolean = false): AssistantEngine {
        engine?.let { startupTiming?.mark(AssistantTiming.Stage.RUNTIME_REUSED); return it }
        startupTiming?.mark(AssistantTiming.Stage.RUNTIME_CREATE_STARTED)
        val application = context.applicationContext
        val credentials = ProviderAuthSource(application)
        val io = Executors.newSingleThreadExecutor { command -> Thread(command, "assistant-store") }
        io.execute { publishOptionalProviderReadiness(application) }
        val provider = Executors.newSingleThreadExecutor { command -> Thread(command, "assistant-provider") }
        val modelStorage = object : AssistantModelStorage {
            private val preferences by lazy { application.getSharedPreferences("assistant-runtime", Context.MODE_PRIVATE) }
            override fun read() = preferences.getString("model", "custom-sol-high") ?: "custom-sol-high"
            override fun write(value: String) { check(preferences.edit().putString("model", value).commit()) }
        }
        val router = router(application)
        val policyStorage = NativeAssistantPolicyStorage(application)
        val worker = AssistantRequestWorker(provider, router)

        return AssistantEngine(AssistantStore(application), io, worker,
            credentials::pathForProvider, modelStorage, credentials, policyStorage,
            beginTiming = { AssistantTiming.begin(application, it) }, startupTiming = startupTiming,
            preparePhoneContext = preparePhoneContext).also {
                engine = it
                startupTiming?.mark(AssistantTiming.Stage.RUNTIME_CREATED)
            }
    }
}
