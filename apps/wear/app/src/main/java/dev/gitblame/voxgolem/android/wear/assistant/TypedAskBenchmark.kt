package dev.gitblame.voxgolem.android.wear.assistant

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
import dev.gitblame.voxgolem.assistant.runtime.*
import java.io.File
import java.lang.ref.WeakReference
import java.util.UUID
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import org.json.JSONObject

/** Only verified same-signer instrumentation can allocate this test-owned runtime. */
object TypedAskBenchmark {
    private const val TARGET = "dev.gitblame.voxgolem.android"
    @Volatile private var owner: Instrumentation? = null
    @Volatile private var session: AssistantRuntime.SyntheticSession? = null
    private var activity: WeakReference<AssistantActivity>? = null
    private var scoped: ScopedContext? = null
    private var expires = 0L
    private var consumed = false
    private var turns = 0
    private var maxTurns = 0
    private var prompt = ""
    private var baseline = 0
    private var terminalFailure = false
    private val sendPermit = TypedBenchmarkSendPermit()

    internal fun validateArguments(arguments: Bundle): Triple<String, Int, Int> {
        val run = arguments.getString("run") ?: error("missing_run")
        require(UUID.fromString(run).toString() == run)
        require(arguments.getString("liveProvider") == "true")
        require((arguments.getString("model") ?: "custom-luna-none") == "custom-luna-none")
        val limit = (arguments.getString("maxTurns") ?: "7").toInt()
        val turnTimeout = (arguments.getString("turnTimeoutSeconds") ?: "120").toInt()
        require(limit in 1..7 && turnTimeout in 10..180)
        val timeout = arguments.getString("timeout")?.toInt() ?: (limit * (turnTimeout + 30) + 180)
        require(timeout in 1..1800)
        return Triple(run, limit, timeout)
    }

    @JvmStatic fun install(instrumentation: Instrumentation, arguments: Bundle): Bundle {
        check(Looper.myLooper() != Looper.getMainLooper())
        val (run, limit, timeout) = validateArguments(arguments)
        val target = instrumentation.targetContext
        val test = instrumentation.context
        val pm = target.packageManager
        check(target.packageName == TARGET && target.applicationInfo.uid == Process.myUid())
        check(test.packageName == "$TARGET.test" && instrumentation.componentName.packageName == test.packageName)
        check(pm.getInstrumentationInfo(instrumentation.componentName, 0).targetPackage == TARGET)
        check(pm.checkSignatures(TARGET, test.packageName) == PackageManager.SIGNATURE_MATCH)
        check(target.applicationInfo.flags and android.content.pm.ApplicationInfo.FLAG_DEBUGGABLE == 0)
        check(Build.MODEL == "SM-L315F" && Build.MANUFACTURER.equals("samsung", true) && Build.VERSION.SDK_INT == 36)
        check(pm.hasSystemFeature(PackageManager.FEATURE_WATCH) && !Build.FINGERPRINT.contains("generic", true))
        synchronized(this) {
            check(!consumed && owner == null && AssistantRuntime.idleForSyntheticSession())
            consumed = true
            owner = instrumentation
            maxTurns = limit
            expires = SystemClock.elapsedRealtime() + timeout * 1000L
        }
        return Bundle().apply {
            putString("run", run); putInt("maxTurns", limit); putInt("timeout", timeout)
            putInt("turnTimeoutSeconds", (arguments.getString("turnTimeoutSeconds") ?: "120").toInt())
        }
    }

    private fun authorize(instrumentation: Instrumentation, expired: Boolean = false) {
        check(owner === instrumentation && (expired || SystemClock.elapsedRealtime() < expires))
    }

