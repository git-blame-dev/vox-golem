package dev.gitblame.voxgolem.android.wear.assistant

import android.content.Intent
import android.graphics.Color
import android.graphics.Rect
import android.graphics.RectF
import android.os.Bundle
import android.os.Build
import android.os.SystemClock
import android.view.InputDevice
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.accessibility.AccessibilityNodeInfo
import androidx.lifecycle.Lifecycle
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import dev.gitblame.voxgolem.android.wear.CompactEdgeButtonDefaults
import dev.gitblame.voxgolem.assistant.recognition.DictationPhase
import dev.gitblame.voxgolem.assistant.speech.LoopPhase
import dev.gitblame.voxgolem.assistant.speech.SpeechPhase
import org.junit.Assert.*
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import java.util.concurrent.atomic.AtomicReference
import java.util.concurrent.atomic.AtomicLong

@RunWith(AndroidJUnit4::class)
class AssistantUxEmulatorTest {
    private val instrumentation get() = InstrumentationRegistry.getInstrumentation()
    private val automation get() = instrumentation.uiAutomation
    private val geometry get() = instrumentation.targetContext.resources.displayMetrics

    @Before fun requireEmulator() {
        assertEquals("Normal app identity/storage must not be used", AssistantUxFixtureActivity.FIXTURE_PACKAGE,
            instrumentation.targetContext.packageName)
        assertTrue("Do not launch this synthetic fixture on a physical device",
            Build.HARDWARE == "ranchu" || Build.HARDWARE == "goldfish")
    }

    @Test(timeout = 90_000) fun waitingHasNoAnswerRowAndOnlySubstantiveTextCreatesTheAssistantRow() {
        launch().use { scenario ->
            scenario.onActivity { it.setDraft("Synthetic waiting prompt"); it.withholdNextResponseText() }
            awaitDescription(SEND)
            tap(SEND)
            awaitDescription("Stop microphone, response and playback")
            fun assertNoAnswerPlaceholder() {
                assertTrue(visibleNodes().none { node -> node.text?.let {
                    it == "Assistant" || it.contains("Replying") || it.contains("not saved yet") || it.contains("Waiting for the response")
                } == true })
            }
            assertNoAnswerPlaceholder()
            assertEquals(1, snapshot(scenario).sends)
            assertTrue(snapshot(scenario).state.busy)
            scenario.onActivity { it.publishProvisional(" \n\t") }
            SystemClock.sleep(250)
            assertNoAnswerPlaceholder()
            scenario.onActivity { it.publishProvisional("Synthetic substantive reply") }
            awaitText("Synthetic substantive reply")
            assertFalse("Streaming never creates a durable head", snapshot(scenario).state.selectedHead != null)
            scenario.onActivity { it.completeRequest(answer = "Synthetic settled answer") }
            awaitText("Synthetic settled answer")
            awaitDescription(MIC)
            assertNotNull(snapshot(scenario).state.selectedHead)
            assertEquals("", snapshot(scenario).state.draft)
        }
    }

    @Test(timeout = 90_000) fun keyboardAndChatsLiveInSettingsAndEditorActionReturnsToChatBeforeDismissal() {
        launch().use { scenario ->
            scenario.onActivity { it.setDraft(SETTINGS_DRAFT) }
            awaitDescription(SEND)
            assertPolarControls(SEND)
            openSettingsByPointer()
            scrollTextIntoView(CHATS)
            tap(CHATS, description = false)
            scrollTextIntoView("Synthetic UX chat")
            instrumentation.sendKeyDownUpSync(KeyEvent.KEYCODE_BACK)
            awaitChat(SEND)
            openSettingsByPointer()
            scrollTextIntoView(KEYBOARD)
            tap(KEYBOARD, description = false)
            awaitChat(SEND)
            val value = snapshot(scenario)
            assertEquals("The fixture records the editor launch boundary", 1, value.edits)
            assertEquals(SETTINGS_DRAFT, value.state.draft)
            assertEquals(0, value.sends)
            assertPolarControls(SEND)
        }
    }

    @Test(timeout = 90_000) fun newChatLivesInSettingsKeepsItsActionAndIsDisabledDuringListeningAndGeneration() {
        launch().use { scenario ->
            scenario.onActivity { it.seedHistory(); it.setDraft(SETTINGS_DRAFT) }
            awaitDescription(SEND)
            assertPolarControls(SEND)
            val primary = controlBounds(SEND)
            val viewport = visibleNodes().first { it.scrollable }.bounds
            assertEquals("Scaffold padding belongs to list content, not a top-32dp clipped viewport", 0, viewport.top)
            assertDockClearance(primary)
            val prompt = visibleNodes().first { it.text == SETTINGS_DRAFT }.bounds
            assertTrue("Native wrapping can use the wider framework margins", prompt.width() >= geometry.widthPixels * .89f)
            assertTrue("The final prompt is accessible above the edge action", prompt.bottom <= primary.top + 2)

            openSettingsByPointer()
            scrollTextIntoView(NEW)
            val newBounds = controlBounds(NEW, description = false)
            assertTrue("Settings New keeps at least a 48dp target", newBounds.height() / geometry.density >= 47f)
            tap(NEW, description = false)
            awaitChat(MIC)
            assertEquals("The relocated action still clears the current draft", "", snapshot(scenario).state.draft)
            assertNull("The relocated action still starts a fresh head", snapshot(scenario).state.selectedHead)
            assertEquals(0, snapshot(scenario).sends)

            tap(MIC)
            scenario.onActivity { it.publishListening() }
            awaitDescription(STOP)
            openSettingsByPointer()
            scrollTextIntoView(NEW)
            assertFalse("Listening prevents New exactly as the old top control did", visibleNodes().first { it.text == NEW }.enabled)
            instrumentation.sendKeyDownUpSync(KeyEvent.KEYCODE_BACK)
            awaitChat(STOP)
            scenario.onActivity { it.publishFinal() }
            awaitDescription("Stop microphone, response and playback")
            openSettingsByPointer()
            scrollTextIntoView(NEW)
            assertFalse("Generation prevents New exactly as the old top control did", visibleNodes().first { it.text == NEW }.enabled)
            instrumentation.sendKeyDownUpSync(KeyEvent.KEYCODE_BACK)
            awaitChat("Stop microphone, response and playback")
            tap("Stop microphone, response and playback")
            awaitDescription(SEND)
            assertEquals(1, snapshot(scenario).stops)
            assertEquals(1, snapshot(scenario).sends)
        }
    }

    @Test(timeout = 90_000) fun webPermissionAndSpeechFallbackTogglesAreSeparateFromSearchEffortAndVoiceChoices() {
        launch().use { scenario ->
            scenario.onActivity { it.seedSpeechChoices() }
            val before = snapshot(scenario)
            assertTrue(before.state.webEnabled)
            assertTrue(before.state.speechNetworkFallback)
            openSettingsByPointer()
            scrollTextIntoView("Web access")
            tap("Web access", description = false)
            await("Web permission changes native-projected state") { !snapshot(scenario).state.webEnabled }
            scrollTextIntoView("Search: Quick")
            assertFalse("Effort is disabled, not relocated to chat", visibleNodes().first { it.text == "Search: Quick" }.enabled)
            scrollTextIntoView("Speech network fallback")
            tap("Speech network fallback", description = false)
            await("Speech policy changes separately") { !snapshot(scenario).state.speechNetworkFallback }
            val after = snapshot(scenario)
            assertEquals(before.readRepliesAloud, after.readRepliesAloud)
            assertEquals(before.conversationMode, after.conversationMode)
            assertEquals(0, after.dictates)
            assertEquals(0, after.sends)
            instrumentation.sendKeyDownUpSync(KeyEvent.KEYCODE_BACK)
            awaitChat(MIC)
            assertTrue(visibleNodes().none { it.text?.startsWith("Search:") == true })
        }
    }

    @Test(timeout = 180_000) fun reportedSpeechSelectorsUseSavedRequestsAndEngineChangeExplicitlyClearsTheVoice() {
        launch().use { scenario ->
            scenario.onActivity { it.seedSpeechChoices() }
            openSettingsByPointer()
            scrollTextIntoView("Speech language: Automatic")
            tap("Speech language: Automatic", description = false)
            scrollTextIntoView("en-AU")
            tap("en-AU", description = false)
            await("Synthetic native snapshot acknowledges the language request") { snapshot(scenario).state.speechLanguage == "en-AU" }
            instrumentation.sendKeyDownUpSync(KeyEvent.KEYCODE_BACK)
            awaitText("Settings")
            scrollTextIntoView("Reply voice engine: Automatic")
            tap("Reply voice engine: Automatic", description = false)
            scrollTextContainingIntoView("Choosing a different engine clears the old voice to Automatic")
            scrollTextIntoView("synthetic.engine.one")
            tap("synthetic.engine.one", description = false)
            await("Synthetic native snapshot acknowledges the engine request") { snapshot(scenario).state.speechTtsProvider == "synthetic.engine.one" }
            instrumentation.sendKeyDownUpSync(KeyEvent.KEYCODE_BACK)
            awaitText("Settings")
            scrollTextIntoView("Speech voice: Automatic")
            tap("Speech voice: Automatic", description = false)
            val localVoice = "synthetic-local-voice · en-AU · network not required"
            scrollTextIntoView(localVoice)
            tap(localVoice, description = false)
            await("Synthetic native snapshot acknowledges the voice request") { snapshot(scenario).state.speechTtsVoice == "synthetic-local-voice" }
            instrumentation.sendKeyDownUpSync(KeyEvent.KEYCODE_BACK)
            awaitText("Settings")
            scrollTextIntoView("Reply voice engine: synthetic.engine.one")
            tap("Reply voice engine: synthetic.engine.one", description = false)
            scrollTextContainingIntoView("Choosing a different engine clears the old voice to Automatic")
            scrollTextIntoView("synthetic.engine.two")
            tap("synthetic.engine.two", description = false)
            await("Engine change clears rather than silently substitutes a voice") {
                val state = snapshot(scenario).state
                state.speechTtsProvider == "synthetic.engine.two" && state.speechTtsVoice == "" && state.speechLanguage == "en-AU"
            }
            assertEquals(0, snapshot(scenario).dictates)
            assertEquals(0, snapshot(scenario).sends)
            instrumentation.sendKeyDownUpSync(KeyEvent.KEYCODE_BACK)
            awaitText("Settings")
            instrumentation.sendKeyDownUpSync(KeyEvent.KEYCODE_BACK)
            awaitChat(MIC)
            assertPolarControls(MIC)
        }
    }

