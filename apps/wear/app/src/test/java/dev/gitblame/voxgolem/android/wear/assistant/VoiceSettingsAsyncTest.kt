package dev.gitblame.voxgolem.android.wear.assistant

import android.os.Looper
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.Shadows
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

class TransientVoiceReadActivity : SyntheticAssistantActivity() {
    companion object { var failRead = true }
    override fun assistantVoiceSettings(): WearVoiceSettings {
        if (failRead) throw IllegalStateException("Synthetic transient read failure")
        return super.assistantVoiceSettings()
    }
}

class RetainedFailedVoiceSettingsActivity : SyntheticAssistantActivity() {
    companion object { internal var retained: WearVoiceSettings? = null }
    override fun assistantVoiceSettings(): WearVoiceSettings = retained ?: run {
        val prefs = getSharedPreferences("synthetic-memory-mutating-failure-${java.util.UUID.randomUUID()}", MODE_PRIVATE)
        check(prefs.edit().putBoolean("read-replies-aloud", false).commit())
        val failing = object : android.content.SharedPreferences by prefs {
            override fun edit(): android.content.SharedPreferences.Editor {
                val actual = prefs.edit()
                return object : android.content.SharedPreferences.Editor by actual {
                    override fun putBoolean(key: String?, value: Boolean): android.content.SharedPreferences.Editor {
                        actual.putBoolean(key, value)
                        return this
                    }
                    override fun putLong(key: String?, value: Long): android.content.SharedPreferences.Editor {
                        actual.putLong(key, value)
                        return this
                    }
                    override fun commit(): Boolean {
                        // Simulate the important platform failure property: memory has changed
                        // even though the durability acknowledgement is false.
                        actual.commit()
                        return false
                    }
                }
            }
        }
        WearVoiceSettings(failing).also { retained = it }
    }
}