    @JvmStatic fun setupCase(instrumentation: Instrumentation, arguments: Bundle): Bundle {
        authorize(instrumentation)
        check(!terminalFailure)
        check(arguments.keySet().all { it == "web_enabled" })
        check(arguments.containsKey("web_enabled"))
        sendPermit.clear()
        retire(instrumentation)
        check(AssistantRuntime.idleForSyntheticSession())
        val application = instrumentation.targetContext.applicationContext
        val directory = File(application.cacheDir, "synthetic-ask-${UUID.randomUUID()}")
        session = AssistantRuntime.typedBenchmarkSession(application, directory, "custom-luna-none",
            arguments.getBoolean("web_enabled")) { AssistantTiming.begin(application, it) }
        scoped = ScopedContext(application, directory)
        prompt = ""
        baseline = 0
        return Bundle().apply { putString("status", "configured"); putString("configuration", "isolated_policy_not_ui"); putBoolean("tts", false) }
    }

    internal fun validateDraft(text: String): String {
        require(text.isNotBlank() && AssistantTextInput.accepts(text))
        return text
    }

    /** Test fixture preparation only. Normal actor/store admission acknowledges the persisted draft. */
    @JvmStatic fun prepareDraft(instrumentation: Instrumentation, arguments: Bundle): Bundle {
        authorize(instrumentation)
        check(Looper.myLooper() != Looper.getMainLooper())
        check(!terminalFailure && turns < maxTurns)
        check(arguments.keySet() == setOf("prompt"))
        sendPermit.clear()
        val text = validateDraft(requireNotNull(arguments.getString("prompt")))
        val engine = requireNotNull(session).engine
        val before = engine.snapshot()
        check(before.contextReady && !before.busy && before.error == null)
        val ready = CountDownLatch(1)
        var acknowledged: AssistantState? = null
        engine.setDraft(text) { acknowledged = it; ready.countDown() }
        check(ready.await(5, TimeUnit.SECONDS)) { "typed_draft_preparation_timeout" }
        val settled = requireNotNull(acknowledged)
        check(settled.draft == text && settled.error == null && settled.contextReady && !settled.busy)
        return Bundle().apply { putString("status", "prepared"); putBoolean("draft_prepared", true) }
    }

    /** Admission only; the runner must subsequently click the real Send UI. */
    @JvmStatic fun arm(instrumentation: Instrumentation, arguments: Bundle): Bundle {
        authorize(instrumentation)
        check(!terminalFailure && turns < maxTurns)
        val text = requireNotNull(arguments.getString("prompt"))
        validateDraft(text)
        val state = requireNotNull(session).engine.snapshot()
        check(state.contextReady && !state.busy && state.error == null && state.draft == text)
        baseline = canonical(instrumentation).getInt("turn_count")
        val selected = requireNotNull(session).engine.snapshot()
        check(selected.draft == text && selected.contextReady && !selected.busy && selected.error == null)
        prompt = text
        turns++ // Consume budget before the UI action, including failed/uncertain submissions.
        sendPermit.arm(requireNotNull(session).engine, selected)
        return Bundle().apply { putString("request", "turn-$turns"); putInt("turn", turns) }
    }

    @JvmStatic fun status(instrumentation: Instrumentation): Bundle {
        authorize(instrumentation)
        val state = requireNotNull(session).engine.snapshot()
        var projected: AssistantTurn? = null
        var presentedProvisional = ""
        var presentedDraft = ""
        var primary: Pair<String, Boolean>? = null
        var projectionReady = false
        instrumentation.runOnMainSync {
            val page = activity?.get()?.typedBenchmarkProjection()
            projectionReady = page != null && !page.loading && page.error == null
            projected = page?.turns?.lastOrNull()
            presentedProvisional = activity?.get()?.typedBenchmarkProvisional().orEmpty()
            presentedDraft = activity?.get()?.typedBenchmarkDraft().orEmpty()
            primary = activity?.get()?.typedBenchmarkPrimary()
        }
        return Bundle().apply {
            putBoolean("busy", state.busy); putBoolean("context_ready", state.contextReady)
            putBoolean("failed", state.error != null)
            putString("tool_status", state.toolStatus)
            putString("provisional", presentedProvisional.take(MAX_TEXT))
            putString("presented_draft", presentedDraft.take(MAX_TEXT))
            putString("primary_action", primary?.first ?: "UNATTACHED")
            putBoolean("primary_enabled", primary?.second == true)
            putBoolean("engine_foreground", state.foreground)
            putInt("branch_count", state.branchChoices.size.coerceAtMost(128))
            putString("projected_prompt", projected?.prompt?.take(MAX_TEXT))
            putString("projected_answer", projected?.answer?.take(MAX_TEXT))
            putBoolean("projection_ready", projectionReady)
            putInt("turn", turns)
        }
    }