    @Test(timeout = 90_000) fun askFinalAutoSendsExactlyOnceInCurrentContextAndIdlesAfterReply() {
        launch().use { scenario ->
            scenario.onActivity { it.seedHistory(count = 2) }
            awaitDescription(MIC)
            assertPolarControls(MIC)
            val original = snapshot(scenario)
            val micBounds = controlBounds(MIC)
            val micGlyph = glyphPixels(micBounds)
            tap(MIC)
            awaitDescription(STOP)
            assertEquals(DictationPhase.CHECKING, snapshot(scenario).dictationPhase)
            assertEquals("Cancel retains the primary target", micBounds, controlBounds(STOP))
            scenario.onActivity { it.publishListening(); it.publishPartial("Synthetic partial question") }
            assertStopGlyph()
            assertTrue("Listening replaces the microphone drawing with square Stop", changedFraction(micGlyph, glyphPixels(micBounds)) > .05f)
            val listening = snapshot(scenario)
            assertEquals(1, listening.dictates)
            assertEquals(0, listening.conversations)
            assertEquals(DictationPhase.LISTENING, listening.dictationPhase)
            assertEquals(0, listening.sends)
            assertPolarControls(STOP)
            assertNoStreamSpeechControls()
            scenario.onActivity { it.publishFinishing() }
            assertStopGlyph()
            val finishing = snapshot(scenario)
            assertEquals("The primary UI never requests legacy Finish", 0, finishing.finishes)
            assertEquals(1, finishing.dictates)
            assertEquals(0, finishing.stops)
            assertEquals(0, finishing.sends)
            scenario.onActivity { it.publishFinal(); it.publishFinal() }
            awaitDescription(STOP)
            awaitText(AssistantUxFixtureActivity.FINAL_DRAFT)
            val final = snapshot(scenario)
            assertEquals(AssistantUxFixtureActivity.FINAL_DRAFT, final.state.draft)
            assertTrue("Ask auto-submits the authoritative final", final.state.busy)
            assertEquals(1, final.sends)
            assertEquals(original.state.conversation, final.state.conversation)
            assertEquals(original.state.selectedHead, final.state.selectedHead)
            assertEquals(original.savedChoices, final.savedChoices)
            awaitText("Synthetic streaming reply")
            assertStopGlyph()
            assertNoStreamSpeechControls()
            val previousHead = snapshot(scenario).state.selectedHead
            scenario.onActivity { it.completeRequest() }
            awaitText(AssistantUxFixtureActivity.SAVED_ANSWER)
            awaitDescription(STOP)
            assertEquals(SpeechPhase.SPEAKING, snapshot(scenario).speechPhase)
            assertEquals(1, snapshot(scenario).micStarts)
            assertStopGlyph()
            scenario.onActivity { it.completePlayback(); it.beginEmptyFollowup() }
            awaitDescription(MIC)
            val complete = snapshot(scenario)
            assertFalse(complete.state.busy)
            assertNotEquals(previousHead, complete.state.selectedHead)
            assertEquals("", complete.state.draft)
            assertEquals("Enabled reply speech does not reintroduce an inline Read aloud action", 0, complete.speaks)
            assertEquals(1, complete.sends)
            assertEquals(1, complete.micStarts)
            assertEquals(LoopPhase.IDLE, complete.loopPhase)
            assertNoStreamSpeechControls()
            assertPolarControls(MIC)
        }
    }

    @Test(timeout = 90_000) fun everyDictationPhaseKeepsOneYouPromptIncludingDelayedCanonicalHistory() {
        launch().use { scenario ->
            tap(MIC)
            scenario.onActivity { it.publishListening(); it.publishPartial("Synthetic words") }
            assertOneYouPrompt("Synthetic words")
            scenario.onActivity { it.publishPartial("Synthetic words grow") }
            assertOneYouPrompt("Synthetic words grow")
            scenario.onActivity { it.publishFinishing() }
            assertOneYouPrompt("Synthetic words grow")
            scenario.onActivity { it.publishFinal("Synthetic final") }
            awaitDescription(STOP)
            assertEquals(1, snapshot(scenario).sends)
            assertOneYouPrompt("Synthetic final")
            awaitText("Synthetic streaming reply")
            assertOneYouPrompt("Synthetic final")
            scenario.onActivity { it.completeRequest(answer = "Synthetic answer", deferHistory = true) }
            scenario.onActivity { it.completePlayback() }
            awaitDescription(MIC)
            assertOneYouPrompt("Synthetic final")
            assertEquals("", snapshot(scenario).state.draft)
            scenario.onActivity { it.publishCompletedHistory() }
            awaitText("Synthetic answer")
            assertOneYouPrompt("Synthetic final")
            tap(MIC)
            scenario.onActivity { it.publishListening(); it.publishPartial("Synthetic next") }
            awaitText("Synthetic next")
            assertEquals(1, snapshot(scenario).sends)
            assertNoSpeechRoutes()
        }
    }

    @Test(timeout = 120_000) fun speechRoutesAppearOnlyInSettingsWithSelectionAndPreferenceEvidence() {
        launch().use { scenario ->
            awaitDescription(MIC)
            assertNoSpeechRoutes()
            openSettingsByPointer()
            scrollTextIntoView("Dictation · not selected")
            awaitText("Replies · not selected")
            instrumentation.sendKeyDownUpSync(KeyEvent.KEYCODE_BACK)
            awaitChat(MIC)
            scenario.onActivity { it.seedSpeechRoutes() }
            assertNoSpeechRoutes()
            openSettingsByPointer()
            scrollTextIntoView("Dictation · last selection")
            scrollTextContainingIntoView("Dictation: Google · requested en-GB")
            val dictation = visibleNodes().first { it.text?.startsWith("Dictation:") == true }.text!!
            assertTrue(dictation.contains("selected en-US"))
            assertTrue(dictation.contains("reported en-US"))
            assertTrue(dictation.contains("actual local/network processing: not reported"))
            scrollTextContainingIntoView("Replies: Google preference · requested en-GB")
            assertTrue(visibleNodes().any { it.text?.contains("installed local voice") == true })
            instrumentation.sendKeyDownUpSync(KeyEvent.KEYCODE_BACK)
            awaitChat(MIC)
            assertNoSpeechRoutes()
            tap(MIC)
            awaitDescription(STOP)
            assertNoSpeechRoutes()
            openSettingsByPointer()
            scrollTextIntoView("Dictation · active selection")
            instrumentation.sendKeyDownUpSync(KeyEvent.KEYCODE_BACK)
            awaitChat(STOP)
            tap(STOP)
            awaitDescription(MIC)
            assertNoSpeechRoutes()
        }
    }

    @Test(timeout = 90_000) fun talkAutoSubmitsFinalAndNextMicWaitsForDurableReplyAndPlaybackCompletion() {
        launch(conversationChoice = null).use { scenario ->
            awaitDescription(CONVERSATION_MIC)
            tap(CONVERSATION_MIC)
            val firstInput = AtomicLong()
            scenario.onActivity { firstInput.set(it.inputOwner()) }
            scenario.onActivity { it.publishListening(); it.publishPartial("Synthetic words"); it.beginEmptyFollowup() }
            assertOneYouPrompt("Synthetic words")
            assertEquals(1, snapshot(scenario).conversations)
            assertEquals(0, snapshot(scenario).dictates)
            assertEquals(0, snapshot(scenario).sends)
            scenario.onActivity { it.publishFinishing() }
            assertOneYouPrompt("Synthetic words")
            assertEquals(0, snapshot(scenario).sends)
            scenario.onActivity { it.publishFinal("Synthetic final") }
            awaitText("Synthetic streaming reply")
            scenario.onActivity { it.beginEmptyFollowup() }
            assertEquals("No restart while generating", 1, snapshot(scenario).micStarts)
            assertEquals(1, snapshot(scenario).sends)
            assertOneYouPrompt("Synthetic final")
            scenario.onActivity { it.completeRequest(answer = "Synthetic answer", deferHistory = true) }
            awaitDescription(STOP)
            scenario.onActivity { it.beginEmptyFollowup() }
            assertEquals("No restart while playback is active", 1, snapshot(scenario).micStarts)
            assertOneYouPrompt("Synthetic final")
            scenario.onActivity { it.publishCompletedHistory() }
            awaitText("Synthetic answer")
            assertOneYouPrompt("Synthetic final")
            assertEquals(1, snapshot(scenario).sends)
            scenario.onActivity { it.completePlayback() }
            assertEquals(LoopPhase.BETWEEN_TURNS, snapshot(scenario).loopPhase)
            assertPolarControls(STOP)
            scenario.onActivity { it.beginEmptyFollowup() }
            awaitDescription(STOP)
            assertEquals(2, snapshot(scenario).micStarts)
            assertEquals(DictationPhase.CHECKING, snapshot(scenario).dictationPhase)
            scenario.onActivity {
                it.publishListening(firstInput.get())
                it.publishPartial("Synthetic stale first partial", firstInput.get())
                it.publishFinal("Synthetic stale first turn", firstInput.get())
            }
            assertEquals(1, snapshot(scenario).sends)
            assertEquals(DictationPhase.CHECKING, snapshot(scenario).dictationPhase)
        }
    }