class LifecycleVoiceSettingsActivity : SyntheticAssistantActivity() {
    companion object {
        internal var owner: WearVoiceSettings? = null
        var failWrites = false
        var backingName = "synthetic-lifecycle-settings-${java.util.UUID.randomUUID()}"
    }
    override fun assistantVoiceSettings(): WearVoiceSettings = owner ?: run {
        val prefs = getSharedPreferences(backingName, MODE_PRIVATE)
        val backing = object : android.content.SharedPreferences by prefs {
            override fun edit(): android.content.SharedPreferences.Editor {
                val actual = prefs.edit()
                return object : android.content.SharedPreferences.Editor by actual {
                    override fun putBoolean(key: String?, value: Boolean): android.content.SharedPreferences.Editor {
                        actual.putBoolean(key, value)
                        return this
                    }
                    override fun putLong(key: String?, value: Long): android.content.SharedPreferences.Editor {
                        actual.putLong(key, value)
                        return this
                    }
                    override fun commit(): Boolean {
                        // Model a failed durable transaction; memory-mutation failure is
                        // separately covered by RetainedFailedVoiceSettingsActivity.
                        return !failWrites && actual.commit()
                    }
                }
            }
        }
        WearVoiceSettings(backing).also { owner = it }
    }
}

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class VoiceSettingsAsyncTest {
    @Test fun durableModeIntentInvalidatesStaleSavedTalkWithFreshSettingsOwner() {
        processReplacementModeIntent(failWrite = false, newerTalk = null)
    }

    @Test fun failedDurableModeIntentDoesNotInvalidateSavedTalkWithFreshSettingsOwner() {
        processReplacementModeIntent(failWrite = true, newerTalk = null)
    }

    @Test fun newerExplicitEntrySurvivesDurableOlderModeIntentWithFreshSettingsOwner() {
        for (talk in listOf(false, true)) processReplacementModeIntent(failWrite = false, newerTalk = talk)
    }

    private fun resetIntentOrderForProcessReplacement() {
        org.robolectric.util.ReflectionHelpers.getStaticField<java.util.concurrent.atomic.AtomicLong>(
            WearVoiceIntentOrder::class.java, "ordinal").set(0L)
        org.robolectric.util.ReflectionHelpers.setStaticField(WearVoiceIntentOrder::class.java, "seeded", false)
    }

    private fun processReplacementModeIntent(failWrite: Boolean, newerTalk: Boolean?) {
        Shadows.shadowOf(org.robolectric.RuntimeEnvironment.getApplication())
            .grantPermissions(android.Manifest.permission.RECORD_AUDIO)
        LifecycleVoiceSettingsActivity.owner = null
        LifecycleVoiceSettingsActivity.backingName = "synthetic-durable-mode-${java.util.UUID.randomUUID()}"
        LifecycleVoiceSettingsActivity.failWrites = failWrite
        val first = Robolectric.buildActivity(LifecycleVoiceSettingsActivity::class.java).setup()
        val activity = first.get()
        activity.requestVoice(true)
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        WearVoiceSettingsIo.executor.execute {
            entered.countDown(); check(release.await(5, TimeUnit.SECONDS))
        }
        assertTrue(entered.await(5, TimeUnit.SECONDS))
        val saved = android.os.Bundle()
        val desiredMode = newerTalk?.not() ?: false
        try {
            activity.setConversationMode(desiredMode)
            newerTalk?.let(activity::requestVoice)
            first.saveInstanceState(saved).pause().stop().destroy()
        } finally { release.countDown() }
        awaitVoiceSettings()
        val oldOwner = LifecycleVoiceSettingsActivity.owner
        LifecycleVoiceSettingsActivity.owner = null
        LifecycleVoiceSettingsActivity.failWrites = false
        resetIntentOrderForProcessReplacement()
        val replacement = Robolectric.buildActivity(LifecycleVoiceSettingsActivity::class.java)
            .create(saved).start().resume().visible()
        try {
            val next = replacement.get()
            assertNotSame(oldOwner, LifecycleVoiceSettingsActivity.owner)
            assertEquals(if (failWrite) true else desiredMode, next.voicePreferences.conversationMode)
            val expectedTalk = newerTalk ?: failWrite
            assertEquals(if (newerTalk != null || failWrite) {
                if (expectedTalk) AssistantVoiceEntryMode.TALK else AssistantVoiceEntryMode.ASK
            } else null, next.presentedVoiceState().entryMode)
            assertEquals(if (expectedTalk) AssistantPrimaryAction.TALK else AssistantPrimaryAction.ASK,
                primaryAction(next))
        } finally { replacement.pause().stop().destroy(); LifecycleVoiceSettingsActivity.owner = null }
    }

    @Test fun freshLauncherBeforeFirstReadRanksAfterLargerStoredModeOrdinalAndEarlySettingsReject() {
        val context = org.robolectric.RuntimeEnvironment.getApplication()
        Shadows.shadowOf(context).grantPermissions(android.Manifest.permission.RECORD_AUDIO)
        for (talk in listOf(false, true)) {
            LifecycleVoiceSettingsActivity.owner = null
            LifecycleVoiceSettingsActivity.failWrites = false
            LifecycleVoiceSettingsActivity.backingName = "synthetic-fresh-launch-${java.util.UUID.randomUUID()}"
            val stored = 500_000L
            assertTrue(context.getSharedPreferences(LifecycleVoiceSettingsActivity.backingName, android.content.Context.MODE_PRIVATE)
                .edit().putBoolean("conversation-mode", !talk)
                .putLong(WearVoiceSettings.ModeOrdinalKey, stored).commit())
            resetIntentOrderForProcessReplacement()
            val entered = CountDownLatch(1)
            val release = CountDownLatch(1)
            WearVoiceSettingsIo.executor.execute {
                entered.countDown(); check(release.await(5, TimeUnit.SECONDS))
            }
            assertTrue(entered.await(5, TimeUnit.SECONDS))
            val intent = android.content.Intent(android.content.Intent.ACTION_MAIN)
                .addCategory(android.content.Intent.CATEGORY_LAUNCHER)
                .setComponent(android.content.ComponentName(context.packageName,
                    if (talk) AssistantActivity.CONVERSATION_ENTRY else AssistantActivity.VOICE_ENTRY))
            val controller = Robolectric.buildActivity(LifecycleVoiceSettingsActivity::class.java, intent)
            try {
                controller.create().start()
                controller.get().setConversationMode(talk)
                controller.get().setReadRepliesAloud(false)
                assertEquals("settings_not_saved", controller.get().presentedVoiceState().error)
                val beforeRead = android.os.Bundle()
                controller.saveInstanceState(beforeRead)
                assertFalse("An unadmitted provisional rank must not become a restored durable entry",
                    beforeRead.containsKey("assistant.voice.entry.ordinal"))
            } finally { release.countDown() }
            controller.resume().visible()
            try {
                val activity = controller.get()
                assertEquals(WearVoicePreferences(true, !talk), activity.voicePreferences)
                assertEquals(if (talk) AssistantVoiceEntryMode.TALK else AssistantVoiceEntryMode.ASK,
                    activity.presentedVoiceState().entryMode)
                assertEquals(if (talk) AssistantPrimaryAction.TALK else AssistantPrimaryAction.ASK, primaryAction(activity))
                val saved = android.os.Bundle()
                controller.saveInstanceState(saved)
                assertTrue(saved.getLong("assistant.voice.entry.ordinal") > stored)
                assertEquals(stored, LifecycleVoiceSettingsActivity.owner!!.successfulModeOrdinal)
            } finally { controller.pause().stop().destroy(); LifecycleVoiceSettingsActivity.owner = null }
        }
    }

    @Test fun successfulModeIntentInvalidatesSavedTalkAcrossActivityReplacement() {
        recreationModeIntent(failWrite = false, newerExplicitTalk = null)
    }

    @Test fun failedModeIntentCannotInvalidateSavedTalkAcrossActivityReplacement() {
        recreationModeIntent(failWrite = true, newerExplicitTalk = null)
    }

    @Test fun newerExplicitAskOrTalkSurvivesOlderModeCommitAcrossActivityReplacement() {
        for (talk in listOf(false, true)) recreationModeIntent(failWrite = false, newerExplicitTalk = talk)
    }

    private fun recreationModeIntent(failWrite: Boolean, newerExplicitTalk: Boolean?) {
        Shadows.shadowOf(org.robolectric.RuntimeEnvironment.getApplication())
            .grantPermissions(android.Manifest.permission.RECORD_AUDIO)
        LifecycleVoiceSettingsActivity.owner = null
        LifecycleVoiceSettingsActivity.backingName = "synthetic-lifecycle-settings-${java.util.UUID.randomUUID()}"
        LifecycleVoiceSettingsActivity.failWrites = failWrite
        val first = Robolectric.buildActivity(LifecycleVoiceSettingsActivity::class.java).setup()
        val activity = first.get()
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        WearVoiceSettingsIo.executor.execute {
            entered.countDown(); check(release.await(5, TimeUnit.SECONDS))
        }
        assertTrue(entered.await(5, TimeUnit.SECONDS))
        val saved = android.os.Bundle()
        val mode = newerExplicitTalk?.not() ?: false
        val replacement = Robolectric.buildActivity(LifecycleVoiceSettingsActivity::class.java)
        try {
            activity.requestVoice(true)
            assertEquals(AssistantVoiceEntryMode.TALK, activity.presentedVoiceState().entryMode)
            activity.setConversationMode(mode)
            newerExplicitTalk?.let(activity::requestVoice)
            first.saveInstanceState(saved).pause().stop().destroy()
            replacement.create(saved)
        } finally { release.countDown() }
        // The replacement restored its override before the older write completed.
        replacement.start().resume().visible()
        try {
            awaitVoiceSettings()
            val next = replacement.get()
            assertEquals(if (failWrite) true else mode, next.voicePreferences.conversationMode)
            val expectedTalk = newerExplicitTalk ?: failWrite
            assertEquals(if (newerExplicitTalk != null || failWrite) {
                if (expectedTalk) AssistantVoiceEntryMode.TALK else AssistantVoiceEntryMode.ASK
            } else null, next.presentedVoiceState().entryMode)
            assertEquals(if (expectedTalk) AssistantPrimaryAction.TALK else AssistantPrimaryAction.ASK,
                primaryAction(next))
            assertEquals(if (failWrite) "settings_not_saved" else null, next.presentedVoiceState().error)
        } finally {
            replacement.pause().stop().destroy()
            LifecycleVoiceSettingsActivity.owner = null
            LifecycleVoiceSettingsActivity.failWrites = false
        }
    }

    @Test fun successfulModeChangeSurvivesNewerReplySettingAcknowledgement() {
        Shadows.shadowOf(org.robolectric.RuntimeEnvironment.getApplication())
            .grantPermissions(android.Manifest.permission.RECORD_AUDIO)
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val activity = controller.get()
        activity.requestVoice(true)
        assertEquals(AssistantVoiceEntryMode.TALK, activity.presentedVoiceState().entryMode)
        val release = CountDownLatch(1)
        WearVoiceSettingsIo.executor.execute { check(release.await(5, TimeUnit.SECONDS)) }
        try {
            activity.setConversationMode(false)
            activity.setReadRepliesAloud(false)
        } finally { release.countDown() }
        awaitVoiceSettings()
        try {
            assertEquals(WearVoicePreferences(false, false), activity.voicePreferences)
            assertNull(activity.presentedVoiceState().entryMode)
            assertEquals(AssistantPrimaryAction.ASK, primaryAction(activity))
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun successfulModeAcknowledgementCannotClearNewerExplicitAskOrTalk() {
        Shadows.shadowOf(org.robolectric.RuntimeEnvironment.getApplication())
            .grantPermissions(android.Manifest.permission.RECORD_AUDIO)
        for (newerTalk in listOf(false, true)) {
            val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
            val activity = controller.get()
            activity.requestVoice(!newerTalk)
            assertNotNull(activity.presentedVoiceState().entryMode)
            val release = CountDownLatch(1)
            WearVoiceSettingsIo.executor.execute { check(release.await(5, TimeUnit.SECONDS)) }
            try {
                activity.setConversationMode(!newerTalk)
                activity.setReadRepliesAloud(false)
                activity.requestVoice(newerTalk)
            } finally { release.countDown() }
            awaitVoiceSettings()
            try {
                assertEquals(WearVoicePreferences(false, !newerTalk), activity.voicePreferences)
                assertEquals(if (newerTalk) AssistantVoiceEntryMode.TALK else AssistantVoiceEntryMode.ASK,
                    activity.presentedVoiceState().entryMode)
                assertEquals(if (newerTalk) AssistantPrimaryAction.TALK else AssistantPrimaryAction.ASK,
                    primaryAction(activity))
            } finally { controller.pause().stop().destroy() }
        }
    }

    private fun primaryAction(activity: SyntheticAssistantActivity): AssistantPrimaryAction {
        val voice = activity.presentedVoiceState()
        return assistantPrimaryControl(activity.synthetic.snapshot().copy(draft = ""), voice,
            assistantRepeatsAfterReply(voice, activity.voicePreferences)).action
    }

    @Test fun rejectedInitialLoadIsExplicitAndRefreshCanAdmitItAfterCapacityReturns() {
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val drained = CountDownLatch(1)
        WearVoiceSettingsIo.executor.execute {
            entered.countDown(); check(release.await(5, TimeUnit.SECONDS))
        }
        assertTrue(entered.await(5, TimeUnit.SECONDS))
        repeat(WearVoiceSettingsIo.Capacity - 1) { WearVoiceSettingsIo.executor.execute {} }
        WearVoiceSettingsIo.executor.execute { drained.countDown() }
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java)
        try {
            controller.create()
            assertEquals("settings_not_saved", controller.get().presentedVoiceState().error)
        } finally { release.countDown() }
        assertTrue(drained.await(5, TimeUnit.SECONDS))
        controller.start().resume()
        controller.get().recheck()
        awaitVoiceSettings()
        assertNull(controller.get().presentedVoiceState().error)
        assertTrue(controller.get().audio.speechChoices.isNotEmpty())
        controller.pause().stop().destroy()
    }

    @Test fun productionFileHasOneProcessOwnerAndFailedCandidateDoesNotReappearOnRecreation() {
        val context = org.robolectric.RuntimeEnvironment.getApplication()
        assertSame(WearVoiceSettings.production(context), WearVoiceSettings.production(context))
        RetainedFailedVoiceSettingsActivity.retained = null
        val first = Robolectric.buildActivity(RetainedFailedVoiceSettingsActivity::class.java).setup()
        first.get().setReadRepliesAloud(true)
        awaitVoiceSettings()
        assertFalse(first.get().voicePreferences.readRepliesAloud)
        assertEquals("settings_not_saved", first.get().presentedVoiceState().error)
        first.pause().stop().destroy()
        val next = Robolectric.buildActivity(RetainedFailedVoiceSettingsActivity::class.java).setup()
        try {
            assertFalse(next.get().voicePreferences.readRepliesAloud)
            assertEquals("settings_not_saved", next.get().presentedVoiceState().error)
        } finally { next.pause().stop().destroy(); RetainedFailedVoiceSettingsActivity.retained = null }
    }

    @Test fun boundedFloodRejectsWithoutRetiringLastAcceptedAcknowledgement() {
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val activity = controller.get()
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val drained = CountDownLatch(1)
        WearVoiceSettingsIo.executor.execute {
            entered.countDown(); check(release.await(5, TimeUnit.SECONDS))
        }
        assertTrue(entered.await(5, TimeUnit.SECONDS))
        try {
            repeat(WearVoiceSettingsIo.Capacity - 1) { activity.setReadRepliesAloud(false) }
            WearVoiceSettingsIo.executor.execute { drained.countDown() }
            repeat(100) { activity.setReadRepliesAloud(true) }
            assertEquals(WearVoiceSettingsIo.Capacity, WearVoiceSettingsIo.executor.queue.size)
            assertEquals("settings_not_saved", activity.presentedVoiceState().error)
        } finally { release.countDown() }
        assertTrue(drained.await(5, TimeUnit.SECONDS))
        awaitVoiceSettings()
        assertFalse(activity.voicePreferences.readRepliesAloud)
        assertEquals("settings_not_saved", activity.presentedVoiceState().error)
        activity.setReadRepliesAloud(true)
        awaitVoiceSettings()
        assertTrue(activity.voicePreferences.readRepliesAloud)
        assertNull(activity.presentedVoiceState().error)
        controller.pause().stop().destroy()
    }

    @Test fun transientInitialReadFailureCanBeRetriedByExplicitRefresh() {
        TransientVoiceReadActivity.failRead = true
        val controller = Robolectric.buildActivity(TransientVoiceReadActivity::class.java).setup()
        try {
            val activity = controller.get()
            assertEquals("settings_not_saved", activity.presentedVoiceState().error)
            TransientVoiceReadActivity.failRead = false
            activity.recheck()
            awaitVoiceSettings()
            assertNull(activity.presentedVoiceState().error)
            assertTrue(activity.audio.speechChoices.isNotEmpty())
        } finally { TransientVoiceReadActivity.failRead = false; controller.pause().stop().destroy() }
    }

    @Test fun delayedReadDoesNotBlockCreationAndDestroyedOwnerIgnoresItsSettlement() {
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        WearVoiceSettingsIo.executor.execute { entered.countDown(); check(release.await(5, TimeUnit.SECONDS)) }
        assertTrue(entered.await(5, TimeUnit.SECONDS))
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java)
        try {
            controller.create()
            assertTrue(controller.get().audio.speechChoices.isEmpty())
            controller.destroy()
        } finally { release.countDown() }
        awaitVoiceSettings()
        assertTrue(controller.get().audio.speechChoices.isEmpty())
    }

    @Test fun rapidChoicesWaitForDurableLastAcknowledgementRatherThanProjectingEnqueue() {
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val activity = controller.get()
        val original = activity.voicePreferences
        val release = CountDownLatch(1)
        WearVoiceSettingsIo.executor.execute { check(release.await(5, TimeUnit.SECONDS)) }
        try {
            activity.setReadRepliesAloud(false)
            activity.setConversationMode(false)
            activity.setReadRepliesAloud(true)
            Shadows.shadowOf(Looper.getMainLooper()).idle()
            assertEquals(original, activity.voicePreferences)
        } finally { release.countDown() }
        awaitVoiceSettings()
        assertEquals(WearVoicePreferences(true, false), activity.voicePreferences)
        assertNull(activity.presentedVoiceState().error)
        controller.pause().stop().destroy()
    }

    @Test fun failedWriteKeepsAcknowledgedChoiceAndExposesSaveFailure() {
        val controller = Robolectric.buildActivity(FailedInitialVoicePreferencesActivity::class.java).setup()
        val activity = controller.get()
        activity.setReadRepliesAloud(true)
        awaitVoiceSettings()
        assertEquals(WearVoicePreferences(false, true), activity.voicePreferences)
        assertEquals("settings_not_saved", activity.presentedVoiceState().error)
        controller.pause().stop().destroy()
    }

    @Test fun pausedSettlementDoesNotInvokeSpeechAndResumeUsesAcknowledgedChoice() {
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val activity = controller.get()
        val count = activity.audio.speechChoices.size
        activity.setReadRepliesAloud(false)
        controller.pause().stop()
        awaitVoiceSettings()
        assertEquals(count, activity.audio.speechChoices.size)
        assertFalse(activity.voicePreferences.readRepliesAloud)
        controller.start().resume()
        assertEquals(false, activity.audio.speechChoices.last())
        controller.pause().stop().destroy()
    }
}
