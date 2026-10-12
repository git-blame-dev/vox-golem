package dev.gitblame.voxgolem.android.wear.assistant

import android.app.Activity
import android.content.Intent
import android.content.ComponentName
import android.Manifest
import android.content.pm.ActivityInfo
import android.content.pm.ApplicationInfo
import android.content.pm.ResolveInfo
import android.os.Bundle
import android.os.Looper
import dev.gitblame.voxgolem.assistant.replication.PeerPairingReason
import dev.gitblame.voxgolem.assistant.replication.PeerPairingState
import dev.gitblame.voxgolem.assistant.runtime.*
import dev.gitblame.voxgolem.assistant.speech.SpeechSelection
import dev.gitblame.voxgolem.assistant.AssistantTiming
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.Shadows
import org.robolectric.RuntimeEnvironment
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowLog
import java.time.Duration

class SyntheticAssistantClient : AssistantClient {
    var state = AssistantState(foreground = true, draft = "Synthetic old draft")
    var resumes = 0; var backgrounds = 0; var sends = 0; var newConversations = 0
    var model = state.model
    val historyReads = mutableListOf<Pair<String?, (String?, AssistantState) -> Unit>>()
    val resumeHistoryOrder = mutableListOf<String>()
    private val observers = mutableListOf<(AssistantState) -> Unit>()
    override fun snapshot() = state
    override fun subscribe(observer: (AssistantState) -> Unit): AutoCloseable {
        observers += observer; observer(state); return AutoCloseable { observers -= observer }
    }
    fun publish(value: AssistantState) { state = value; observers.toList().forEach { it(value) } }
    override fun history(before: String?, onSettled: (String?, AssistantState) -> Unit) { resumeHistoryOrder += "history"; historyReads += before to onSettled }
    override fun resume() { resumeHistoryOrder += "resume"; resumes++ }
    override fun background() { backgrounds++ }
    override fun refresh() {}
    override fun stop() {}
    override fun setDraft(text: String) { state = state.copy(draft = text) }
    override fun selectModel(model: String) { this.model = model }
    override fun setWebEnabled(enabled: Boolean) { state = state.copy(webEnabled = enabled) }
    override fun setSpeechNetworkFallback(enabled: Boolean) { state = state.copy(speechNetworkFallback = enabled) }
    override fun newConversation() { newConversations++ }
    override fun selectConversation(id: String) {}
    override fun selectHead(id: String) {}
    override fun send() { sends++ }
}

open class SyntheticAssistantActivity : AssistantActivity() {
    companion object {
        var preferenceName = "synthetic-assistant-activity-defaults"
        var retainedClient: SyntheticAssistantClient? = null
    }
    val synthetic = retainedClient ?: SyntheticAssistantClient()
    val audio = SyntheticAssistantVoice()
    val linkReads = mutableListOf<(PeerPairingState) -> Unit>()
    override fun assistantClient(): AssistantClient = synthetic
    override fun assistantVoice(client: AssistantClient): AssistantVoiceController = audio
    override fun readLinkStatus(result: (PeerPairingState) -> Unit) { linkReads.add(result) }
    override fun assistantVoiceSettings() = WearVoiceSettings(getSharedPreferences(preferenceName, MODE_PRIVATE))
}

class FailedInitialVoicePreferencesActivity : SyntheticAssistantActivity() {
    override fun assistantVoiceSettings(): WearVoiceSettings {
        val actual = getSharedPreferences("synthetic-failed-initial-voice", MODE_PRIVATE)
        check(actual.edit().putBoolean("read-replies-aloud", false).commit())
        val failed = object : android.content.SharedPreferences by actual {
            override fun edit(): android.content.SharedPreferences.Editor {
                val editor = actual.edit()
                return object : android.content.SharedPreferences.Editor by editor {
                    override fun putBoolean(key: String?, value: Boolean): android.content.SharedPreferences.Editor = this
                    override fun commit() = false
                }
            }
        }
        return WearVoiceSettings(failed)
    }
}