    @Test(timeout = 120_000) fun disabledReplySpeechSettlesAskAndOnlyTalkSchedulesAnotherMic() {
        for (repeat in listOf(false, true)) {
            launch(conversationChoice = repeat).use { scenario ->
                openSettingsByPointer()
                scrollTextIntoView("Speak replies")
                tap("Speak replies", description = false)
                assertFalse(snapshot(scenario).readRepliesAloud)
                instrumentation.sendKeyDownUpSync(KeyEvent.KEYCODE_BACK)
                val entry = if (repeat) CONVERSATION_MIC else MIC
                awaitChat(entry)
                tap(entry)
                scenario.onActivity { it.publishListening(); it.publishFinal("Synthetic silent reply question"); it.completeRequest() }
                assertEquals(SpeechPhase.IDLE, snapshot(scenario).speechPhase)
                assertEquals(1, snapshot(scenario).micStarts)
                scenario.onActivity { it.completePlayback(); it.beginEmptyFollowup() }
                if (repeat) {
                    awaitDescription(STOP)
                    assertEquals(2, snapshot(scenario).micStarts)
                    assertEquals(DictationPhase.CHECKING, snapshot(scenario).dictationPhase)
                } else {
                    awaitDescription(MIC)
                    assertEquals(1, snapshot(scenario).micStarts)
                    assertEquals(LoopPhase.IDLE, snapshot(scenario).loopPhase)
                }
                assertEquals(1, snapshot(scenario).sends)
            }
        }
    }

    @Test(timeout = 90_000) fun enabledDefaultsAreSavedAndExplicitDisabledChoicesSurviveRecreation() {
        launch(conversationChoice = null).use { scenario ->
            awaitDescription(CONVERSATION_MIC)
            var value = snapshot(scenario)
            assertTrue(value.readRepliesAloud)
            assertTrue(value.conversationMode)
            assertEquals(true, value.savedChoices["read-replies-aloud"])
            assertEquals(true, value.savedChoices["conversation-mode"])
            assertEquals(0, value.dictates)
            assertEquals(0, value.conversations)
            scenario.recreate()
            awaitDescription(CONVERSATION_MIC)
            openSettingsByPointer()
            scrollTextIntoView("Speak replies")
            tap("Speak replies", description = false)
            scrollTextIntoView("Talk by default")
            tap("Talk by default", description = false)
            value = snapshot(scenario)
            assertFalse(value.readRepliesAloud)
            assertFalse(value.conversationMode)
            assertEquals(false, value.savedChoices["read-replies-aloud"])
            assertEquals(false, value.savedChoices["conversation-mode"])
            scenario.recreate()
            awaitText("Settings")
            assertFalse(snapshot(scenario).readRepliesAloud)
            assertFalse(snapshot(scenario).conversationMode)
            instrumentation.sendKeyDownUpSync(KeyEvent.KEYCODE_BACK)
            awaitChat(MIC)
            assertPolarControls(MIC)
        }
    }

    @Test(timeout = 90_000) fun transientAskAndTalkOverrideIdleLabelWithoutChangingGlobalDefaultsOrCurrentContext() {
        for ((choice, entry, label) in listOf(
            Triple(true, AssistantVoiceEntryMode.ASK, MIC), Triple(false, AssistantVoiceEntryMode.TALK, CONVERSATION_MIC))) {
            launch(conversationChoice = choice).use { scenario ->
                scenario.onActivity { it.seedHistory(); it.selectTransientEntry(entry) }
                awaitDescription(label)
                val before = snapshot(scenario)
                scenario.onActivity { it.setDraft(SETTINGS_DRAFT); it.beginVoiceEntry(entry) }
                awaitDescription(STOP)
                val checking = snapshot(scenario)
                assertEquals(SETTINGS_DRAFT, checking.state.draft)
                assertEquals(before.state.conversation, checking.state.conversation)
                assertEquals(before.state.selectedHead, checking.state.selectedHead)
                tap(STOP)
                awaitDescription(SEND)
                val canceled = snapshot(scenario)
                assertEquals(SETTINGS_DRAFT, canceled.state.draft)
                assertEquals(before.savedChoices, canceled.savedChoices)
                assertEquals(choice, canceled.conversationMode)
                assertEquals(0, canceled.sends)
                scenario.onActivity { it.setDraft("") }
                awaitDescription(label)
                scenario.recreate()
                awaitDescription(label)
                assertEquals(before.savedChoices, snapshot(scenario).savedChoices)
            }
        }
    }

    @Test(timeout = 120_000) fun launchConfirmationKeepsRetainedDraftAndCurrentContextUntilExplicitAskOrTalkTap() {
        for ((defaultTalk, entry, label) in listOf(
            Triple(true, AssistantVoiceEntryMode.ASK, MIC), Triple(false, AssistantVoiceEntryMode.TALK, CONVERSATION_MIC))) {
            launch(conversationChoice = defaultTalk).use { scenario ->
                scenario.onActivity { it.seedHistory(count = 2); it.setDraft(SETTINGS_DRAFT) }
                awaitDescription(SEND)
                val before = snapshot(scenario)
                scenario.onActivity { it.requireLaunchConfirmation(entry) }
                awaitText("Tap ${if (entry == AssistantVoiceEntryMode.ASK) "Ask" else "Talk"} to start voice.")
                awaitDescription(label)
                assertPolarControls(label)
                swipe(.5f, .25f, .5f, .66f)
                awaitDescription(label)
                assertTrue("Neither retained-draft Send nor Latest can hide voice consent", visibleNodes().none {
                    it.description == SEND || it.description == LATEST
                })
                val waiting = snapshot(scenario)
                assertTrue(waiting.launchConfirmationRequired)
                assertEquals(before.state, waiting.state)
                assertEquals(before.savedChoices, waiting.savedChoices)
                assertEquals(0, waiting.micStarts)
                assertEquals(0, waiting.sends)
                assertEquals(0, waiting.dictates)
                assertEquals(0, waiting.conversations)

                tap(label)
                awaitDescription(STOP)
                val started = snapshot(scenario)
                assertFalse(started.launchConfirmationRequired)
                assertEquals(DictationPhase.CHECKING, started.dictationPhase)
                assertEquals(SETTINGS_DRAFT, started.state.draft)
                assertEquals(before.state.conversation, started.state.conversation)
                assertEquals(before.state.selectedHead, started.state.selectedHead)
                assertEquals(before.savedChoices, started.savedChoices)
                assertEquals(1, started.micStarts)
                assertEquals(if (entry == AssistantVoiceEntryMode.ASK) 1 else 0, started.dictates)
                assertEquals(if (entry == AssistantVoiceEntryMode.TALK) 1 else 0, started.conversations)
                assertEquals("Explicit voice start does not submit or clear the retained draft", 0, started.sends)
            }
        }
    }

    @Test(timeout = 180_000) fun confirmationSuffixPreservesOlderPagingAndPromptAnswerRevalidationAnchors() {
        for (answer in listOf(false, true)) {
            launch(conversationChoice = answer).use { scenario ->
                val entry = if (answer) AssistantVoiceEntryMode.ASK else AssistantVoiceEntryMode.TALK
                val label = if (answer) MIC else CONVERSATION_MIC
                scenario.onActivity { it.seedReadingAnchorHistory(); it.requireLaunchConfirmation(entry) }
                awaitDescription(label)
                awaitText("Tap ${if (answer) "Ask" else "Talk"} to start voice.")
                val message = if (answer) AssistantUxFixtureActivity.ANCHOR_ANSWER else AssistantUxFixtureActivity.ANCHOR_PROMPT
                fun visibleAnchor(): CharacterBox? {
                    if (visibleNodes().none { it.text == message }) return null
                    val viewport = RectF(readingViewport())
                    return answerCharacterBoxes(message).second.firstOrNull { it.index > 0 && viewport.contains(it.bounds) }
                }
                fun scrollToInteriorAnchor(): CharacterBox {
                    // Leave follow-latest explicitly, including when the answer already fills the final viewport.
                    swipe(.5f, .25f, .5f, .66f)
                    repeat(50) {
                        val anchor = visibleAnchor()
                        if (anchor != null && answerCharacterBoxes(message).second.none {
                            it.index == 0 && RectF(readingViewport()).contains(it.bounds)
                        }) return anchor
                        swipe(.5f, .25f, .5f, .66f)
                    }
                    error("The synthetic ${if (answer) "answer" else "prompt"} must expose an interior reading anchor")
                }
                val original = scrollToInteriorAnchor()
                val viewportTop = readingViewport().top
                scenario.onActivity { it.beginHistoryRevalidation() }
                awaitText("Loading conversation…")
                scenario.onActivity { it.completeHistoryRevalidation() }
                await("Revalidation restores the same message character and viewport offset with confirmation present") {
                    visibleNodes().any { it.text == message } && answerCharacterBoxes(message).second.any {
                        it.index == original.index && kotlin.math.abs((it.bounds.top - readingViewport().top) -
                            (original.bounds.top - viewportTop)) <= 2f
                    }
                }
                assertTrue(snapshot(scenario).launchConfirmationRequired)
                awaitDescription(label)
                assertTrue(visibleNodes().none { it.description == LATEST })

                if (!answer) {
                    scrollTextContainingIntoView("Older messages", exact = true, backwards = true)
                    repeat(3) {
                        if (visibleNodes().none { it.text == message } || answerCharacterBoxes(message).second.none {
                            RectF(readingViewport()).contains(it.bounds)
                        }) swipe(.5f, .25f, .5f, .66f)
                    }
                    val beforePaging = answerCharacterBoxes(message).second.first {
                        RectF(readingViewport()).contains(it.bounds)
                    }
                    val beforePagingTop = readingViewport().top
                    tap("Older messages", description = false)
                    await("Actual renderer Older action requests the synthetic page") { snapshot(scenario).olderRequests == 1 }
                    scenario.onActivity { it.completeOlderHistory() }
                    await("Prepending older history preserves the existing prompt and its offset, not the prefixed-row index") {
                        visibleNodes().any { it.text == message } && answerCharacterBoxes(message).second.any {
                            it.index == beforePaging.index && kotlin.math.abs((it.bounds.top - readingViewport().top) -
                                (beforePaging.bounds.top - beforePagingTop)) <= 2f
                        }
                    }
                    assertTrue(snapshot(scenario).launchConfirmationRequired)
                }
                assertEquals(0, snapshot(scenario).micStarts)
                assertEquals(0, snapshot(scenario).sends)
            }
        }
    }

