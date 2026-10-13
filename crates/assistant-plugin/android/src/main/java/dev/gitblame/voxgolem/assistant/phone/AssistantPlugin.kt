package dev.gitblame.voxgolem.assistant.phone

import android.app.Activity
import android.app.KeyguardManager
import android.content.Context
import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import android.webkit.WebView
import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleOwner
import app.tauri.annotation.Command
import app.tauri.annotation.Permission
import app.tauri.annotation.PermissionCallback
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Channel
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import org.json.JSONObject
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong
import java.lang.ref.WeakReference
import dev.gitblame.voxgolem.assistant.runtime.AssistantRuntime
import dev.gitblame.voxgolem.assistant.AssistantTiming.Stage

@InvokeArg
class SubmitArgs {
    lateinit var requestId: String
    lateinit var prompt: String
    lateinit var updates: Channel
    var searchMode: String? = null
}

@InvokeArg
class VoiceArgs {
    lateinit var sessionId: String
    var loopMode: Boolean = false
    var draft: String = ""
    var completionPolicy: String = "manual"
    var launchToken: String? = null
    lateinit var updates: Channel
}

@TauriPlugin(permissions = [Permission(alias = "assistantMic", strings = [Manifest.permission.RECORD_AUDIO])])
class AssistantPlugin(private val activity: Activity) : Plugin(activity) {
    private val lock = Any()
    private var request: String? = null
    private val backend by lazy { AssistantPhoneBindings.get(activity) }
    private val speech by lazy { PhoneSpeechSession(activity.applicationContext, backend) }
    private val main = Handler(Looper.getMainLooper())
    private val visible = AtomicBoolean(false)
    private val inputEpoch = AtomicLong()
    private var webViewReference: WeakReference<WebView>? = null
    private data class PendingVoice(val epoch: Long, val args: VoiceArgs, val invoke: Invoke, val owner: SpeechAdmissionSnapshot)
    private var pendingVoice: PendingVoice? = null
    private var promptWakeLock: PowerManager.WakeLock? = null

    private fun isScreenOffOrLocked(): Boolean {
        val pm = activity.getSystemService(Context.POWER_SERVICE) as? PowerManager
        val km = activity.getSystemService(Context.KEYGUARD_SERVICE) as? KeyguardManager
        val screenOff = pm?.isInteractive == false
        val locked = km?.isKeyguardLocked == true
        return screenOff || locked
    }