    private var lastValidatedTurnKey: String? = null

    internal fun ancestryValid(previousKey: String?, parentKey: String?): Boolean {
        if (previousKey == null) {
            return parentKey == null
        }
        return parentKey == previousKey
    }

    /** Canonical turns always carry an explicit `parent` (object or null); anything else is malformed. */
    internal fun extractParentKey(body: JSONObject?): String? {
        if (body == null) return null
        check(body.has("parent"))
        return if (body.isNull("parent")) null else assistantIdKey(body.getJSONObject("parent").toString())
    }

    @JvmStatic fun validate(instrumentation: Instrumentation): Bundle {
        authorize(instrumentation)
        val result = canonical(instrumentation)
        val projection = status(instrumentation)
        result.putAll(projection)
        val correct = result.getInt("turn_count") == baseline + 1 && result.getString("canonical_prompt") == prompt
        var sameTurn = false
        var turnKey: String? = null
        var parentKey: String? = null
        instrumentation.runOnMainSync {
            sameTurn = activity?.get()?.typedBenchmarkProjection()?.turns?.lastOrNull()?.id?.let(::assistantIdKey) == result.getString("turn_key")
        }
        turnKey = result.getString("turn_key")
        parentKey = result.getString("turn_parent_key")
        result.remove("turn_key")
        result.remove("turn_parent_key")

        val ancestry = ancestryValid(lastValidatedTurnKey, parentKey)
        val equal = correct && sameTurn && ancestry && result.getBoolean("projection_ready") && result.getString("canonical_answer") == result.getString("projected_answer") &&
            result.getString("canonical_prompt") == result.getString("projected_prompt")
        val valid = correct && equal && !result.getBoolean("busy") && !result.getBoolean("failed") && result.getBoolean("context_ready")
        result.putBoolean("ancestry_valid", ancestry)
        result.putBoolean("projection_equal", equal)
        result.putBoolean("execution_valid", valid)
        result.putString("sources_status", "unsupported")
        result.putString("page_read_status", "unsupported")
        result.putString("request", "turn-$turns")
        if (valid) {
            lastValidatedTurnKey = turnKey
        } else {
            terminalFailure = true
        }
        return result
    }

    private fun canonical(instrumentation: Instrumentation): Bundle {
        check(Looper.myLooper() != Looper.getMainLooper())
        val ready = CountDownLatch(1)
        var raw: String? = null
        requireNotNull(session).engine.history { page, _ -> raw = page; ready.countDown() }
        check(ready.await(5, TimeUnit.SECONDS))
        val page = raw?.let(::JSONObject)
        val records = page?.getJSONArray("turns")
        val count = records?.length() ?: 0
        val operation = if (count > 0) records!!.getJSONObject(count - 1).getJSONObject("operation") else null
        val body = operation?.getJSONObject("body")
        // Ancestry is only meaningful if the last record is the selected head of a head-anchored page.
        if (page != null) check(page.isNull("head") == (count == 0))
        if (operation != null) {
            check(body!!.getString("kind") == "turn_completed")
            check(assistantIdKey(page!!.getJSONObject("head").toString()) == assistantIdKey(operation.getJSONObject("id").toString()))
        }
        val answer = body?.getJSONObject("answer")
        val text = answer?.let { it.getString(if (it.getString("kind") == "refusal") "reason" else "text") }
        check(text == null || text.length <= MAX_TEXT)
        return Bundle().apply {
            putInt("turn_count", count)
            putString("turn_key", operation?.getJSONObject("id")?.toString()?.let(::assistantIdKey))
            putString("turn_parent_key", extractParentKey(body))
            putString("canonical_prompt", body?.getString("prompt"))
            putString("canonical_answer", text)
        }
    }