    @Test(timeout = 180_000) fun squareStopCancelsEveryVoiceStageAndRetiredCallbacksCannotSendOrReopen() {
        for (stage in listOf("checking", "listening", "finishing", "generating", "playback", "between")) {
            launch(conversationChoice = true).use { scenario ->
                tap(CONVERSATION_MIC)
                val input = AtomicLong()
                scenario.onActivity {
                    input.set(it.inputOwner())
                    if (stage != "checking") { it.publishListening(); it.publishPartial("Synthetic retained input") }
                    if (stage == "finishing") it.publishFinishing()
                    if (stage in listOf("generating", "playback", "between")) it.publishFinal("Synthetic retained input")
                    if (stage in listOf("playback", "between")) it.completeRequest()
                    if (stage == "between") it.completePlayback()
                }
                awaitDescription(STOP)
                assertPolarControls(STOP)
                assertStopGlyph()
                val before = snapshot(scenario)
                val request = before.sends
                tap(STOP)
                await("Stop retires all synthetic owners at $stage") { snapshot(scenario).loopPhase == LoopPhase.IDLE }
                scenario.onActivity {
                    it.publishListening(input.get())
                    it.publishPartial("Synthetic stale partial", input.get())
                    it.publishFinishing(input.get())
                    it.publishFinal("Synthetic stale final", input.get())
                    it.completeRequest("Synthetic stale reply", request = request)
                    it.completePlayback(request)
                    it.beginEmptyFollowup()
                }
                val stopped = snapshot(scenario)
                assertEquals("No late send at $stage", before.sends, stopped.sends)
                assertEquals("No next microphone at $stage", before.micStarts, stopped.micStarts)
                assertEquals(1, stopped.stops)
                assertEquals(0, stopped.finishes)
                assertEquals("Late partials do not replace recoverable input at $stage", before.state.draft, stopped.state.draft)
                assertFalse(stopped.state.busy)
                assertEquals(DictationPhase.IDLE, stopped.dictationPhase)
                assertEquals(SpeechPhase.IDLE, stopped.speechPhase)
                assertEquals(before.state.selectedHead, stopped.state.selectedHead)
                if (stage in listOf("listening", "finishing", "generating")) {
                    assertEquals("Recoverable input survives cancellation", "Synthetic retained input", stopped.state.draft)
                }
            }
        }
    }

    @Test(timeout = 90_000) fun backgroundCancelsTalkAndResumeOrRecreationDoesNotReplayLateOwners() {
        launch(conversationChoice = true).use { scenario ->
            tap(CONVERSATION_MIC)
            val input = AtomicLong()
            val activity = AtomicReference<AssistantUxFixtureActivity>()
            scenario.onActivity {
                input.set(it.inputOwner()); activity.set(it)
                it.publishListening(); it.publishFinal("Synthetic lifecycle input")
            }
            awaitDescription(STOP)
            val before = snapshot(scenario)
            scenario.moveToState(Lifecycle.State.CREATED)
            instrumentation.runOnMainSync {
                activity.get().publishFinal("Synthetic stale final", input.get())
                activity.get().completeRequest(request = before.sends)
                activity.get().completePlayback(before.sends)
                activity.get().beginEmptyFollowup()
            }
            scenario.moveToState(Lifecycle.State.RESUMED)
            awaitDescription(SEND)
            scenario.recreate()
            awaitDescription(SEND)
            val resumed = snapshot(scenario)
            assertEquals(before.sends, resumed.sends)
            assertEquals(before.micStarts, resumed.micStarts)
            assertEquals(before.savedChoices, resumed.savedChoices)
            assertEquals("Synthetic lifecycle input", resumed.state.draft)
            assertEquals(LoopPhase.IDLE, resumed.loopPhase)
        }
    }

    @Test(timeout = 90_000) fun nativeManualDictationRemainsDraftOnlyButIsNotTheAskPrimaryAction() {
        launch().use { scenario ->
            scenario.onActivity { it.beginManualDictation(); it.publishListening(); it.publishPartial("Synthetic manual partial") }
            awaitDescription(STOP)
            scenario.onActivity { it.publishFinishing(); it.publishFinal("Synthetic manual draft") }
            awaitDescription(SEND)
            assertOneYouPrompt("Synthetic manual draft")
            val manual = snapshot(scenario)
            assertEquals(0, manual.sends)
            assertEquals(0, manual.dictates)
            assertEquals(0, manual.conversations)
            assertFalse(manual.state.busy)
            assertEquals("Synthetic manual draft", manual.state.draft)
            tap(SEND)
            assertEquals(1, snapshot(scenario).sends)
        }
    }

    @Test(timeout = 90_000) fun unavailableRecognitionDisablesOneSemanticAskTargetAndUsesDisabledArtwork() {
        launch().use { scenario ->
            awaitDescription(MIC)
            val bounds = controlBounds(MIC)
            val enabled = glyphPixels(bounds)
            scenario.onActivity { it.setRecognitionAvailable(false) }
            await("Ask reports disabled instead of advertising usable recognition") {
                visibleNodes().singleOrNull { it.description == MIC }?.enabled == false
            }
            assertTrue("Disabled glyph uses visibly different native colors", changedFraction(enabled, glyphPixels(bounds)) > .02f)
            injectTap(bounds.exactCenterX(), bounds.exactCenterY())
            assertEquals(0, snapshot(scenario).dictates)
            assertEquals(0, snapshot(scenario).sends)
            scenario.onActivity { it.setRecognitionAvailable(true) }
            awaitDescription(MIC)
            assertPolarControls(MIC)
        }
    }

    @Test(timeout = 90_000) fun strictFallbackExplainsDedicatedApiAvailabilityNotForegroundAsOfflineProof() {
        launch().use { scenario ->
            openSettingsByPointer()
            scrollTextIntoView("Speech network fallback")
            awaitText("Dedicated on-device API unavailable or not reported. Ordinary recognition may still work offline; strict fallback-off is unavailable.")
            assertFalse(visibleNodes().first { it.text == "Speech network fallback" }.enabled)
            scenario.onActivity { it.setDedicatedRecognitionAvailable(true) }
            await("Reported dedicated support enables the fallback control") {
                visibleNodes().firstOrNull { it.text == "Speech network fallback" }?.enabled == true
            }
            tap("Speech network fallback", description = false)
            awaitText("Dedicated on-device recognition required; network speech disabled")
            assertEquals(0, snapshot(scenario).dictates)
            assertEquals(0, snapshot(scenario).sends)
        }
    }

    @Test(timeout = 120_000) fun delayedFirstChatHistoryKeepsSubmittedYouThroughQuietAndPartialFollowups() {
        launch(conversationChoice = null).use { scenario ->
            scenario.onActivity { it.prepareFreshConversation() }
            awaitDescription(CONVERSATION_MIC)
            assertNull(snapshot(scenario).state.conversation)
            tap(CONVERSATION_MIC)
            scenario.onActivity { it.publishListening(); it.publishPartial("Synthetic A") }
            assertOneYouPrompt("Synthetic A")
            scenario.onActivity { it.publishFinal("Synthetic A") }
            awaitText("Synthetic streaming reply")
            assertNotNull(snapshot(scenario).state.conversation)
            assertOneYouPrompt("Synthetic A")
            scenario.onActivity { it.completeRequest(answer = "Synthetic answer", deferHistory = true) }
            scenario.onActivity { it.completePlayback() }
            awaitDescription(STOP)
            assertOneYouPrompt("Synthetic A")
            scenario.onActivity { it.beginEmptyFollowup() }
            awaitDescription(STOP)
            assertOneYouPrompt("Synthetic A")
            scenario.onActivity { it.publishListening() }
            assertOneYouPrompt("Synthetic A")
            scenario.onActivity { it.publishQuietNoMatchRetirement() }
            awaitDescription(CONVERSATION_MIC)
            assertOneYouPrompt("Synthetic A")
            assertEquals(1, snapshot(scenario).sends)
            tap(CONVERSATION_MIC)
            scenario.onActivity { it.publishListening(); it.publishPartial("Synthetic B") }
            assertTwoYouPrompts("Synthetic A", "Synthetic B")
            scenario.onActivity { it.publishCompletedHistory() }
            awaitText("Synthetic answer")
            assertTwoYouPrompts("Synthetic A", "Synthetic B")
            assertEquals("The new partial follow-up remains unsent", 1, snapshot(scenario).sends)
            assertEquals("The owned partial is projected to native draft state without sending", "Synthetic B", snapshot(scenario).state.draft)
        }
    }