class SyntheticAssistantVoice : AssistantVoiceController {
    var state = AssistantVoiceState(admissionReady = true)
    private val observers = mutableListOf<(AssistantVoiceState) -> Unit>()
    var dictations = 0; var conversations = 0; var stops = 0
    val dictatedDrafts = mutableListOf<String>()
    val conversationDrafts = mutableListOf<String>()
    var onConversation: () -> Unit = {}
    val foregrounds = mutableListOf<Boolean>()
    var closes = 0
    var handlesManualSend = false
    var manualSends = 0
    val speechChoices = mutableListOf<Boolean>()
    var onStop: () -> Unit = {}
    var acceptsSpeechSelection = true
    val submittedSpeechSelections = mutableListOf<SpeechSelection>()
    val speechSettlements = mutableListOf<(Boolean) -> Unit>()
    var speechSubmissionFailure: RuntimeException? = null
    var synchronousSpeechSettlement: Boolean? = null
    var legacySpeechConfigurations = 0
    var speechOptionsPreparations = 0
    override fun snapshot() = state
    fun publish(value: AssistantVoiceState) { state = value; observers.toList().forEach { it(value) } }
    override fun subscribe(observer: (AssistantVoiceState) -> Unit): AutoCloseable {
        observers += observer; observer(snapshot()); return AutoCloseable { observers -= observer }
    }
    override fun foreground(visible: Boolean) { foregrounds += visible }
    override fun dictate(draft: String) { dictations++; dictatedDrafts += draft }
    // Keep legacy fixture counters compatible while recording the new single-turn contract.
    val askedDrafts = mutableListOf<String>()
    override fun ask(draft: String) { askedDrafts += draft; dictate(draft) }
    override fun conversation(draft: String) { conversations++; conversationDrafts += draft; onConversation() }
    override fun endUtterance() {}
    override fun stop() { stops++; onStop() }
    override fun tts(enabled: Boolean) { speechChoices += enabled }
    override fun prepareSpeechOptions() { speechOptionsPreparations++ }
    override fun speak(text: String) {}
    override fun send(): Boolean { manualSends++; return handlesManualSend }
    override fun configureSpeech(selection: SpeechSelection): Boolean {
        legacySpeechConfigurations++
        submittedSpeechSelections += selection
        return acceptsSpeechSelection
    }
    override fun configureSpeech(selection: SpeechSelection, onSettled: (Boolean) -> Unit): Boolean {
        speechSubmissionFailure?.let { throw it }
        submittedSpeechSelections += selection
        speechSettlements += onSettled
        synchronousSpeechSettlement?.let(onSettled)
        return acceptsSpeechSelection
    }
    override fun close() { closes++ }
}

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class AssistantActivityTest {
    @Test fun resumeQueuesForegroundBeforeReadingSelectedConversationHistory() {
        val previous = SyntheticAssistantActivity.retainedClient
        val client = SyntheticAssistantClient().apply {
            state = state.copy(conversation = "{\"replica\":\"synthetic-resume-order\",\"sequence\":\"1\"}")
        }
        SyntheticAssistantActivity.retainedClient = client
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).create().start()
        try {
            controller.resume()
            Shadows.shadowOf(Looper.getMainLooper()).idle()
            assertEquals(listOf("resume", "history"), client.resumeHistoryOrder)
        } finally {
            controller.pause().stop().destroy()
            SyntheticAssistantActivity.retainedClient = previous
        }
    }

    @Test fun ordinaryResumeDoesNotPrepareSpeechOptionsButExplicitVisibleSettingsRequestDoes() {
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val activity = controller.get()
        try {
            Shadows.shadowOf(Looper.getMainLooper()).idle()
            assertEquals(0, activity.audio.speechOptionsPreparations)
            activity.prepareSpeechOptions()
            assertEquals(1, activity.audio.speechOptionsPreparations)
            controller.pause()
            activity.prepareSpeechOptions()
            assertEquals(1, activity.audio.speechOptionsPreparations)
            controller.stop().start().resume()
            Shadows.shadowOf(Looper.getMainLooper()).idle()
            assertEquals(1, activity.audio.speechOptionsPreparations)
            activity.prepareSpeechOptions()
            assertEquals(2, activity.audio.speechOptionsPreparations)
        } finally { controller.pause().stop().destroy() }
    }

    private fun conversationLaunchIntent() = Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER)
        .setComponent(ComponentName(RuntimeEnvironment.getApplication(), AssistantActivity.CONVERSATION_ENTRY))
    private fun voiceLaunchIntent() = Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER)
        .setComponent(ComponentName(RuntimeEnvironment.getApplication(), AssistantActivity.VOICE_ENTRY))

    @Test fun matchingAskAndTalkLauncherEntriesAutomaticallyStartWithoutCallerVerification() {
        Shadows.shadowOf(RuntimeEnvironment.getApplication()).grantPermissions(Manifest.permission.RECORD_AUDIO)
        for ((intent, mode) in listOf(voiceLaunchIntent() to AssistantVoiceEntryMode.ASK, conversationLaunchIntent() to AssistantVoiceEntryMode.TALK)) {
            val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java, intent).setup()
            val activity = controller.get()
            try {
                Shadows.shadowOf(Looper.getMainLooper()).idle()
                assertEquals(mode, activity.presentedVoiceState().entryMode)
                assertFalse(activity.presentedVoiceState().launchConfirmationRequired)
                assertFalse(activity.voiceAdmissionPending)
                assertEquals(1, activity.audio.dictations + activity.audio.conversations)
                assertEquals(0, activity.synthetic.sends)
                controller.newIntent(intent); Shadows.shadowOf(Looper.getMainLooper()).idle()
                assertEquals(2, activity.audio.dictations + activity.audio.conversations)
                assertFalse(activity.presentedVoiceState().launchConfirmationRequired)
            } finally { controller.pause().stop().destroy() }
        }
    }

    @Test fun stopRetiresDelayedHydrationLaunchAndPermissionAdmissionWithoutForegroundReplay() {
        Shadows.shadowOf(RuntimeEnvironment.getApplication()).grantPermissions(Manifest.permission.RECORD_AUDIO)
        SyntheticAssistantActivity.retainedClient = SyntheticAssistantClient().apply { state = state.copy(foreground = false) }
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java, voiceLaunchIntent()).setup()
        val activity = controller.get()
        try {
            Shadows.shadowOf(Looper.getMainLooper()).idle()
            assertTrue(activity.voiceAdmissionPending)
            assertTrue(activity.presentedVoiceState().active)
            activity.newConversation(); activity.selectHead("synthetic-blocked-head")
            assertEquals(0, activity.synthetic.newConversations)
            activity.stopAll()
            assertFalse(activity.voiceAdmissionPending)
            activity.synthetic.publish(activity.synthetic.state.copy(foreground = true))
            Shadows.shadowOf(Looper.getMainLooper()).idle()
            assertEquals(0, activity.audio.dictations + activity.audio.conversations)
            controller.pause().stop().start().resume(); Shadows.shadowOf(Looper.getMainLooper()).idle()
            assertEquals(0, activity.audio.dictations + activity.audio.conversations)
        } finally { controller.pause().stop().destroy(); SyntheticAssistantActivity.retainedClient = null }

        Shadows.shadowOf(RuntimeEnvironment.getApplication()).denyPermissions(Manifest.permission.RECORD_AUDIO)
        val permission = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        try {
            permission.get().requestVoice(false)
            assertTrue(permission.get().presentedVoiceState().admissionPending)
            permission.get().stopAll(); permission.get().receiveMicrophonePermission(true)
            assertFalse(permission.get().voiceAdmissionPending)
            assertTrue(permission.get().audio.askedDrafts.isEmpty())
        } finally { permission.pause().stop().destroy() }
    }

    @Test fun selectedHeadChangeDuringPermissionAdmissionRequiresFreshConsent() {
        Shadows.shadowOf(RuntimeEnvironment.getApplication()).denyPermissions(Manifest.permission.RECORD_AUDIO)
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        try {
            val activity = controller.get()
            activity.requestVoice(false)
            activity.synthetic.state = activity.synthetic.state.copy(selectedHead = "synthetic-other-head")
            activity.receiveMicrophonePermission(true)
            assertTrue(activity.audio.askedDrafts.isEmpty())
            assertFalse(activity.voiceAdmissionPending)
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun startupDiagnosticsCoverHotIntentAndResumeWithoutReplayingVoice() {
        val context = RuntimeEnvironment.getApplication()
        context.applicationInfo.flags = context.applicationInfo.flags or ApplicationInfo.FLAG_DEBUGGABLE
        Shadows.shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
        ShadowLog.clear()
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java, voiceLaunchIntent()).setup()
        val looper = Shadows.shadowOf(Looper.getMainLooper())
        try {
            looper.idle()
            controller.newIntent(voiceLaunchIntent()); looper.idle()
            controller.pause().stop().start().resume(); looper.idle()
            val messages = ShadowLog.getLogsForTag(AssistantTiming.TAG).map { it.msg }
                .filter { it.startsWith("operation=ACTIVITY ") }
            assertEquals(3, messages.map { Regex("run=([0-9]+)").find(it)!!.groupValues[1] }.distinct().size)
            assertEquals(1, messages.count { it.contains("stage=READY_REUSED ") })
            assertEquals(2, messages.count { it.contains("stage=FIRST_PROJECTION ") })
            assertEquals(2, controller.get().audio.dictations)
            assertEquals(0, controller.get().synthetic.sends)
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun bothAliasesResolveToTheSameSingleTopActivityWithDistinctLabels() {
        val pm = RuntimeEnvironment.getApplication().packageManager
        for ((name, label) in listOf(AssistantActivity.VOICE_ENTRY to "Vox Ask",
            AssistantActivity.CONVERSATION_ENTRY to "Vox Talk")) {
            val info = pm.getActivityInfo(ComponentName(RuntimeEnvironment.getApplication(), name), 0)
            assertEquals(AssistantActivity::class.java.name, info.targetActivity)
            assertEquals(label, info.loadLabel(pm).toString())
            assertEquals(ActivityInfo.LAUNCH_SINGLE_TOP, info.launchMode)
            val launch = Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER)
                .setComponent(ComponentName(RuntimeEnvironment.getApplication(), name))
            assertNotNull(pm.resolveActivity(launch, 0))
        }
    }

    @Test fun conversationLauncherStartsOnceAfterResumeAndKeepsCurrentChatDraftAndGlobalChoice() {
        Shadows.shadowOf(RuntimeEnvironment.getApplication()).grantPermissions(Manifest.permission.RECORD_AUDIO)
        SyntheticAssistantActivity.preferenceName = "synthetic-conversation-entry-disabled"
        RuntimeEnvironment.getApplication().getSharedPreferences(SyntheticAssistantActivity.preferenceName, Activity.MODE_PRIVATE)
            .edit().putBoolean("conversation-mode", false).commit()
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java, conversationLaunchIntent()).create().start()
        val activity = controller.get()
        val chat = "{\"replica\":\"synthetic-entry-chat\",\"sequence\":\"1\"}"
        activity.synthetic.state = activity.synthetic.state.copy(conversation = chat)
        activity.audio.onConversation = {
            assertEquals(1, activity.synthetic.resumes)
            assertEquals(true, activity.audio.foregrounds.last())
            assertEquals(true, activity.audio.speechChoices.last())
        }
        try {
            Shadows.shadowOf(Looper.getMainLooper()).idle()
            assertEquals(0, activity.audio.conversations)
            controller.resume().visible()
            Shadows.shadowOf(Looper.getMainLooper()).idle()
            assertEquals(1, activity.audio.conversations)
            assertEquals(0, activity.audio.dictations)
            assertEquals(listOf("Synthetic old draft"), activity.audio.conversationDrafts)
            assertEquals(chat, activity.synthetic.state.conversation)
            assertEquals(0, activity.synthetic.newConversations)
            assertEquals(0, activity.synthetic.sends)
            assertFalse(activity.voicePreferences.conversationMode)
            assertFalse(activity.getSharedPreferences(SyntheticAssistantActivity.preferenceName, Activity.MODE_PRIVATE)
                .getBoolean("conversation-mode", true))
            activity.audio.onConversation = {}
            controller.pause().stop().start().resume()
            assertEquals(1, activity.audio.conversations)
            controller.newIntent(conversationLaunchIntent())
            assertEquals(2, activity.audio.conversations)
        } finally {
            controller.pause().stop().destroy()
            SyntheticAssistantActivity.preferenceName = "synthetic-assistant-activity-defaults"
        }
    }

    @Test fun bothEntryLaunchesWaitForHydrationOnceAndNeverReplayAfterRecreationOrBackground() {
        Shadows.shadowOf(RuntimeEnvironment.getApplication()).grantPermissions(Manifest.permission.RECORD_AUDIO)
        for ((intent, conversation) in listOf(voiceLaunchIntent() to false, conversationLaunchIntent() to true)) {
            SyntheticAssistantActivity.retainedClient = SyntheticAssistantClient().apply { state = state.copy(foreground = false) }
            val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java, intent).setup()
            val looper = Shadows.shadowOf(Looper.getMainLooper())
            try {
                val original = controller.get()
                looper.idle()
                assertEquals(0, original.audio.dictations + original.audio.conversations)
                original.synthetic.publish(original.synthetic.state.copy(foreground = true, draft = "Synthetic admitted draft"))
                looper.idle()
                assertEquals(if (conversation) 1 else 0, original.audio.conversations)
                assertEquals(if (conversation) 0 else 1, original.audio.dictations)
                assertEquals(listOf("Synthetic admitted draft"), if (conversation) original.audio.conversationDrafts else original.audio.dictatedDrafts)
                original.synthetic.publish(original.synthetic.state); looper.idle()
                assertEquals(1, original.audio.dictations + original.audio.conversations)
                controller.configurationChange(android.content.res.Configuration(original.resources.configuration).apply { fontScale += 0.1f })
                looper.idle()
                val replacement = controller.get()
                assertEquals(0, replacement.audio.dictations + replacement.audio.conversations)
                replacement.synthetic.publish(replacement.synthetic.state.copy(foreground = false)); looper.idle()
                controller.newIntent(intent)
                controller.pause().stop()
                replacement.synthetic.publish(replacement.synthetic.state.copy(foreground = true)); looper.idle()
                controller.start().resume(); looper.idle()
                assertEquals(0, replacement.audio.dictations + replacement.audio.conversations)
            } finally { controller.pause().stop().destroy(); SyntheticAssistantActivity.retainedClient = null }
        }
    }

    @Test fun bothEntriesRetainActiveEditorPendingPermissionCapabilityAndBusyGuards() {
        Shadows.shadowOf(RuntimeEnvironment.getApplication()).grantPermissions(Manifest.permission.RECORD_AUDIO)
        for (intent in listOf(voiceLaunchIntent(), conversationLaunchIntent())) {
            val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
            val activity = controller.get()
            val looper = Shadows.shadowOf(Looper.getMainLooper())
            try {
                activity.audio.state = activity.audio.state.copy(loop = dev.gitblame.voxgolem.assistant.speech.ConversationLoopState(
                    dev.gitblame.voxgolem.assistant.speech.LoopPhase.LISTENING))
                controller.newIntent(intent)
                activity.audio.state = activity.audio.state.copy(loop = dev.gitblame.voxgolem.assistant.speech.ConversationLoopState())
                activity.synthetic.publish(activity.synthetic.state); looper.idle()
                assertEquals(0, activity.audio.dictations + activity.audio.conversations)
                activity.editDraft(); controller.newIntent(intent)
                activity.receiveText(Activity.RESULT_CANCELED, null); looper.idle()
                assertEquals(0, activity.audio.dictations + activity.audio.conversations)
                activity.synthetic.state = activity.synthetic.state.copy(busy = true)
                controller.newIntent(intent)
                activity.synthetic.publish(activity.synthetic.state.copy(busy = false)); looper.idle()
                assertEquals(0, activity.audio.dictations + activity.audio.conversations)
                Shadows.shadowOf(RuntimeEnvironment.getApplication()).denyPermissions(Manifest.permission.RECORD_AUDIO)
                controller.newIntent(intent); controller.newIntent(intent)
                activity.receiveMicrophonePermission(false)
                activity.receiveMicrophonePermission(true); looper.idle()
                assertEquals(0, activity.audio.dictations + activity.audio.conversations)
                Shadows.shadowOf(RuntimeEnvironment.getApplication()).grantPermissions(Manifest.permission.RECORD_AUDIO)
                activity.audio.state = activity.audio.state.copy(admissionReady = false)
                controller.newIntent(intent)
                assertEquals(1, activity.audio.dictations + activity.audio.conversations)
                // The controller owns the capability error; no permission dialog or second launch is queued.
                activity.audio.state = activity.audio.state.copy(admissionReady = true)
                activity.synthetic.publish(activity.synthetic.state); looper.idle()
                assertEquals(1, activity.audio.dictations + activity.audio.conversations)
            } finally { controller.pause().stop().destroy() }
        }
    }

    @Test fun recreationBeforeNativeHydrationRetiresBothPendingLauncherRequests() {
        Shadows.shadowOf(RuntimeEnvironment.getApplication()).grantPermissions(Manifest.permission.RECORD_AUDIO)
        for (intent in listOf(voiceLaunchIntent(), conversationLaunchIntent())) {
            SyntheticAssistantActivity.retainedClient = SyntheticAssistantClient().apply { state = state.copy(foreground = false) }
            val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java, intent).setup()
            val looper = Shadows.shadowOf(Looper.getMainLooper())
            try {
                val original = controller.get()
                looper.idle()
                assertEquals(0, original.audio.dictations + original.audio.conversations)
                controller.configurationChange(android.content.res.Configuration(original.resources.configuration).apply { fontScale += 0.1f })
                val replacement = controller.get()
                replacement.synthetic.publish(replacement.synthetic.state.copy(foreground = true)); looper.idle()
                assertEquals(0, replacement.audio.dictations + replacement.audio.conversations)
                controller.pause().stop().start().resume(); looper.idle()
                assertEquals(0, replacement.audio.dictations + replacement.audio.conversations)
            } finally { controller.pause().stop().destroy(); SyntheticAssistantActivity.retainedClient = null }
        }
    }

    @Test fun latestExplicitAliasSelectsTheEntryActionWhileWaitingForHydration() {
        Shadows.shadowOf(RuntimeEnvironment.getApplication()).grantPermissions(Manifest.permission.RECORD_AUDIO)
        SyntheticAssistantActivity.retainedClient = SyntheticAssistantClient().apply { state = state.copy(foreground = false) }
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java, conversationLaunchIntent()).setup()
        val activity = controller.get()
        val looper = Shadows.shadowOf(Looper.getMainLooper())
        try {
            controller.newIntent(voiceLaunchIntent())
            activity.synthetic.publish(activity.synthetic.state.copy(foreground = true)); looper.idle()
            assertEquals(1, activity.audio.dictations)
            assertEquals(0, activity.audio.conversations)
            assertEquals(0, activity.synthetic.newConversations)
        } finally { controller.pause().stop().destroy(); SyntheticAssistantActivity.retainedClient = null }
    }

    @Test fun launcherAliasWithoutMainLauncherIntentOrAfterFinishCannotStartAudio() {
        Shadows.shadowOf(RuntimeEnvironment.getApplication()).grantPermissions(Manifest.permission.RECORD_AUDIO)
        for (entry in listOf(AssistantActivity.VOICE_ENTRY, AssistantActivity.CONVERSATION_ENTRY)) {
            val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
            val activity = controller.get()
            try {
                controller.newIntent(Intent(Intent.ACTION_VIEW).addCategory(Intent.CATEGORY_LAUNCHER)
                    .setComponent(ComponentName(activity, entry)))
                controller.newIntent(Intent(Intent.ACTION_MAIN).setComponent(ComponentName(activity, entry)))
                activity.finish()
                controller.newIntent(Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER)
                    .setComponent(ComponentName(activity, entry)))
                assertEquals(0, activity.audio.dictations + activity.audio.conversations)
            } finally { controller.pause().stop().destroy() }
        }
    }

    @Test fun askLauncherUsesTheSingleTurnCallbackOnceAndPreservesTheCurrentDraft() {
        Shadows.shadowOf(RuntimeEnvironment.getApplication()).grantPermissions(Manifest.permission.RECORD_AUDIO)
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java, voiceLaunchIntent()).setup()
        val activity = controller.get()
        try {
            Shadows.shadowOf(Looper.getMainLooper()).idle()
            assertEquals(1, activity.audio.dictations)
            assertEquals(0, activity.audio.conversations)
            assertEquals(0, activity.synthetic.sends)
            assertEquals("Synthetic old draft", activity.synthetic.state.draft)
            assertEquals(listOf("Synthetic old draft"), activity.audio.dictatedDrafts)
            assertEquals(listOf("Synthetic old draft"), activity.audio.askedDrafts)
            controller.pause().stop().start().resume()
            assertEquals(1, activity.audio.dictations)
            controller.newIntent(voiceLaunchIntent())
            assertEquals(2, activity.audio.dictations)
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun permissionCompletionCannotMoveAnAskToADifferentConversationOrNewerDraft() {
        Shadows.shadowOf(RuntimeEnvironment.getApplication()).denyPermissions(Manifest.permission.RECORD_AUDIO)
        for (changeConversation in listOf(true, false)) {
            val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
            val activity = controller.get()
            try {
                activity.requestVoice(false)
                activity.synthetic.state = if (changeConversation) activity.synthetic.state.copy(conversation = "synthetic-new-context")
                    else activity.synthetic.state.copy(draft = "Synthetic newer typing")
                activity.receiveMicrophonePermission(true)
                assertTrue(activity.audio.askedDrafts.isEmpty())
                assertEquals(0, activity.synthetic.newConversations)
                assertEquals(0, activity.synthetic.sends)
            } finally { controller.pause().stop().destroy() }
        }
    }

    @Test fun voiceLauncherWaitsForForegroundHydrationButDoesNotReplayAfterRecreation() {
        Shadows.shadowOf(RuntimeEnvironment.getApplication()).grantPermissions(Manifest.permission.RECORD_AUDIO)
        SyntheticAssistantActivity.retainedClient = SyntheticAssistantClient().apply { state = state.copy(foreground = false) }
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java, voiceLaunchIntent()).setup()
        try {
            val original = controller.get()
            val looper = Shadows.shadowOf(Looper.getMainLooper())
            looper.idle(); assertEquals(0, original.audio.dictations)
            original.synthetic.publish(original.synthetic.state.copy(foreground = true, draft = "Synthetic hydrated draft"))
            looper.idle(); assertEquals(1, original.audio.dictations)
            assertEquals(listOf("Synthetic hydrated draft"), original.audio.dictatedDrafts)
            controller.configurationChange(android.content.res.Configuration(original.resources.configuration).apply { fontScale += 0.1f })
            looper.idle()
            assertEquals(0, controller.get().audio.dictations)
            assertEquals("Synthetic hydrated draft", controller.get().synthetic.state.draft)
        } finally {
            controller.pause().stop().destroy()
            SyntheticAssistantActivity.retainedClient = null
        }
    }

    @Test fun voiceLauncherDoesNotRetryWhenGenerationWasBusyOrPermissionWasDenied() {
        Shadows.shadowOf(RuntimeEnvironment.getApplication()).grantPermissions(Manifest.permission.RECORD_AUDIO)
        SyntheticAssistantActivity.retainedClient = SyntheticAssistantClient().apply { state = state.copy(busy = true) }
        val busy = Robolectric.buildActivity(SyntheticAssistantActivity::class.java, voiceLaunchIntent()).setup()
        try {
            val activity = busy.get()
            activity.synthetic.publish(activity.synthetic.state.copy(busy = false))
            Shadows.shadowOf(Looper.getMainLooper()).idle()
            assertEquals(0, activity.audio.dictations)
        } finally { busy.pause().stop().destroy(); SyntheticAssistantActivity.retainedClient = null }
        Shadows.shadowOf(RuntimeEnvironment.getApplication()).denyPermissions(Manifest.permission.RECORD_AUDIO)
        val denied = Robolectric.buildActivity(SyntheticAssistantActivity::class.java, voiceLaunchIntent()).setup()
        try {
            denied.get().receiveMicrophonePermission(false)
            denied.get().receiveMicrophonePermission(true)
            assertEquals(0, denied.get().audio.dictations)
        } finally { denied.pause().stop().destroy() }
    }

    @Test fun configurationRecreationRetainsPendingHistoryButAcceptsTheReplacementRecognizersFirstUtterance() {
        SyntheticAssistantActivity.retainedClient = SyntheticAssistantClient()
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val looper = Shadows.shadowOf(Looper.getMainLooper())
        try {
            val original = controller.get()
            looper.idle()
            val dictation = dev.gitblame.voxgolem.assistant.recognition.DictationState(
                phase = dev.gitblame.voxgolem.assistant.recognition.DictationPhase.LISTENING,
                draft = "Synthetic first partial", utterance = 1)
            original.audio.publish(original.audio.state.copy(dictation = dictation)); looper.idle()
            original.synthetic.publish(original.synthetic.state.copy(draft = "Synthetic first final"))
            original.audio.publish(original.audio.state.copy(dictation = dictation.copy(
                phase = dev.gitblame.voxgolem.assistant.recognition.DictationPhase.FINAL,
                draft = "Synthetic first final"))); looper.idle()
            original.synthetic.publish(original.synthetic.state.copy(draft = "")); looper.idle()
            assertTrue(original.pendingPrompts.isEmpty())
            controller.configurationChange(android.content.res.Configuration(original.resources.configuration).apply { fontScale += 0.1f })
            val replacement = controller.get()
            assertNotSame(original, replacement)
            assertSame(original.synthetic, replacement.synthetic)
            replacement.audio.publish(replacement.audio.state.copy(dictation = dictation.copy(draft = "Synthetic replacement partial")))
            looper.idle()
            assertEquals(listOf("Synthetic replacement partial"), replacement.pendingPrompts.map { it.text })
            val conversation = "{\"replica\":\"synthetic-recreated-chat\",\"sequence\":\"1\"}"
            val head = "{\"replica\":\"synthetic-recreated-turn\",\"sequence\":\"1\"}"
            val admitted = replacement.synthetic.state.copy(conversation = conversation, busy = true,
                draft = "Synthetic replacement partial", clientRequestToken = "synthetic-recreated-request")
            replacement.synthetic.publish(admitted)
            replacement.synthetic.publish(admitted.copy(busy = false, selectedHead = head, draft = ""))
            looper.idle()
            val pending = replacement.pendingPrompts
            assertEquals(listOf("Synthetic replacement partial"), pending.map { it.text })
            controller.configurationChange(android.content.res.Configuration(replacement.resources.configuration).apply { fontScale += 0.1f }); looper.idle()
            assertEquals(pending, controller.get().pendingPrompts)
        } finally {
            controller.pause().stop().destroy()
            SyntheticAssistantActivity.retainedClient = null
        }
    }
    @Test fun nativeAdmissionAndSettlementKeepYouWhenTheUiSnapshotQueueCoalescesThem() {
        for (fresh in listOf(false, true)) {
            val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
            val activity = controller.get()
            val looper = Shadows.shadowOf(Looper.getMainLooper())
            looper.idle()
            val conversation = "{\"replica\":\"synthetic-coalesced-chat\",\"sequence\":\"1\"}"
            val oldHead = "{\"replica\":\"synthetic-coalesced-turn\",\"sequence\":\"30\"}"
            val head = "{\"replica\":\"synthetic-coalesced-turn\",\"sequence\":\"31\"}"
            val initial = activity.synthetic.state.copy(conversation = if (fresh) null else conversation,
                selectedHead = if (fresh) null else oldHead, draft = "Synthetic coalesced prompt")
            activity.synthetic.publish(initial); looper.idle()
            val admitted = initial.copy(conversation = conversation, busy = true, clientRequestToken = "synthetic-coalesced-request")
            val settled = admitted.copy(busy = false, selectedHead = head, draft = "")
            activity.synthetic.publish(admitted)
            activity.synthetic.publish(settled)
            looper.idle()
            assertEquals(listOf("Synthetic coalesced prompt"), activity.pendingPrompts.map { it.text })
            if (!fresh) { activity.synthetic.historyReads.last().second(null, settled); looper.idle() }
            val page = """{"revision":"0","conversation":$conversation,"head":$head,"older_before":null,"turns":[{"operation":{"id":$head,"body":{"kind":"turn_completed","conversation":$conversation,"prompt":"Synthetic coalesced prompt","answer":{"kind":"text","text":"Synthetic canonical answer"}}}}]}"""
            activity.synthetic.historyReads.last().second(page, settled); looper.idle()
            assertTrue(activity.pendingPrompts.isEmpty())
            assertEquals(listOf("Synthetic coalesced prompt"), activity.historyState.turns.map { it.prompt })
            controller.pause().stop().destroy()
        }
    }
    @Test fun failedInitialDefaultPersistenceDoesNotCrashOrEnableExplicitlyDisabledSpeech() {
        val controller = Robolectric.buildActivity(FailedInitialVoicePreferencesActivity::class.java).setup()
        val activity = controller.get()
        assertEquals(WearVoicePreferences(false, true), activity.voicePreferences)
        assertEquals(listOf(false), activity.audio.speechChoices)
        controller.pause().stop().start().resume()
        assertEquals(WearVoicePreferences(false, true), activity.voicePreferences)
        assertEquals(listOf(false, false), activity.audio.speechChoices)
        controller.pause().stop().destroy()
    }
    @Test fun manualSendUsesVoiceTerminalPathOnceAndRetainsLegacyClientAdapterCompatibility() {
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val activity = controller.get()
        activity.audio.handlesManualSend = true
        activity.sendManually()
        assertEquals(1, activity.audio.manualSends)
        assertEquals(0, activity.synthetic.sends)
        activity.audio.handlesManualSend = false
        activity.sendManually()
        assertEquals(2, activity.audio.manualSends)
        assertEquals(1, activity.synthetic.sends)
        activity.synthetic.state = activity.synthetic.state.copy(busy = true)
        activity.sendManually()
        controller.pause().stop()
        activity.sendManually()
        assertEquals(2, activity.audio.manualSends)
        assertEquals(1, activity.synthetic.sends)
        controller.destroy()
    }

    @Test fun bothDefaultsAreSavedWithoutStartingRecognitionAndFalseChoicesSurviveRestart() {
        SyntheticAssistantActivity.preferenceName = "synthetic-voice-preferences-${java.util.UUID.randomUUID()}"
        val first = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val activity = first.get()
        assertEquals(WearVoicePreferences(true, true), activity.voicePreferences)
        assertEquals(listOf(true), activity.audio.speechChoices)
        val savedPreferences = activity.getSharedPreferences(SyntheticAssistantActivity.preferenceName, Activity.MODE_PRIVATE)
        assertEquals(true, savedPreferences.all["read-replies-aloud"])
        assertEquals(true, savedPreferences.all["conversation-mode"])
        assertEquals(0, activity.audio.dictations)
        assertEquals(0, activity.audio.conversations)
        activity.setReadRepliesAloud(false)
        activity.setConversationMode(false)
        first.pause().stop().destroy()
        val restarted = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        assertEquals(WearVoicePreferences(false, false), restarted.get().voicePreferences)
        assertEquals(listOf(false), restarted.get().audio.speechChoices)
        restarted.get().setReadRepliesAloud(true)
        restarted.get().setConversationMode(true)
        restarted.pause().stop().destroy()
        val enabledAgain = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        assertEquals(WearVoicePreferences(true, true), enabledAgain.get().voicePreferences)
        assertEquals(listOf(true), enabledAgain.get().audio.speechChoices)
        enabledAgain.pause().stop().destroy()
        SyntheticAssistantActivity.preferenceName = "synthetic-assistant-activity-defaults"
    }

    @Test fun keyboardAlwaysLaunchesThePrefilledNativeEditorEvenWhenAnExternalChooserResolves() {
        val chooser = Intent("android.support.wearable.input.action.REMOTE_INPUT")
        Shadows.shadowOf(RuntimeEnvironment.getApplication().packageManager).addResolveInfoForIntent(chooser, ResolveInfo().apply {
            activityInfo = ActivityInfo().apply { packageName = "synthetic.external.chooser"; name = "SyntheticChooserActivity" }
        })
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val activity = controller.get()
        assertNotNull(chooser.resolveActivity(activity.packageManager))
        activity.editDraft()
        val launch = Shadows.shadowOf(activity).nextStartedActivityForResult.intent
        assertEquals(AssistantTextEntryActivity::class.java.name, launch.component?.className)
        assertEquals("Synthetic old draft", launch.getStringExtra(AssistantTextInput.DRAFT))
        activity.editDraft()
        assertNull(Shadows.shadowOf(activity).nextStartedActivityForResult)
        activity.receiveText(Activity.RESULT_CANCELED, null)
        assertEquals("Synthetic old draft", activity.synthetic.state.draft)
        assertEquals(0, activity.synthetic.sends)
        controller.pause().stop().destroy()
    }

    @Test fun editorCapturesCurrentPartialBeforeStopQueuesPersistenceAndClearsTheVoiceSnapshot() {
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val activity = controller.get()
        val looper = Shadows.shadowOf(Looper.getMainLooper())
        try {
            val partial = "Synthetic old draft plus current partial"
            activity.audio.state = activity.audio.state.copy(dictation = dev.gitblame.voxgolem.assistant.recognition.DictationState(
                phase = dev.gitblame.voxgolem.assistant.recognition.DictationPhase.LISTENING, draft = partial, utterance = 1))
            activity.audio.onStop = {
                android.os.Handler(Looper.getMainLooper()).post { activity.synthetic.setDraft(partial) }
                activity.audio.state = AssistantVoiceState()
            }
            activity.editDraft()
            val launch = Shadows.shadowOf(activity).nextStartedActivityForResult.intent
            assertEquals("The actor has not yet persisted the interrupted partial", "Synthetic old draft", activity.synthetic.state.draft)
            assertEquals(partial, launch.getStringExtra(AssistantTextInput.DRAFT))
            assertEquals(1, activity.audio.stops)
            looper.idle()
            activity.receiveText(Activity.RESULT_CANCELED, null)
            assertEquals(partial, activity.synthetic.state.draft)
            assertEquals(0, activity.synthetic.sends)
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun editorDoesNotUseRetiredFinalSpeechOrAdmitInputAfterNativeDismissal() {
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val activity = controller.get()
        try {
            activity.audio.state = activity.audio.state.copy(dictation = dev.gitblame.voxgolem.assistant.recognition.DictationState(
                phase = dev.gitblame.voxgolem.assistant.recognition.DictationPhase.FINAL, draft = "Synthetic retired final"))
            activity.editDraft()
            assertEquals("Synthetic old draft", Shadows.shadowOf(activity).nextStartedActivityForResult.intent.getStringExtra(AssistantTextInput.DRAFT))
            activity.receiveText(Activity.RESULT_CANCELED, null)
            activity.finish()
            activity.editDraft()
            assertNull(Shadows.shadowOf(activity).nextStartedActivityForResult)
            assertEquals(0, activity.synthetic.sends)
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun policyTogglesDelegateToTheNativeClientWithoutChangingVoiceChoicesOrStartingWork() {
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val activity = controller.get()
        try {
            val preferences = activity.voicePreferences
            activity.audio.state = activity.audio.state.copy(localOnlyRecognitionAvailable = true)
            activity.setWebEnabled(false)
            activity.setSpeechNetworkFallback(false)
            assertFalse(activity.synthetic.snapshot().webEnabled)
            assertFalse(activity.synthetic.snapshot().speechNetworkFallback)
            assertEquals(preferences, activity.voicePreferences)
            assertEquals(0, activity.audio.dictations)
            assertEquals(0, activity.audio.conversations)
            assertEquals(0, activity.synthetic.sends)
            activity.setWebEnabled(true)
            activity.setSpeechNetworkFallback(true)
            assertTrue(activity.synthetic.snapshot().webEnabled)
            assertTrue(activity.synthetic.snapshot().speechNetworkFallback)
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun delayedSpeechPersistenceFencesCrossFieldSelectionsThenPreservesTheAcknowledgedLanguage() {
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val activity = controller.get()
        try {
            val saved = activity.synthetic.state.copy(speechLanguage = "en-AU", speechTtsProvider = "synthetic.engine.one",
                speechTtsVoice = "synthetic-voice-one")
            val preferences = activity.voicePreferences
            val storedVoicePreferences = activity.getSharedPreferences(SyntheticAssistantActivity.preferenceName, Activity.MODE_PRIVATE).all
            activity.synthetic.publish(saved)
            activity.audio.state = activity.audio.state.copy(speechSelection = SpeechSelection("fr-FR", "synthetic.stale.engine", "synthetic-stale-voice"))
            activity.setSpeechLanguage("en-GB")
            val first = SpeechSelection("en-GB", saved.speechTtsProvider, saved.speechTtsVoice)
            assertEquals(first, activity.audio.submittedSpeechSelections.single())
            assertEquals(saved.speechAdmission().selection, activity.synthetic.snapshot().speechAdmission().selection)
            assertNull(activity.speechSelectionError)
            assertTrue(activity.speechSelectionPending)
            activity.setSpeechTtsProvider("synthetic.engine.two")
            activity.setSpeechTtsVoice("synthetic-voice-two")
            activity.resetSpeechSelection()
            assertEquals("Additional choices are not queued against the old persisted fields", listOf(first), activity.audio.submittedSpeechSelections)
            assertEquals("Accepted/submitted is not an actor persistence acknowledgement", saved.speechAdmission().selection,
                activity.synthetic.snapshot().speechAdmission().selection)
            val firstPersisted = saved.copy(speechLanguage = "en-GB")
            activity.synthetic.publish(firstPersisted)
            activity.audio.speechSettlements[0](true)
            Shadows.shadowOf(Looper.getMainLooper()).idle()
            assertFalse(activity.speechSelectionPending)
            assertNull(activity.speechSelectionError)
            activity.setSpeechTtsProvider("synthetic.engine.two")
            assertEquals(SpeechSelection("en-GB", "synthetic.engine.two", ""), activity.audio.submittedSpeechSelections.last())
            assertEquals(firstPersisted.speechAdmission().selection, activity.synthetic.snapshot().speechAdmission().selection)
            activity.synthetic.publish(firstPersisted.copy(speechTtsProvider = "synthetic.engine.two", speechTtsVoice = ""))
            activity.audio.speechSettlements[1](true)
            Shadows.shadowOf(Looper.getMainLooper()).idle()
            activity.setSpeechTtsVoice("synthetic-new-engine-voice")
            assertEquals(SpeechSelection("en-GB", "synthetic.engine.two", "synthetic-new-engine-voice"), activity.audio.submittedSpeechSelections.last())
            assertEquals(0, activity.synthetic.sends)
            assertEquals(0, activity.audio.dictations)
            assertEquals(preferences, activity.voicePreferences)
            assertEquals(storedVoicePreferences, activity.getSharedPreferences(SyntheticAssistantActivity.preferenceName, Activity.MODE_PRIVATE).all)
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun rejectedSpeechSelectionIsActionableAndResetIsExplicitWithoutChangingStoredChoices() {
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val activity = controller.get()
        try {
            val saved = activity.synthetic.state.copy(speechLanguage = "en-AU", speechTtsProvider = "synthetic.removed.engine", speechTtsVoice = "synthetic-removed-voice")
            activity.synthetic.publish(saved)
            activity.audio.acceptsSpeechSelection = false
            activity.resetSpeechSelection()
            assertEquals(SpeechSelection(), activity.audio.submittedSpeechSelections.single())
            assertEquals("speech_selection_rejected", activity.speechSelectionError)
            assertFalse(activity.speechSelectionPending)
            assertTrue(assistantSpeechSelectionErrorText(activity.speechSelectionError!!).contains("not accepted"))
            assertEquals(saved.speechAdmission().selection, activity.synthetic.snapshot().speechAdmission().selection)
            activity.audio.acceptsSpeechSelection = true
            activity.resetSpeechSelection()
            assertNull(activity.speechSelectionError)
            assertTrue(activity.speechSelectionPending)
            assertEquals(saved.speechAdmission().selection, activity.synthetic.snapshot().speechAdmission().selection)
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun metadataCoalescingAndEvenAnEqualSelectionSnapshotCannotAcknowledgeSpeechPersistence() {
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val activity = controller.get()
        val looper = Shadows.shadowOf(Looper.getMainLooper())
        try {
            activity.setSpeechLanguage("en-AU")
            activity.synthetic.publish(activity.synthetic.state.copy(provisional = "Synthetic unrelated metadata update", error = "settings_not_saved"))
            activity.audio.publish(activity.audio.state.copy(speechOptions = dev.gitblame.voxgolem.assistant.speech.SpeechOptions(ttsProviders = listOf("synthetic.reported.engine"))))
            activity.synthetic.publish(activity.synthetic.state.copy(speechLanguage = "en-AU", error = null))
            looper.idle()
            assertTrue("Only the explicit operation callback can settle the pending request", activity.speechSelectionPending)
            activity.setSpeechTtsProvider("synthetic.reported.engine")
            assertEquals(1, activity.audio.submittedSpeechSelections.size)
            activity.audio.speechSettlements.single()(true)
            looper.idle()
            assertFalse(activity.speechSelectionPending)
            activity.setSpeechTtsProvider("synthetic.reported.engine")
            assertEquals(SpeechSelection("en-AU", "synthetic.reported.engine", ""), activity.audio.submittedSpeechSelections.last())
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun alreadyEqualRequestedFieldsStillWaitForExplicitSettlementNotSnapshotEquality() {
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val activity = controller.get()
        try {
            activity.resetSpeechSelection()
            assertEquals(SpeechSelection(), activity.synthetic.snapshot().speechAdmission().selection)
            Shadows.shadowOf(Looper.getMainLooper()).idle()
            assertTrue(activity.speechSelectionPending)
            activity.setSpeechLanguage("en-AU")
            assertEquals(listOf(SpeechSelection()), activity.audio.submittedSpeechSelections)
            activity.audio.speechSettlements.single()(false)
            Shadows.shadowOf(Looper.getMainLooper()).idle()
            assertFalse(activity.speechSelectionPending)
            assertEquals("speech_selection_not_saved", activity.speechSelectionError)
            activity.setSpeechLanguage("en-AU")
            assertEquals(SpeechSelection("en-AU"), activity.audio.submittedSpeechSelections.last())
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun persistenceFailureFromAnotherThreadReleasesTheFenceOnlyOnMainAndAllowsRetry() {
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val activity = controller.get()
        val looper = Shadows.shadowOf(Looper.getMainLooper())
        try {
            val original = activity.synthetic.snapshot().speechAdmission().selection
            activity.setSpeechLanguage("en-AU")
            val callback = activity.audio.speechSettlements.single()
            val worker = Thread { callback(false) }
            worker.start(); worker.join(2_000)
            assertFalse("Synthetic callback worker must finish", worker.isAlive)
            assertTrue("The worker cannot mutate UI state directly", activity.speechSelectionPending)
            looper.idle()
            assertFalse(activity.speechSelectionPending)
            assertEquals("speech_selection_not_saved", activity.speechSelectionError)
            assertTrue(assistantSpeechSelectionErrorText(activity.speechSelectionError!!).contains("not saved"))
            assertEquals(original, activity.synthetic.snapshot().speechAdmission().selection)
            activity.setSpeechLanguage("en-GB")
            assertTrue(activity.speechSelectionPending)
            assertEquals(SpeechSelection("en-GB"), activity.audio.submittedSpeechSelections.last())
            callback(true)
            looper.idle()
            assertTrue("A duplicate old settlement cannot release the retry's token", activity.speechSelectionPending)
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun unsupportedSubmissionAndRejectedReturnCannotFakeAckThroughLegacyOrQueuedCallbacks() {
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val activity = controller.get()
        val looper = Shadows.shadowOf(Looper.getMainLooper())
        try {
            activity.audio.speechSubmissionFailure = UnsupportedOperationException("Synthetic legacy callback API unsupported")
            activity.setSpeechLanguage("en-AU")
            assertFalse(activity.speechSelectionPending)
            assertEquals("speech_selection_rejected", activity.speechSelectionError)
            assertEquals(0, activity.audio.legacySpeechConfigurations)
            activity.audio.speechSubmissionFailure = null
            activity.audio.acceptsSpeechSelection = false
            activity.audio.synchronousSpeechSettlement = true
            activity.setSpeechLanguage("en-AU")
            assertFalse(activity.speechSelectionPending)
            looper.idle()
            assertEquals("A rejected local return takes precedence over its queued callback", "speech_selection_rejected", activity.speechSelectionError)
            activity.audio.acceptsSpeechSelection = true
            activity.audio.synchronousSpeechSettlement = false
            activity.setSpeechLanguage("en-AU")
            assertTrue("Even synchronous settlement is marshalled through Main", activity.speechSelectionPending)
            looper.idle()
            assertFalse(activity.speechSelectionPending)
            assertEquals("speech_selection_not_saved", activity.speechSelectionError)
            activity.audio.synchronousSpeechSettlement = null
            activity.setSpeechLanguage("en-AU")
            assertTrue(activity.speechSelectionPending)
            assertEquals("", activity.synthetic.snapshot().speechLanguage)
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun pauseResumeAndDestroyFenceOldSpeechSettlementsWithoutPromotingOrClearingANewRequest() {
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val activity = controller.get()
        val looper = Shadows.shadowOf(Looper.getMainLooper())
        activity.setSpeechLanguage("en-AU")
        val oldCallback = activity.audio.speechSettlements.single()
        controller.pause().stop()
        assertFalse(activity.speechSelectionPending)
        assertEquals("speech_selection_unconfirmed", activity.speechSelectionError)
        controller.start().resume()
        activity.setSpeechLanguage("en-GB")
        val freshCallback = activity.audio.speechSettlements.last()
        oldCallback(true); looper.idle()
        assertTrue(activity.speechSelectionPending)
        controller.pause().stop().destroy()
        val errorAfterDestroy = activity.speechSelectionError
        freshCallback(true); oldCallback(false); looper.idle()
        assertFalse(activity.speechSelectionPending)
        assertEquals(errorAfterDestroy, activity.speechSelectionError)
        assertEquals("", activity.synthetic.snapshot().speechLanguage)
        assertEquals(0, activity.synthetic.sends)
    }

    @Test fun speechSelectorsKeepForegroundBusyVoiceAndEditorAdmissionFences() {
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val activity = controller.get()
        try {
            activity.synthetic.state = activity.synthetic.state.copy(busy = true)
            activity.setSpeechLanguage("en-AU")
            activity.synthetic.state = activity.synthetic.state.copy(busy = false)
            activity.audio.state = activity.audio.state.copy(dictation = dev.gitblame.voxgolem.assistant.recognition.DictationState(
                phase = dev.gitblame.voxgolem.assistant.recognition.DictationPhase.LISTENING))
            activity.setSpeechTtsVoice("synthetic-voice")
            activity.audio.state = AssistantVoiceState()
            activity.editDraft()
            activity.resetSpeechSelection()
            activity.receiveText(Activity.RESULT_CANCELED, null)
            controller.pause().stop()
            activity.setSpeechTtsProvider("synthetic.engine")
            assertTrue(activity.audio.submittedSpeechSelections.isEmpty())
            assertEquals("speech_selection_rejected", activity.speechSelectionError)
        } finally { controller.destroy() }
    }

    @Test fun lostLocalCapabilityCannotTrapAnAlreadyDisabledNetworkFallback() {
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val activity = controller.get()
        try {
            activity.audio.state = activity.audio.state.copy(localOnlyRecognitionAvailable = false)
            activity.setSpeechNetworkFallback(false)
            assertTrue(activity.synthetic.snapshot().speechNetworkFallback)
            activity.synthetic.publish(activity.synthetic.state.copy(speechNetworkFallback = false))
            activity.setSpeechNetworkFallback(true)
            assertTrue(activity.synthetic.snapshot().speechNetworkFallback)
            assertNull(activity.speechSelectionError)
            assertEquals(0, activity.audio.dictations)
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun openingEditorRetiresPendingMicrophonePermissionAndNeverPrefillsAnEmptyCheckingSnapshot() {
        Shadows.shadowOf(RuntimeEnvironment.getApplication()).denyPermissions(Manifest.permission.RECORD_AUDIO)
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val activity = controller.get()
        try {
            activity.requestVoice(false)
            activity.audio.state = activity.audio.state.copy(dictation = dev.gitblame.voxgolem.assistant.recognition.DictationState(
                phase = dev.gitblame.voxgolem.assistant.recognition.DictationPhase.CHECKING))
            activity.editDraft()
            assertEquals("Synthetic old draft", Shadows.shadowOf(activity).nextStartedActivityForResult.intent.getStringExtra(AssistantTextInput.DRAFT))
            activity.receiveMicrophonePermission(true)
            assertEquals(0, activity.audio.dictations)
            assertEquals(0, activity.audio.conversations)
            assertEquals(0, activity.synthetic.sends)
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun historyCallbacksAreRetiredAcrossBackgroundAndReloadTheCurrentNativeSelection() {
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val activity = controller.get()
        val looper = Shadows.shadowOf(Looper.getMainLooper())
        val selected = activity.synthetic.state.copy(conversation = "{\"replica\":\"synthetic-chat\",\"sequence\":\"1\"}")
        activity.synthetic.publish(selected)
        looper.idle()
        assertEquals(1, activity.synthetic.historyReads.size)
        controller.pause()
        activity.synthetic.historyReads[0].second(null, selected)
        looper.idle()
        assertNull(activity.historyState.error)
        controller.resume()
        looper.idle()
        assertEquals(2, activity.synthetic.historyReads.size)
        val empty = "{\"revision\":\"0\",\"conversation\":${selected.conversation},\"head\":null,\"turns\":[],\"older_before\":null}"
        activity.synthetic.historyReads[1].second(empty, selected)
        looper.idle()
        assertFalse(activity.historyState.loading)
        assertNull(activity.historyState.error)
        assertEquals(selected.conversation, activity.historyState.conversation)
        controller.pause().stop().destroy()
    }

    @Test fun anOutstandingHistoryReadCannotMixNewChatOrContinuationCallbacks() {
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val activity = controller.get()
        val looper = Shadows.shadowOf(Looper.getMainLooper())
        val first = activity.synthetic.state.copy(conversation = "{\"replica\":\"synthetic-chat\",\"sequence\":\"1\"}")
        val next = first.copy(conversation = "{\"replica\":\"synthetic-chat\",\"sequence\":\"2\"}")
        activity.synthetic.publish(first)
        looper.idle()
        activity.synthetic.publish(next)
        looper.idle()
        assertEquals(1, activity.synthetic.historyReads.size)
        activity.synthetic.historyReads[0].second(null, first)
        looper.idle()
        assertEquals(2, activity.synthetic.historyReads.size)
        assertEquals(next.conversation, activity.historyState.conversation)
        assertTrue(activity.historyState.turns.isEmpty())
        assertNull(activity.historyState.error)
        controller.pause().stop().destroy()
    }

    @Test fun automaticLinkStatusDoesNotOverlapAndRetiresReadsAcrossPauseResume() {
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val activity = controller.get()
        val looper = Shadows.shadowOf(Looper.getMainLooper())
        looper.idle()
        assertEquals(1, activity.linkReads.size)
        looper.idleFor(Duration.ofSeconds(6))
        assertEquals(1, activity.linkReads.size)
        controller.pause()
        activity.linkReads[0](PeerPairingState(PeerPairingReason.ENROLLED))
        looper.idle()
        assertEquals(PeerPairingReason.WAITING_FOR_PEER, activity.pairing.reason)
        controller.resume()
        looper.idle()
        assertEquals(2, activity.linkReads.size)
        activity.linkReads[1](PeerPairingState(PeerPairingReason.ENROLLED))
        looper.idle()
        assertEquals(PeerPairingReason.ENROLLED, activity.pairing.reason)
        controller.pause().stop().destroy()
        looper.idleFor(Duration.ofSeconds(9))
        assertEquals(2, activity.linkReads.size)
    }

    @Test fun nativeRemoteInputReturnUpdatesDraftExactlyOnceAndNeverAutomaticallySends() {
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val activity = controller.get()
        activity.editDraft()
        activity.receiveText(Activity.RESULT_OK, AssistantTextInput.result("Synthetic edited text"))
        activity.receiveText(Activity.RESULT_OK, AssistantTextInput.result("Synthetic stale text"))
        assertEquals("Synthetic edited text", activity.synthetic.state.draft)
        assertEquals(0, activity.synthetic.sends)
        controller.pause().stop().destroy()
    }
    @Test fun backgroundRetiresRuntimeWhileImeCancellationKeepsDraftRecoverable() {
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val activity = controller.get()
        activity.editDraft()
        controller.pause().stop()
        assertEquals(1, activity.synthetic.backgrounds)
        activity.receiveText(Activity.RESULT_CANCELED, null)
        assertEquals("Synthetic old draft", activity.synthetic.state.draft)
        controller.start().resume()
        assertEquals(2, activity.synthetic.resumes)
        assertEquals(0, activity.synthetic.sends)
        controller.pause().stop().destroy()
    }
    @Test fun pendingImeCorrelationSurvivesRecreationAndBusyStateCannotStartInput() {
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val original = controller.get()
        original.editDraft()
        val saved = Bundle()
        controller.saveInstanceState(saved).pause().stop().destroy()
        val replacement = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).create(saved).start().resume().visible()
        replacement.get().receiveText(Activity.RESULT_OK, AssistantTextInput.result("Synthetic restored input"))
        assertEquals("Synthetic restored input", replacement.get().synthetic.state.draft)
        replacement.get().synthetic.state = replacement.get().synthetic.state.copy(busy = true)
        replacement.get().editDraft()
        replacement.get().receiveText(Activity.RESULT_OK, AssistantTextInput.result("Synthetic rejected input"))
        assertEquals("Synthetic restored input", replacement.get().synthetic.state.draft)
        replacement.pause().stop().destroy()
    }
    @Test fun nativeInputResultContractConfirmsDraftNotSendAndFailureCopyRetainsDraft() {
        val input = AssistantImeSession()
        assertTrue(input.begin())
        assertEquals("Synthetic confirmed draft", input.accept(Activity.RESULT_OK, AssistantTextInput.result("Synthetic confirmed draft")))
        assertNull(input.accept(Activity.RESULT_OK, AssistantTextInput.result("Synthetic duplicate draft")))
        assertTrue(assistantErrorText("auth_expired").contains("Draft retained"))
        assertFalse(assistantErrorText("auth_expired").contains("Import"))
        assertTrue(assistantErrorText("not_saved").contains("Draft retained"))
        assertEquals(4, AssistantModels.aliases.size)
    }

    @Test fun lateMicrophonePermissionAfterBackgroundCannotStartAnyRecognizerOrLoop() {
        Shadows.shadowOf(RuntimeEnvironment.getApplication()).denyPermissions(android.Manifest.permission.RECORD_AUDIO)
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val activity = controller.get()
        activity.requestVoice(true)
        controller.pause().stop()
        activity.receiveMicrophonePermission(true)
        controller.start().resume()
        assertEquals(0, activity.audio.conversations)
        assertEquals(0, activity.audio.dictations)
        assertEquals(0, activity.synthetic.sends)
        controller.pause().stop().destroy()
    }

    @Test fun visiblePermissionGrantOnlyStartsExplicitDictationNotSendAndKeyboardStopsAudio() {
        Shadows.shadowOf(RuntimeEnvironment.getApplication()).denyPermissions(android.Manifest.permission.RECORD_AUDIO)
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val activity = controller.get()
        activity.requestVoice(false)
        activity.receiveMicrophonePermission(true)
        assertEquals(1, activity.audio.dictations)
        assertEquals(0, activity.synthetic.sends)
        activity.editDraft()
        assertEquals(1, activity.audio.stops)
        assertTrue(assistantVoiceErrorText("on_device_recognition_unavailable").contains("no voice-model download"))
        controller.pause().stop().destroy()
    }
    @Test fun nativeDismissalPreservesDraftRetiresVoiceAndRejectsLatePermission() {
        Shadows.shadowOf(RuntimeEnvironment.getApplication()).denyPermissions(android.Manifest.permission.RECORD_AUDIO)
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val activity = controller.get()
        activity.requestVoice(true)
        activity.finish()
        activity.receiveMicrophonePermission(true)
        assertEquals(0, activity.audio.conversations)
        controller.pause().stop().destroy()
        assertEquals("Synthetic old draft", activity.synthetic.state.draft)
        assertEquals(0, activity.synthetic.sends)
        assertEquals(listOf(true, false), activity.audio.foregrounds)
        assertEquals(1, activity.audio.closes)
    }
}