    private fun acquirePromptWakeLock() {
        try {
            if (promptWakeLock == null) {
                val pm = activity.getSystemService(Context.POWER_SERVICE) as? PowerManager
                promptWakeLock = pm?.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "VoxGolem:PromptExecution")
            }
            promptWakeLock?.takeIf { !it.isHeld }?.acquire(60_000)
        } catch (_: RuntimeException) {}
    }

    private fun releasePromptWakeLock() {
        try {
            promptWakeLock?.takeIf { it.isHeld }?.release()
        } catch (_: RuntimeException) {}
    }

    override fun load(webView: WebView) {
        val startupTiming = (activity as? PhoneStartupTimingOwner)?.startupTiming?.trace
        startupTiming?.mark(Stage.PLUGIN_LOAD_STARTED)
        AssistantRuntime.configureCaptureResourceAdmission(true)
        AssistantRuntime.get(activity, startupTiming)
        webViewReference = WeakReference(webView)
        (activity as? LifecycleOwner)?.lifecycle?.addObserver(object : DefaultLifecycleObserver {
            override fun onResume(owner: LifecycleOwner) = setForeground(true)
            override fun onPause(owner: LifecycleOwner) {
                if (!isScreenOffOrLocked()) setForeground(false)
            }
            override fun onStop(owner: LifecycleOwner) {
                if (!isScreenOffOrLocked()) setForeground(false)
            }
            override fun onDestroy(owner: LifecycleOwner) {
                setForeground(false)
                speech.close()
                releasePromptWakeLock()
            }
        })
        setForeground((activity as? LifecycleOwner)?.lifecycle?.currentState?.isAtLeast(Lifecycle.State.RESUMED) == true)
        startupTiming?.mark(Stage.PLUGIN_LOAD_COMPLETED)
    }

    override fun onNewIntent(intent: Intent) {
        val token = intent.getStringExtra(NewVoiceShortcutLauncher.TOKEN_EXTRA)
        if (intent.action == NewVoiceShortcutLauncher.MAIN_ACTION && intent.data == null &&
            intent.flags and Intent.FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY == 0 &&
            (token != null && ShortcutActionGate.valid(token) || NewVoiceShortcutLauncher.hasConfirmation())) notifyShortcut(token)
    }
    private fun notifyShortcut(token: String? = null) { main.post {
        val webView = webViewReference?.get() ?: return@post
        if (visible.get() && (NewVoiceShortcutLauncher.actions.notifyPending(token) || NewVoiceShortcutLauncher.notifyConfirmation())) {
            webView.evaluateJavascript("window.dispatchEvent(new Event('${NewVoiceShortcutLauncher.NOTIFICATION_EVENT}'))", null)
        }
    } }

    private fun setForeground(visible: Boolean) {
        val wasVisible = this.visible.get()
        this.visible.set(visible)
        if (!visible) { inputEpoch.incrementAndGet(); pendingVoice = null }
        if (!visible && wasVisible) NewVoiceShortcutLauncher.cancelEntries()
        try { backend.foreground(visible) } catch (_: AssistantPhoneFailure) { /* Commands report the unavailable binding. */ }
        speech.foreground(visible)
        if (visible && (NewVoiceShortcutLauncher.actions.hasPending() || NewVoiceShortcutLauncher.hasConfirmation())) notifyShortcut()
    }

    private fun execute(invoke: Invoke, name: String = invoke.command) {
        try { backend.command(name, JSONObject(invoke.getRawArgs())) { result -> reply(invoke, result) } }
        catch (error: RuntimeException) { reply(invoke, Result.failure(error)) }
    }

    @Command fun get_app_snapshot(invoke: Invoke) {
        val timing = (activity as? PhoneStartupTimingOwner)?.startupTiming?.trace
        timing?.mark(Stage.INITIAL_SNAPSHOT_REQUESTED)
        try { backend.command(invoke.command, JSONObject(invoke.getRawArgs())) { result ->
            if (result.isSuccess) timing?.mark(Stage.INITIAL_SNAPSHOT_REPLIED)
            reply(invoke, result)
        } } catch (error: RuntimeException) { reply(invoke, Result.failure(error)) }
    }
    @Command fun report_startup_stage(invoke: Invoke) {
        val stage = invoke.getArgs().optString("stage")
        if (stage !in setOf("frontend_ready", "first_response_dom")) {
            reject(invoke, "invalid_input", "Unknown startup timing stage"); return
        }
        (activity as? PhoneStartupTimingOwner)?.startupTiming?.trace?.reportFrontendStage(stage)
        invoke.resolve()
    }
    @Command fun new_conversation(invoke: Invoke) {
        if (pendingVoice != null || speech.active() && speech.status().optString("mode") != "shortcut") reject(invoke, "busy", "Stop voice before changing conversations") else execute(invoke)
    }
    @Command fun set_preferences(invoke: Invoke) = execute(invoke)
    @Command fun set_search_mode(invoke: Invoke) = execute(invoke)
    @Command fun select_conversation(invoke: Invoke) {
        if (speech.active() || pendingVoice != null) reject(invoke, "busy", "Stop voice before changing conversations") else execute(invoke)
    }
    @Command fun select_conversation_head(invoke: Invoke) {
        if (speech.active() || pendingVoice != null) reject(invoke, "busy", "Stop voice before changing continuation") else execute(invoke)
    }
    @Command fun save_assistant_draft(invoke: Invoke) = execute(invoke)
    @Command fun list_assistant_conversations(invoke: Invoke) = execute(invoke)
    @Command fun list_assistant_heads(invoke: Invoke) = execute(invoke)
    @Command fun assistant_pairing_status(invoke: Invoke) = execute(invoke)

    @Command fun assistant_voice_status(invoke: Invoke) { invoke.resolve(JSObject(speech.status().toString())) }
    @Command fun open_speech_services_store(invoke: Invoke) {
        val opened = dev.gitblame.voxgolem.assistant.speech.SpeechServiceRequirement.openPlayStore(activity)
        if (opened) invoke.resolve()
        else reject(invoke, "unavailable", "Unable to open Play Store or browser")
    }
    @Command fun new_voice_shortcut_status(invoke: Invoke) { invoke.resolve(JSObject(NewVoiceShortcutLauncher.status(activity).toString())) }
    @Command fun pin_new_voice_shortcut(invoke: Invoke) {
        if (!visible.get()) { reject(invoke, "unavailable", "Pinning a shortcut requires the visible app"); return }
        try { invoke.resolve(JSObject(NewVoiceShortcutLauncher.pin(activity).toString())) } catch (error: RuntimeException) { reply(invoke, Result.failure(error)) }
    }
    @Command fun consume_new_voice_shortcut(invoke: Invoke) {
        if (!visible.get()) { invoke.resolve(JSObject().put("token", JSONObject.NULL).put("error", JSONObject.NULL)); return }
        if (NewVoiceShortcutLauncher.takeConfirmation()) {
            invoke.resolve(JSObject().put("token", JSONObject.NULL).put("error", JSONObject.NULL).put("confirmationRequired", true)); return
        }
        val token = NewVoiceShortcutLauncher.actions.consume()
        val busy = token != null && (backend.busy() || speech.active() || pendingVoice != null)
        if (busy) NewVoiceShortcutLauncher.actions.release(token!!)
        invoke.resolve(JSObject().put("token", if (busy) JSONObject.NULL else token ?: JSONObject.NULL).put("error", if (busy) "Voice chat is busy; existing work was not cancelled" else JSONObject.NULL))
    }
    @Command fun release_new_voice_shortcut(invoke: Invoke) {
        val token = invoke.getArgs().optString("token")
        if (ShortcutActionGate.valid(token)) NewVoiceShortcutLauncher.actions.release(token)
        invoke.resolve()
    }
    @Command fun prepare_new_voice_shortcut(invoke: Invoke) {
        try { speech.shortcutPrepared(invoke.getArgs().getString("sessionId")); invoke.resolve() } catch (error: RuntimeException) { reply(invoke, Result.failure(error)) }
    }
    @Command fun stop_send_new_voice_shortcut(invoke: Invoke) {
        try { speech.stopAndSendShortcut(); invoke.resolve() } catch (error: RuntimeException) { reply(invoke, Result.failure(error)) }
    }
    @Command fun start_assistant_voice(invoke: Invoke) {
        try {
            val args = invoke.parseArgs(VoiceArgs::class.java)
            require(args.sessionId.isNotEmpty() && args.sessionId.toByteArray(Charsets.UTF_8).size <= 128 && args.draft.toByteArray(Charsets.UTF_8).size <= 32 * 1024)
            require(args.completionPolicy in setOf("manual", "stop_send_new", "single_turn", "repeat_after_reply"))
            if (args.completionPolicy == "stop_send_new" && (args.loopMode || args.launchToken == null || !NewVoiceShortcutLauncher.actions.active(args.launchToken!!))) {
                reject(invoke, "invalid_input", "A deliberate voice shortcut launch is required"); return
            }
            if (args.launchToken != null && !NewVoiceShortcutLauncher.actions.active(args.launchToken!!)) {
                reject(invoke, "invalid_input", "A deliberate voice shortcut launch is required"); return
            }
            if (!visible.get()) { reject(invoke, "unavailable", "Voice input requires the visible app"); return }
            if (pendingVoice != null) { reject(invoke, "busy", "Microphone permission admission is already pending"); return }
            if (backend.busy() || speech.active()) { reject(invoke, "busy", "Voice chat is busy; existing work was not cancelled"); return }
            if (!speech.status().getBoolean("available")) { reject(invoke, "unavailable", "Native dictation is unavailable under the configured speech policy"); return }
            val epoch = inputEpoch.incrementAndGet()
            pendingVoice = PendingVoice(epoch, args, invoke, backend.speechAdmissionSnapshot())
            if (activity.checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
                requestPermissionForAlias("assistantMic", invoke, "assistantMicGranted")
            } else startVoice(invoke)
        } catch (error: RuntimeException) { reply(invoke, Result.failure(error)) }
    }
    @PermissionCallback private fun assistantMicGranted(invoke: Invoke) = startVoice(invoke)
    private fun startVoice(invoke: Invoke) {
        val pending = pendingVoice
        if (pending != null && pending.invoke !== invoke) {
            reject(invoke, "unavailable", "Microphone permission admission was cancelled"); return
        }
        pendingVoice = null
        if (pending == null || pending.epoch != inputEpoch.get() || !visible.get() || activity.checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED ||
            backend.speechAdmissionSnapshot() != pending.owner ||
            (pending.args.launchToken != null && pending.args.launchToken?.let(NewVoiceShortcutLauncher.actions::active) != true)) {
            pending?.args?.launchToken?.let(NewVoiceShortcutLauncher.actions::release)
            reject(invoke, "unavailable", "Microphone permission or foreground admission was cancelled"); return
        }
        try {
            speech.start(pending.args.sessionId, pending.args.loopMode, pending.args.draft,
                { event -> pending.args.updates.send(JSObject(event.toString())) }, pending.args.launchToken, pending.args.completionPolicy)
            invoke.resolve(JSObject(speech.status().toString()))
        } catch (error: RuntimeException) { pending.args.launchToken?.let(NewVoiceShortcutLauncher.actions::release); reply(invoke, Result.failure(error)) }
    }
    @Command fun finish_assistant_dictation(invoke: Invoke) { speech.finish(); invoke.resolve() }
    @Command fun stop_assistant_voice(invoke: Invoke) { inputEpoch.incrementAndGet(); pendingVoice = null; NewVoiceShortcutLauncher.cancelEntries(); speech.stop(); invoke.resolve() }

    @Command fun cancel_prompt(invoke: Invoke) {
        val id = invoke.getArgs().optString("requestId")
        synchronized(lock) { if (request == id) backend.cancelDelivery(id) }
        execute(invoke)
    }

    @Command fun ack_delivery(invoke: Invoke) {
        val args = invoke.getArgs()
        val accepted = synchronized(lock) {
            val id = args.optString("requestId")
            if (request == id) backend.acknowledge(id, args.optString("providerRequestId"), args.optString("revision"), args.optBoolean("accepted", false)) else false
        }
        invoke.resolve(JSObject().put("accepted", accepted))
    }

    @Command fun submit_prompt(invoke: Invoke) {
        val args = try { invoke.parseArgs(SubmitArgs::class.java) }
        catch (_: RuntimeException) { reject(invoke, "invalid_input", "Invalid prompt request"); return }
        if (args.searchMode != null && args.searchMode !in dev.gitblame.voxgolem.assistant.runtime.AssistantSearchModes.values) {
            reject(invoke, "invalid_input", "Invalid search mode"); return
        }
        if (args.requestId.isEmpty() || args.requestId.toByteArray(Charsets.UTF_8).size > 128 || args.prompt.isBlank() || args.prompt.toByteArray(Charsets.UTF_8).size > 32 * 1024) {
            reject(invoke, "invalid_input", "Prompt or request ID is empty or too long"); return
        }
        val isAlive = !activity.isFinishing && !activity.isDestroyed
        val isForeground = (activity as? LifecycleOwner)?.lifecycle?.currentState?.isAtLeast(Lifecycle.State.RESUMED) == true
        if (!isAlive || (!isForeground && !isScreenOffOrLocked())) {
            reject(invoke, "unavailable", "The app must be in the foreground to send a request"); return
        }
        val nativeVoice = args.requestId.startsWith("voice-") || args.requestId.startsWith("shortcut-")
        val speechOwner = if (nativeVoice) speech.submissionOwner(args.requestId) else null
        if (nativeVoice && speechOwner == null) {
            reject(invoke, "unavailable", "Voice submission was cancelled or its conversation changed"); return
        }
        synchronized(lock) {
            if (request != null) { reject(invoke, "busy", "A request is already active"); return }
            request = args.requestId
        }
        val ended = AtomicBoolean(false)
        val replyTicket = speech.onRequestSubmitted(args.requestId)
        acquirePromptWakeLock()
        val terminal: (Result<Any?>) -> Unit = { result ->
            try {
                if (ended.compareAndSet(false, true)) {
                    synchronized(lock) { if (request == args.requestId) request = null }
                    speech.onRequestResult(args.requestId, result, replyTicket)
                    reply(invoke, result)
                }
            } finally {
                releasePromptWakeLock()
            }
        }
        try {
            val update: (JSONObject) -> Unit = { update ->
                if (!ended.get()) {
                    args.updates.send(JSObject(update.toString()))
                }
            }
            if (speechOwner == null) backend.submit(args.requestId, args.prompt, update, terminal, args.searchMode)
            else backend.submitOwnedSpeech(args.requestId, args.prompt, update, terminal, args.searchMode, speechOwner)
        } catch (error: RuntimeException) { terminal(Result.failure(error)) }
    }

    private fun reply(invoke: Invoke, result: Result<Any?>) {
        result.fold({ value ->
            when (value) {
                null -> invoke.resolve()
                is JSONObject -> invoke.resolve(JSObject(value.toString()))
                else -> invoke.resolveObject(value)
            }
        }, { error ->
            val failure = error as? AssistantPhoneFailure
            reject(invoke, failure?.code ?: "internal", failure?.boundedMessage ?: "Native command failed")
        })
    }

    private fun reject(invoke: Invoke, code: String, message: String) {
        val safe = code in setOf("invalid_input", "not_found", "busy", "unavailable", "internal") && message.isNotEmpty() && message.toByteArray(Charsets.UTF_8).size <= 512
        invoke.reject(if (safe) message else "Native command failed", if (safe) code else "internal")
    }
}