    @Test(timeout = 90_000) fun settingsLeftSwipeBackRightSwipeAndRecreationRetainUnsentDraft() {
        launch().use { scenario ->
            scenario.onActivity { it.setDraft(SETTINGS_DRAFT) }
            awaitDescription(SEND)
            assertPolarControls(SEND)
            openSettingsByPointer()
            instrumentation.sendKeyDownUpSync(KeyEvent.KEYCODE_BACK)
            awaitChat(SEND)
            assertDraftUnsent(scenario, SETTINGS_DRAFT)

            openSettingsByPointer()
            swipe(.18f, .48f, .88f, .48f)
            awaitChat(SEND)
            assertDraftUnsent(scenario, SETTINGS_DRAFT)
            assertTrue("Settings dismissal is not root activity dismissal", scenario.state == Lifecycle.State.RESUMED)

            openSettingsByPointer()
            scenario.recreate()
            awaitText("Settings")
            assertDraftUnsent(scenario, SETTINGS_DRAFT)
            instrumentation.sendKeyDownUpSync(KeyEvent.KEYCODE_BACK)
            awaitChat(SEND)
            assertDraftUnsent(scenario, SETTINGS_DRAFT)
            assertPolarControls(SEND)
        }
    }

    @Test(timeout = 60_000) fun rightwardRootEdgeSwipeStillDismissesInsteadOfOpeningSettings() {
        launch().use { scenario ->
            val activity = AtomicReference<AssistantUxFixtureActivity>()
            scenario.onActivity { it.setDraft(SETTINGS_DRAFT); activity.set(it) }
            awaitDescription(SEND)
            swipe(.025f, .48f, .92f, .48f)
            await("A rightward edge swipe must dismiss native root chat, not navigate to Settings") {
                var finished = false
                instrumentation.runOnMainSync { finished = activity.get().isFinishing || activity.get().isDestroyed }
                finished || scenario.state == Lifecycle.State.DESTROYED
            }
            assertFalse("Native root dismissal must not expose Settings", visibleNodes().any { it.text == "Settings" })
        }
    }

    @Test(timeout = 60_000) fun nativeBackOnRootFinishesWithoutInvokingSend() {
        launch().use { scenario ->
            val activity = AtomicReference<AssistantUxFixtureActivity>()
            scenario.onActivity { it.setDraft(SETTINGS_DRAFT); activity.set(it) }
            awaitDescription(SEND)
            instrumentation.sendKeyDownUpSync(KeyEvent.KEYCODE_BACK)
            await("Native Back on root must finish the host activity") {
                var finished = false
                instrumentation.runOnMainSync { finished = activity.get().isFinishing || activity.get().isDestroyed }
                finished || scenario.state == Lifecycle.State.DESTROYED
            }
            instrumentation.runOnMainSync {
                assertEquals(0, activity.get().snapshot().sends)
                assertEquals(SETTINGS_DRAFT, activity.get().snapshot().state.draft)
            }
        }
    }

    @Test(timeout = 90_000) fun latestIsCompactAndReturnsToSyntheticLatestAnswer() {
        launch().use { scenario ->
            scenario.onActivity { it.seedHistory() }
            awaitText("Synthetic answer 16. This fixture has no provider or private history.")
            assertFalse("Following latest initially needs no Latest target", visibleNodes().any { it.description == LATEST })
            val initialViewport = readingViewport()
            swipe(.5f, .25f, .5f, .66f)
            awaitDescription(LATEST)
            assertTrue("Latest replaces idle Ask, not a second primary target", visibleNodes().none { it.description == MIC })
            assertLatestGeometry()
            assertTrue("Floating Latest restores the primary dock area to the reading viewport",
                readingViewport().bottom > initialViewport.bottom)
            tap(LATEST)
            awaitText("Synthetic answer 16. This fixture has no provider or private history.")
            await("Latest action must restore follow-latest") { visibleNodes().none { it.description == LATEST } }
            awaitDescription(MIC)
            assertPolarControls(MIC)
            assertEquals(0, snapshot(scenario).sends)
            assertNoStreamSpeechControls()
        }
    }

    @Test(timeout = 180_000) fun longAnswerBeginningMiddleAndEndUseTransparentLatestAndPrimaryClearance() =
        assertLongAnswerReading(fontScale = 1f)

    @Test(timeout = 180_000) fun enlargedFontLongAnswerReadsBehindLatestAndEndsAbovePrimary() =
        assertLongAnswerReading(fontScale = 1.3f)

    @Test(timeout = 120_000) fun activeStopWinsWhileReadingOlderAndLatestReturnsAfterReplySettlement() {
        launch().use { scenario ->
            tap(MIC)
            scenario.onActivity { it.publishListening(); it.publishFinal("Synthetic reading prompt") }
            awaitDescription(STOP)
            scenario.onActivity { it.publishProvisional((1..30).joinToString("\n\n") { index ->
                "Synthetic streaming paragraph $index is long enough to scroll while response generation continues."
            }) }
            await("Overflowing streaming text appears") { visibleNodes().any { it.text?.contains("Synthetic streaming paragraph 30") == true } }
            swipe(.5f, .25f, .5f, .66f)
            awaitDescription(STOP)
            assertTrue(visibleNodes().none { it.description == LATEST })
            assertPolarControls(STOP)
            assertStopGlyph()
            val olderViewport = readingViewport()
            scenario.onActivity { it.completeRequest(answer = (1..30).joinToString("\n\n") { index ->
                "Synthetic settled paragraph $index is long enough to preserve manual reading during reply playback."
            }) }
            awaitDescription(STOP)
            swipe(.5f, .25f, .5f, .66f)
            assertTrue("Playback Stop also has priority over navigation", visibleNodes().none { it.description == LATEST })
            assertStopGlyph()
            scenario.onActivity { it.completePlayback() }
            awaitDescription(LATEST)
            assertLatestGeometry()
            assertTrue("Settled Latest removes Stop's viewport exclusion, not the reader's scroll state",
                readingViewport().bottom > olderViewport.bottom)
            tap(LATEST)
            awaitDescription(MIC)
            assertPolarControls(MIC)
            assertEquals(1, snapshot(scenario).sends)
            assertEquals(1, snapshot(scenario).micStarts)
        }
    }

    @Test(timeout = 120_000) fun listeningStopRemainsReachableWhileReadingOlderAndNeverBecomesLatest() {
        launch().use { scenario ->
            scenario.onActivity { it.seedHistory() }
            awaitDescription(MIC)
            tap(MIC)
            for (phase in listOf(DictationPhase.CHECKING, DictationPhase.LISTENING, DictationPhase.FINISHING)) {
                scenario.onActivity {
                    if (phase == DictationPhase.LISTENING) it.publishListening()
                    if (phase == DictationPhase.FINISHING) it.publishFinishing()
                }
                swipe(.5f, .25f, .5f, .66f)
                awaitDescription(STOP)
                assertPolarControls(STOP)
                assertStopGlyph()
                assertTrue("Stop wins over Latest during $phase", visibleNodes().none { it.description == LATEST })
            }
            tap(STOP)
            awaitDescription(LATEST)
            assertLatestGeometry()
            assertEquals(0, snapshot(scenario).sends)
            assertEquals(1, snapshot(scenario).stops)
        }
    }

    @Test(timeout = 120_000) fun touchingBusyOrPlaybackStopWhileOlderCancelsOwnersBeforeLatestNavigationReturns() {
        for (playback in listOf(false, true)) {
            launch(conversationChoice = true).use { scenario ->
                scenario.onActivity { it.seedHistory() }
                awaitDescription(CONVERSATION_MIC)
                tap(CONVERSATION_MIC)
                val input = AtomicLong()
                scenario.onActivity {
                    input.set(it.inputOwner()); it.publishListening(); it.publishFinal("Synthetic older-reading cancellation")
                    if (playback) it.completeRequest()
                }
                awaitDescription(STOP)
                swipe(.5f, .25f, .5f, .66f)
                assertPolarControls(STOP)
                assertTrue(visibleNodes().none { it.description == LATEST })
                val before = snapshot(scenario)
                tap(STOP)
                awaitDescription(LATEST)
                assertLatestGeometry()
                scenario.onActivity {
                    it.publishFinal("Synthetic stale final", input.get())
                    it.completeRequest(request = before.sends)
                    it.completePlayback(before.sends)
                    it.beginEmptyFollowup()
                }
                assertEquals(before.sends, snapshot(scenario).sends)
                assertEquals(before.micStarts, snapshot(scenario).micStarts)
                assertEquals(1, snapshot(scenario).stops)
                assertFalse(snapshot(scenario).state.busy)
                tap(LATEST)
                awaitDescription(if (playback) CONVERSATION_MIC else SEND)
            }
        }
    }