    @JvmStatic fun close(instrumentation: Instrumentation): Bundle {
        authorize(instrumentation, expired = true)
        sendPermit.clear()
        retire(instrumentation)
        owner = null
        return Bundle().apply { putString("status", "cleaned"); putInt("turns", turns) }
    }

    private fun retire(instrumentation: Instrumentation) {
        check(Looper.myLooper() != Looper.getMainLooper())
        val old = activity?.get()
        runSyntheticCleanup(listOf(
            {
                instrumentation.runOnMainSync { old?.finish() }
                val deadline = SystemClock.elapsedRealtime() + 5_000
                var destroyed = old == null
                while (!destroyed && SystemClock.elapsedRealtime() < deadline) {
                    instrumentation.runOnMainSync { destroyed = old?.isDestroyed != false }
                    if (!destroyed) Thread.sleep(20)
                }
                check(destroyed) { "typed_activity_retirement_timeout" }
            },
            { session?.close() },
            { scoped?.cleanup() },
        ))
        session = null; activity = null; scoped = null
        lastValidatedTurnKey = null
    }


    internal fun client(target: AssistantActivity): AssistantClient? {
        if (owner == null) return null
        check(SystemClock.elapsedRealtime() < expires)
        check(activity?.get() == null)
        activity = WeakReference(target)
        return requireNotNull(session).engine
    }
    internal fun active(client: AssistantClient): Boolean = session?.engine === client
    internal fun permitUiSend(client: AssistantClient): Boolean {
        if (!active(client)) return true
        return sendPermit.consume(client, client.snapshot(), owner != null && SystemClock.elapsedRealtime() < expires)
    }
    internal fun context(client: AssistantClient): Context? = if (active(client)) scoped else null
    internal fun voice(client: AssistantClient): AssistantVoiceController? = if (!active(client)) null else object : AssistantVoiceController {
        override fun snapshot() = AssistantVoiceState()
        override fun subscribe(observer: (AssistantVoiceState) -> Unit): AutoCloseable { observer(snapshot()); return AutoCloseable {} }
        override fun foreground(visible: Boolean) {}
        override fun dictate(draft: String) { error("typed_only") }
        override fun conversation(draft: String) { error("typed_only") }
        override fun endUtterance() {}
        override fun stop() {}
        override fun tts(enabled: Boolean) { check(!enabled) }
        override fun speak(text: String) { error("typed_only") }
        override fun close() {}
    }
    private const val MAX_TEXT = 131072

    private class ScopedContext(base: Context, private val directory: File) : ContextWrapper(base) {
        private val names = mutableSetOf<String>()
        override fun getApplicationContext(): Context = this
        override fun getNoBackupFilesDir(): File = directory
        override fun getCacheDir(): File = directory
        @Synchronized override fun getSharedPreferences(name: String, mode: Int): SharedPreferences {
            require(name.matches(Regex("[a-zA-Z0-9_.-]{1,100}")))
            val isolated = "${directory.name}-$name"
            names += isolated
            return super.getSharedPreferences(isolated, Context.MODE_PRIVATE)
        }
        @Synchronized fun cleanup() {
            names.forEach { check(baseContext.deleteSharedPreferences(it)) }
            names.clear()
        }
    }
}

/** Benchmark-only one-shot admission; domain Send and persistence remain unchanged. */
internal class TypedBenchmarkSendPermit {
    private var client: AssistantClient? = null
    private var selection: AssistantState? = null
    @Synchronized fun clear() { client = null; selection = null }
    @Synchronized fun arm(owner: AssistantClient, state: AssistantState) { client = owner; selection = state }
    @Synchronized fun consume(owner: AssistantClient, current: AssistantState, authorized: Boolean): Boolean {
        val expected = selection
        val matches = authorized && client === owner && expected != null &&
            current.draft == expected.draft && current.conversation == expected.conversation &&
            current.selectedHead == expected.selectedHead && current.model == expected.model &&
            current.webEnabled == expected.webEnabled && current.contextReady && current.foreground &&
            !current.busy && current.error == null
        clear()
        return matches
    }
}