    @Test(timeout = 90_000) fun failedAndCanceledSyntheticRequestsKeepDraftAndNeverInventCompletedHead() {
        launch().use { scenario ->
            scenario.onActivity { it.setDraft(SETTINGS_DRAFT) }
            awaitDescription(SEND)
            tap(SEND)
            awaitText("Synthetic streaming reply")
            scenario.onActivity { it.failRequest() }
            awaitText("Network request failed. Draft retained; retry deliberately.")
            awaitDescription(SEND)
            assertEquals(null, snapshot(scenario).state.selectedHead)
            assertEquals(SETTINGS_DRAFT, snapshot(scenario).state.draft)
            assertEquals(1, snapshot(scenario).sends)

            tap(SEND)
            awaitText("Synthetic streaming reply")
            val stop = visibleNodes().single { it.description?.startsWith("Stop") == true }
            val bounds = stop.clickableBounds ?: stop.bounds
            openSettingsByPointer()
            scrollTextIntoView(KEYBOARD)
            val keyboardNode = visibleNodes().first { it.text == KEYBOARD }
            assertFalse("Keyboard intentionally stays disabled while generation is busy", keyboardNode.enabled)
            instrumentation.sendKeyDownUpSync(KeyEvent.KEYCODE_BACK)
            awaitDescription("Stop microphone, response and playback")
            assertEquals("Generation Stop stays bottom center", geometry.widthPixels / 2f, bounds.exactCenterX(), 1f)
            injectTap(bounds.exactCenterX(), bounds.exactCenterY())
            awaitText("Stopped. Draft retained.")
            awaitDescription(SEND)
            val canceled = snapshot(scenario)
            assertEquals(2, canceled.sends)
            assertEquals(1, canceled.stops)
            assertEquals(null, canceled.state.selectedHead)
            assertEquals(SETTINGS_DRAFT, canceled.state.draft)
            assertFalse(canceled.state.busy)
            assertPolarControls(SEND)
            assertNoStreamSpeechControls()
        }
    }

    @Test(timeout = 120_000) fun actualArtworkAnimatesForBusyAndResetsAfterCompletionFailureStopAndRecreation() {
        assertTrue("Artwork margin sampling targets the named round Wear emulator",
            instrumentation.targetContext.resources.configuration.isScreenRound)
        launch().use { scenario ->
            scenario.onActivity { it.setDraft(SETTINGS_DRAFT) }
            awaitDescription(SEND)
            val neutral = artworkMargins()
            assertTrue("Real shared artwork, not an all-black substitute, must render behind chat",
                neutral.count { Color.red(it) + Color.green(it) + Color.blue(it) > 30 } > neutral.size / 10)

            fun assertGenerating() {
                await("Busy must visibly transform the actual artwork") {
                    changedFraction(neutral, artworkMargins()) > .05f
                }
            }

            fun assertNeutral() {
                await("Terminal or inactive chat must return artwork to its original neutral transform") {
                    changedFraction(neutral, artworkMargins()) < .02f
                }
                SystemClock.sleep(500)
                assertTrue("Neutral artwork must stay reset instead of continuing a stale animation",
                    changedFraction(neutral, artworkMargins()) < .02f)
            }

            tap(SEND)
            awaitText("Synthetic streaming reply")
            assertGenerating()
            SystemClock.sleep(800)
            val generating = artworkMargins()
            assertReturningArtwork(scenario, neutral, generating)
            awaitText(AssistantUxFixtureActivity.SAVED_ANSWER)
            assertNeutral()

            scenario.onActivity { it.setDraft(SETTINGS_DRAFT) }
            awaitDescription(SEND)
            tap(SEND)
            awaitText("Synthetic streaming reply")
            assertGenerating()
            scenario.onActivity { it.failRequest() }
            awaitDescription(SEND)
            assertNeutral()

            tap(SEND)
            awaitText("Synthetic streaming reply")
            assertGenerating()
            val stop = visibleNodes().single { it.description?.startsWith("Stop") == true }
            val bounds = requireNotNull(stop.clickableBounds)
            injectTap(bounds.exactCenterX(), bounds.exactCenterY())
            awaitDescription(SEND)
            assertNeutral()

            val backgroundedActivity = AtomicReference<AssistantUxFixtureActivity>()
            scenario.onActivity { backgroundedActivity.set(it) }
            tap(SEND)
            awaitText("Synthetic streaming reply")
            assertGenerating()
            scenario.moveToState(Lifecycle.State.CREATED)
            instrumentation.runOnMainSync {
                assertFalse(backgroundedActivity.get().snapshot().state.foreground)
                backgroundedActivity.get().failRequest()
            }
            scenario.moveToState(Lifecycle.State.RESUMED)
            awaitDescription(SEND)
            assertNeutral()

            scenario.recreate()
            awaitDescription(SEND)
            assertNeutral()
            openSettingsByPointer()
            instrumentation.sendKeyDownUpSync(KeyEvent.KEYCODE_BACK)
            awaitChat(SEND)
            assertNeutral()
        }
    }

    private fun readingViewport(): Rect = visibleNodes().filter { it.scrollable }.maxByOrNull { it.bounds.height() }!!.bounds

    private fun assertDockClearance(control: Rect) {
        val viewport = readingViewport()
        assertEquals("Only the primary reserves its reachable action height plus the 3dp reading gap",
            maxOf(48f, CompactEdgeButtonDefaults.Height.value) + 3f,
            (geometry.heightPixels - viewport.bottom) / geometry.density, 1f)
        assertTrue("Text viewport never intersects the action's touch target", viewport.bottom < control.top)
    }

    private fun assertLatestGeometry() {
        val latest = controlBounds(LATEST)
        assertEquals("Latest uses native 48dp round target", 48f, latest.width() / geometry.density, 1f)
        assertEquals("Latest uses native 48dp round target", 48f, latest.height() / geometry.density, 1f)
        assertEquals("Latest has only the specified 6dp bottom inset", 6f,
            (geometry.heightPixels - latest.bottom) / geometry.density, 1f)
        assertEquals(geometry.widthPixels / 2f, latest.exactCenterX(), 1f)
        assertEquals("One accessible down-to-end action, not icon plus parent buttons", 1,
            visibleNodes().count { it.description == LATEST })
        assertEquals("Latest exposes the native Button role", "android.widget.Button",
            visibleNodes().single { it.description == LATEST }.className)
        assertTrue("Latest is a glyph rather than a padded text footer", visibleNodes().none { it.text == "Latest" })
        assertEquals("Latest floats inside the full-height scrollable viewport; no black footer clips text",
            geometry.heightPixels, readingViewport().bottom)
        assertTrue("Text and Latest intentionally share the bottom reading region", Rect.intersects(readingViewport(), latest))
    }

    private fun assertStopGlyph() {
        awaitDescription(STOP)
        val bounds = controlBounds(STOP)
        val bitmap = requireNotNull(automation.takeScreenshot())
        try {
            val x = bounds.centerX()
            val y = bounds.centerY() - geometry.density.toInt()
            val center = bitmap.getPixel(x, y)
            fun distance(a: Int, b: Int) = kotlin.math.abs(Color.red(a) - Color.red(b)) +
                kotlin.math.abs(Color.green(a) - Color.green(b)) + kotlin.math.abs(Color.blue(a) - Color.blue(b))
            val inner = (3f * geometry.density).toInt()
            for (dx in listOf(-inner, inner)) for (dy in listOf(-inner, inner)) {
                assertTrue("Stop has filled square corners, not the old outlined microphone",
                    distance(center, bitmap.getPixel(x + dx, y + dy)) < 24)
            }
            val outer = (7f * geometry.density).toInt()
            assertTrue("The square has a visible boundary inside the native compact icon",
                distance(center, bitmap.getPixel(x + outer, y)) > 24)
        } finally { bitmap.recycle() }
    }

    private data class CharacterBox(val index: Int, val bounds: RectF)

    /** Real Compose text-layout accessibility boxes, not the full (possibly off-screen) paragraph node. */
    private fun answerCharacterBoxes(expectedText: String? = null): Pair<String, List<CharacterBox>> {
        val root = requireNotNull(automation.rootInActiveWindow)
        fun read(node: AccessibilityNodeInfo): Pair<String, List<CharacterBox>>? {
            val text = node.text?.toString()
            if (text != null && (if (expectedText == null) text.contains("Synthetic long answer end.") else text == expectedText)) {
                val key = AccessibilityNodeInfo.EXTRA_DATA_TEXT_CHARACTER_LOCATION_KEY
                assertTrue("Actual renderer must expose text character geometry", node.availableExtraData.contains(key))
                val arguments = Bundle().apply {
                    putInt(AccessibilityNodeInfo.EXTRA_DATA_TEXT_CHARACTER_LOCATION_ARG_START_INDEX, 0)
                    putInt(AccessibilityNodeInfo.EXTRA_DATA_TEXT_CHARACTER_LOCATION_ARG_LENGTH, text.length)
                }
                assertTrue("Compose supplies actual character bounds", node.refreshWithExtraData(key, arguments))
                @Suppress("DEPRECATION")
                val locations = requireNotNull(node.extras.getParcelableArray(key))
                return text to locations.mapIndexedNotNull { index, value ->
                    (value as? RectF)?.takeIf { it.width() > 0 && it.height() > 0 }?.let { CharacterBox(index, RectF(it)) }
                }
            }
            for (index in 0 until node.childCount) node.getChild(index)?.let { child ->
                try { read(child)?.let { return it } } finally { child.recycle() }
            }
            return null
        }
        try { return requireNotNull(read(root)) { "Synthetic long answer must be exposed by actual AssistantScreen" } }
        finally { root.recycle() }
    }

    private fun visibleAnswerCharacters(): Pair<String, List<CharacterBox>> {
        val (text, boxes) = answerCharacterBoxes()
        val viewport = RectF(readingViewport())
        return text to boxes.filter { viewport.contains(it.bounds) }
    }

    private fun assertLongAnswerReading(fontScale: Float) {
        launch(fontScale = fontScale).use { scenario ->
            scenario.onActivity { it.seedLongAnswer() }
            awaitDescription(MIC)
            await("Long synthetic answer is displayed") { visibleNodes().any { it.text?.contains("Synthetic long answer end.") == true } }
            assertDockClearance(controlBounds(MIC))
            fun visible(index: Int) = visibleAnswerCharacters().second.any { it.index == index }
            val answer = answerCharacterBoxes().first
            val end = answer.indexOf("Synthetic long answer end.")
            await("Follow-latest exposes the final answer line, not just a full-paragraph semantic node") { visible(end) }
            fun scrollToCharacter(index: Int, backwards: Boolean) {
                repeat(70) {
                    if (visible(index)) return
                    if (backwards) swipe(.5f, .25f, .5f, .66f) else swipe(.5f, .66f, .5f, .27f)
                }
            }
            scrollToCharacter(0, backwards = true)
            assertTrue("Beginning of overflowing answer can be read", visible(0))
            awaitDescription(LATEST)
            assertLatestGeometry()
            assertTextPaintsBehindLatest()
            val beginning = visibleAnswerCharacters().second
            val tops = beginning.map { it.bounds.top }.distinct().sorted()
            val expectedLineHeight = 16f * geometry.density * fontScale
            assertTrue("Actual bodyMedium line advance follows 16sp and the requested font scale",
                tops.zipWithNext().any { (first, second) -> kotlin.math.abs(second - first - expectedLineHeight) <= 2f })
            assertTrue("The old larger reading body role must not return",
                tops.zipWithNext().filter { (first, second) -> second - first < expectedLineHeight * 1.5f }
                    .all { (first, second) -> kotlin.math.abs(second - first - expectedLineHeight) <= 2f })
            val middle = answer.indexOf("Synthetic long paragraph 20 ")
            assertTrue("Synthetic middle marker is present in the complete answer", middle > 0)
            scrollToCharacter(middle, backwards = false)
            assertTrue("Middle of overflowing answer can be read", visible(middle))
            awaitDescription(LATEST)
            assertLatestGeometry()
            assertTextPaintsBehindLatest()
            assertEquals("Text remains complete rather than shortened/ellipsized", answer, answerCharacterBoxes().first)
            tap(LATEST)
            awaitDescription(MIC)
            await("Latest returns to actual final-line geometry after dock layout settles") { visible(end) }
            assertDockClearance(controlBounds(MIC))
            assertPolarControls(MIC)
            assertEquals(0, snapshot(scenario).sends)
        }
    }

    private fun assertTextPaintsBehindLatest() {
        val latest = controlBounds(LATEST)
        val icon = RectF(latest).apply { inset(12f * geometry.density, 12f * geometry.density) }
        val (text, characters) = visibleAnswerCharacters()
        val behindTarget = characters.filter { text[it.index].isLetterOrDigit() &&
            RectF.intersects(it.bounds, RectF(latest)) && !RectF.intersects(it.bounds, icon) }
        assertTrue("Long text must continue into Latest's transparent touch region outside the arrow", behindTarget.isNotEmpty())
        val bitmap = requireNotNull(automation.takeScreenshot())
        try {
            val radius = 23f * geometry.density
            assertTrue("Actual text ink remains painted behind the transparent target, not covered by a filled container",
                behindTarget.any { character ->
                    val bounds = RectF(character.bounds).apply { intersect(RectF(latest)) }
                    (bounds.top.toInt().coerceAtLeast(0) until bounds.bottom.toInt().coerceAtMost(bitmap.height)).any { y ->
                        (bounds.left.toInt().coerceAtLeast(0) until bounds.right.toInt().coerceAtMost(bitmap.width)).any { x ->
                            val pixel = bitmap.getPixel(x, y)
                            val dx = x - latest.exactCenterX()
                            val dy = y - latest.exactCenterY()
                            dx * dx + dy * dy < radius * radius &&
                                Color.red(pixel) > 180 && Color.green(pixel) > 180 && Color.blue(pixel) > 180
                        }
                    }
                })
        } finally { bitmap.recycle() }
    }

    private fun artworkMargins(): IntArray {
        val bitmap = requireNotNull(automation.takeScreenshot()) { "UiAutomation screenshot must be available" }
        try {
            // Stay outside M3's 5.2% text margin and away from the right-hand scroll indicator.
            val xFractions = listOf(.025f, .03f, .035f, .04f)
            return xFractions.flatMap { x ->
                (40..60 step 2).map { y -> bitmap.getPixel((bitmap.width * x).toInt(), bitmap.height * y / 100) }
            }.toIntArray()
        } finally { bitmap.recycle() }
    }

    private fun assertReturningArtwork(scenario: ActivityScenario<AssistantUxFixtureActivity>, neutral: IntArray, generating: IntArray) {
        val completedAt = AtomicLong()
        scenario.onActivity {
            completedAt.set(SystemClock.elapsedRealtime())
            it.completeRequest()
        }
        val samples = mutableListOf<String>()
        var observedIntermediate = false
        val deadline = completedAt.get() + 5_000
        // Capture before accessibility polling: it can consume the entire 1500ms return animation.
        do {
            val startedAt = SystemClock.elapsedRealtime()
            val returning = artworkMargins()
            val finishedAt = SystemClock.elapsedRealtime()
            val fromNeutral = changedFraction(neutral, returning)
            val fromGenerating = changedFraction(generating, returning)
            samples += "startMs=${startedAt - completedAt.get()} endMs=${finishedAt - completedAt.get()} fromNeutral=$fromNeutral fromGenerating=$fromGenerating"
            observedIntermediate = fromNeutral > .02f && fromGenerating > .02f
            if (!observedIntermediate) SystemClock.sleep(16)
        } while (!observedIntermediate && SystemClock.elapsedRealtime() < deadline)
        assertTrue("A new completed head must visibly animate back, not instantly snap to neutral. " +
            "Samples: ${samples.joinToString("; ")}", observedIntermediate)
    }

    private fun glyphPixels(bounds: Rect): IntArray {
        val bitmap = requireNotNull(automation.takeScreenshot()) { "UiAutomation screenshot must be available" }
        try {
            // Includes the compact native icon and surrounding paint for action/color comparison.
            val size = (26f * geometry.density).toInt()
            return IntArray(size * size).also { pixels ->
                bitmap.getPixels(pixels, 0, size, bounds.centerX() - size / 2, bounds.centerY() - size / 2, size, size)
            }
        } finally { bitmap.recycle() }
    }

    private fun changedFraction(first: IntArray, second: IntArray): Float {
        assertEquals("Screenshot sample geometry must remain stable", first.size, second.size)
        return first.indices.count { index ->
            kotlin.math.abs(Color.red(first[index]) - Color.red(second[index])) > 8 ||
                kotlin.math.abs(Color.green(first[index]) - Color.green(second[index])) > 8 ||
                kotlin.math.abs(Color.blue(first[index]) - Color.blue(second[index])) > 8
        }.toFloat() / first.size
    }

    private fun launch(conversationChoice: Boolean? = false, fontScale: Float? = null): ActivityScenario<AssistantUxFixtureActivity> = ActivityScenario.launch(
        Intent(instrumentation.targetContext, AssistantUxFixtureActivity::class.java)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK).apply {
                conversationChoice?.let { putExtra(AssistantUxFixtureActivity.CONVERSATION_CHOICE, it) }
                fontScale?.let { putExtra(AssistantUxFixtureActivity.FONT_SCALE, it) }
            })

    private fun snapshot(scenario: ActivityScenario<AssistantUxFixtureActivity>): AssistantUxFixtureSnapshot {
        val value = AtomicReference<AssistantUxFixtureSnapshot>()
        scenario.onActivity { value.set(it.snapshot()) }
        return value.get()
    }

    private fun assertDraftUnsent(scenario: ActivityScenario<AssistantUxFixtureActivity>, text: String) {
        val value = snapshot(scenario)
        assertEquals(text, value.state.draft)
        assertEquals(0, value.sends)
        assertEquals(0, value.edits)
        assertEquals(0, value.dictates)
        assertFalse(value.state.busy)
    }

    private fun openSettingsByPointer() {
        swipe(.82f, .48f, .18f, .48f)
        awaitText("Settings")
        awaitText("Back to chat")
    }

    private fun assertPolarControls(middle: String) {
        await("The persistent compact action must finish layout before measuring its geometry") {
            val primary = visibleNodes().firstOrNull { it.description == middle }?.let { it.clickableBounds ?: it.bounds }
            primary != null && primary.height() / geometry.density >= CompactEdgeButtonDefaults.Height.value - 1f
        }
        val nodes = visibleNodes()
        val descriptions = nodes.mapNotNull { it.description }
        assertTrue(descriptions.contains(middle))
        assertFalse("New, Chats and Keyboard live only in Settings", nodes.any {
            it.description in listOf(NEW, CHATS, KEYBOARD) || it.text in listOf(NEW, CHATS, KEYBOARD)
        })
        assertFalse("Settings stays swipe-accessible rather than taking chat space", descriptions.contains("Settings"))
        val primary = controlBounds(middle)
        assertEquals("The primary has one semantic action target", 1, nodes.count { it.description == middle })
        assertEquals("The primary exposes the native Button role", "android.widget.Button",
            nodes.single { it.description == middle }.className)
        assertTrue("The native edge action uses the screen width", primary.width() >= geometry.widthPixels - 2)
        assertTrue("The compact visual shape retains a full 48dp semantic touch target within the screen",
            primary.height() / geometry.density in 47f..49f)
        assertTrue("Target bounds stay on screen", Rect(0, 0, geometry.widthPixels, geometry.heightPixels).contains(primary))
        assertEquals("Target is centered horizontally", geometry.widthPixels / 2f, primary.exactCenterX(), 1f)
        assertTrue("Primary stays bottom center", primary.centerY() > geometry.heightPixels * .75f)
        assertTrue("The edge action's accessible bounds hug the bottom; painted shape is verified separately",
            geometry.heightPixels - primary.bottom <= 4f * geometry.density)
        // A full-width edge button has a curved painted shape, not an inscribed rectangular target.
        // Offscreen paragraph semantics also cannot prove painted glyph readability at the bezel.
        val rowDescriptions = visibleNodes().filter {
            it.description != null && (it.clickableBounds ?: it.bounds).centerY() == primary.centerY()
        }.map { it.description }
        assertEquals("Only the primary action shares the bottom row", setOf(middle), rowDescriptions.toSet())
        val rowTargets = visibleNodes().mapNotNull { it.clickableBounds }
            .filter { it.centerY() == primary.centerY() }.distinct()
        assertEquals("No hidden extra control shares the bottom row", 1, rowTargets.size)
        assertDockClearance(primary)
    }

    private fun assertNoStreamSpeechControls() {
        assertTrue("No inline Read aloud or stream Stop microphone control", visibleNodes().none {
            it.text?.contains("Read aloud", ignoreCase = true) == true ||
                it.text?.contains("Stop microphone", ignoreCase = true) == true
        })
    }

    private fun assertNoSpeechRoutes() {
        assertTrue("Speech provider and route details belong only in Settings", visibleNodes().none {
            it.text?.startsWith("Dictation:") == true || it.text?.startsWith("Replies:") == true ||
                it.text?.contains("requested en-GB") == true
        })
    }

    private fun assertOneYouPrompt(text: String) {
        awaitText(text)
        repeat(4) {
            if (visibleNodes().none { it.text == "You" }) swipe(.5f, .28f, .5f, .65f)
        }
        val nodes = visibleNodes()
        assertEquals("One You label across live, final, busy and canonical phases", 1, nodes.count { it.text == "You" })
        assertEquals("Prompt text must not be duplicated", 1, nodes.count { it.text == text })
        assertTrue("Prompt labels never switch to Draft or review-before-Send", nodes.none {
            it.text?.contains("Draft ·") == true || it.text?.contains("Listening · draft") == true
        })
        assertNoSpeechRoutes()
    }

    private fun assertTwoYouPrompts(first: String, second: String) {
        awaitText(second)
        assertVisiblePromptHasYou(second)
        repeat(5) {
            if (!promptHasVisibleYou(first)) swipe(.5f, .28f, .5f, .65f)
        }
        assertVisiblePromptHasYou(first)
        if (visibleNodes().any { it.description == LATEST }) tap(LATEST)
        else repeat(8) {
            if (!promptHasVisibleYou(second)) swipe(.5f, .66f, .5f, .27f)
        }
        awaitText(second)
        assertVisiblePromptHasYou(second)
        assertNoSpeechRoutes()
    }

    private fun promptHasVisibleYou(text: String): Boolean {
        val nodes = visibleNodes()
        val prompt = nodes.firstOrNull { it.text == text } ?: return false
        return nodes.any { it.text == "You" && it.bounds.bottom <= prompt.bounds.top &&
            prompt.bounds.top - it.bounds.bottom <= 24f * geometry.density }
    }

    private fun assertVisiblePromptHasYou(text: String) {
        assertEquals("Each distinct turn's prompt appears exactly once in its viewport", 1, visibleNodes().count { it.text == text })
        assertTrue("$text keeps its own immediately preceding You label", promptHasVisibleYou(text))
    }

    private fun scrollTextIntoView(text: String) = scrollTextContainingIntoView(text, exact = true)

    private fun scrollTextContainingIntoView(text: String, exact: Boolean = false, backwards: Boolean = false) {
        fun found() = visibleNodes().any { if (exact) it.text == text else it.text?.contains(text) == true }
        repeat(12) {
            if (found()) return
            if (backwards) swipe(.5f, .25f, .5f, .66f) else swipe(.5f, .75f, .5f, .30f)
        }
        assertTrue("Settings must expose $text", found())
    }

    private data class Node(val text: String?, val description: String?, val bounds: Rect, val clickableBounds: Rect?,
        val enabled: Boolean, val scrollable: Boolean, val className: String?)

    private fun visibleNodes(): List<Node> {
        val result = mutableListOf<Node>()
        fun visit(node: AccessibilityNodeInfo, clickable: Rect?, enabled: Boolean) {
            val bounds = Rect().also { node.getBoundsInScreen(it) }
            val target = if (node.isClickable) bounds else clickable
            val canClick = enabled && node.isEnabled
            if (node.isVisibleToUser) result += Node(node.text?.toString(), node.contentDescription?.toString(), bounds, target,
                canClick, node.isScrollable, node.className?.toString())
            for (index in 0 until node.childCount) node.getChild(index)?.let { child ->
                try { visit(child, target, canClick) } finally { child.recycle() }
            }
        }
        automation.rootInActiveWindow?.let { root ->
            try { visit(root, null, true) } finally { root.recycle() }
        }
        return result
    }

    private fun controlBounds(label: String, description: Boolean = true, requireEnabled: Boolean = true): Rect {
        val node = visibleNodes().first { (if (description) it.description else it.text) == label }
        if (requireEnabled) assertTrue("$label is enabled", node.enabled)
        return requireNotNull(node.clickableBounds) { "$label must belong to a clickable target" }
    }

    private fun tap(label: String, description: Boolean = true) {
        if (description) awaitDescription(label) else awaitText(label)
        if (description && label in listOf(MIC, CONVERSATION_MIC, SEND, STOP)) {
            await("The compact primary must finish layout before a center tap") {
                val node = visibleNodes().firstOrNull { it.description == label }
                node != null && (node.clickableBounds ?: node.bounds).height() / geometry.density >= CompactEdgeButtonDefaults.Height.value - 1f
            }
        }
        val bounds = controlBounds(label, description)
        injectTap(bounds.exactCenterX(), bounds.exactCenterY())
    }

    private fun injectTap(x: Float, y: Float) {
        val down = SystemClock.uptimeMillis()
        inject(MotionEvent.ACTION_DOWN, down, down, x, y)
        SystemClock.sleep(75)
        inject(MotionEvent.ACTION_UP, down, SystemClock.uptimeMillis(), x, y)
    }

    private fun swipe(startX: Float, startY: Float, endX: Float, endY: Float) {
        val down = SystemClock.uptimeMillis()
        fun point(action: Int, fraction: Float) = inject(action, down, SystemClock.uptimeMillis(),
            geometry.widthPixels * (startX + (endX - startX) * fraction),
            geometry.heightPixels * (startY + (endY - startY) * fraction))
        point(MotionEvent.ACTION_DOWN, 0f)
        for (step in 1..18) {
            SystemClock.sleep(20)
            point(MotionEvent.ACTION_MOVE, step / 18f)
        }
        point(MotionEvent.ACTION_UP, 1f)
    }

    private fun inject(action: Int, downTime: Long, eventTime: Long, x: Float, y: Float) {
        val event = MotionEvent.obtain(downTime, eventTime, action, x, y, 0).apply { source = InputDevice.SOURCE_TOUCHSCREEN }
        try { assertTrue("UiAutomation must inject the actual touchscreen event", automation.injectInputEvent(event, true)) }
        finally { event.recycle() }
    }

    private fun awaitDescription(description: String) = await("Visible control: $description") {
        visibleNodes().any { it.description == description && it.enabled }
    }

    private fun awaitChat(middle: String) = await("Settings is dismissed and root chat is visible") {
        val nodes = visibleNodes()
        nodes.none { it.text == "Settings" } && nodes.any { it.description == middle && it.enabled }
    }

    private fun awaitText(text: String) = await("Visible synthetic text: $text") { visibleNodes().any { it.text == text } }

    private fun await(message: String, timeoutMillis: Long = 15_000, condition: () -> Boolean) {
        val deadline = SystemClock.elapsedRealtime() + timeoutMillis
        while (!condition()) {
            assertTrue(message, SystemClock.elapsedRealtime() < deadline)
            SystemClock.sleep(50)
        }
    }

    companion object {
        private const val CHATS = "Chats"
        private const val KEYBOARD = "Keyboard"
        private const val NEW = "New chat"
        private const val MIC = "Ask once"
        private const val CONVERSATION_MIC = "Talk continuously"
        private const val STOP = "Stop microphone, response and playback"
        private const val LATEST = "Jump to latest"
        private const val SEND = "Send draft"
        private const val SETTINGS_DRAFT = "Synthetic draft survives Settings."
    }
}
