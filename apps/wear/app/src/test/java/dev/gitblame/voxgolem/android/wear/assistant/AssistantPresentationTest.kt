package dev.gitblame.voxgolem.android.wear.assistant

import androidx.compose.ui.unit.sp
import androidx.wear.compose.material3.Typography
import dev.gitblame.voxgolem.assistant.recognition.DictationPhase
import dev.gitblame.voxgolem.assistant.recognition.DictationState
import dev.gitblame.voxgolem.assistant.recognition.RecognitionRoute
import dev.gitblame.voxgolem.assistant.runtime.AssistantState
import dev.gitblame.voxgolem.assistant.speech.SpeechPhase
import dev.gitblame.voxgolem.assistant.speech.SpeechState
import dev.gitblame.voxgolem.assistant.speech.TtsRoute
import dev.gitblame.voxgolem.assistant.speech.LoopPhase
import dev.gitblame.voxgolem.assistant.speech.ConversationLoopState
import dev.gitblame.voxgolem.android.wear.formatVisibleDuration
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import kotlinx.coroutines.runBlocking

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class AssistantPresentationTest {
    private val conversation = "{\"replica\":\"synthetic-chat\",\"sequence\":\"1\"}"
    private val ready = AssistantState(conversation = conversation, foreground = true, draft = "Synthetic draft")

    @Test fun streamingHeaderDisplaysSuppliedProviderMillisecondsWithoutWaitingForTextOrVisibility() {
        val stream = ready.copy(busy = true, providerRequestId = "synthetic-request:1", firstTextMs = 123L,
            provisional = "")
        assertEquals("Assistant · 123 ms · not saved yet", assistantStreamingReplyLabel(stream))
        assertEquals("Assistant · 123 ms · not saved yet",
            assistantStreamingReplyLabel(stream.copy(provisional = "Synthetic streamed answer")))
        assertEquals("Assistant · 0 ms · not saved yet", assistantStreamingReplyLabel(stream.copy(firstTextMs = 0L)))
    }

    @Test fun replyHeadersUseRoundedSecondsAtOneSecondWithoutChangingRequestMetrics() {
        val stream = ready.copy(busy = true, providerRequestId = "synthetic-request:1", firstTextMs = 999L)
        assertEquals("Assistant · 999 ms · not saved yet", assistantStreamingReplyLabel(stream))
        assertEquals("Assistant · 1 s · not saved yet", assistantStreamingReplyLabel(stream.copy(firstTextMs = 1000L)))
        assertEquals("Assistant · 1.3 s · not saved yet", assistantStreamingReplyLabel(stream.copy(firstTextMs = 1300L)))
        assertEquals("Assistant · 1.24 s · not saved yet", assistantStreamingReplyLabel(stream.copy(firstTextMs = 1235L)))
        assertEquals(999L, stream.firstTextMs)
    }

    @Test fun visibleDurationKeepsPrecisionBoundedAndAvoidsOverflowOrScientificNotation() {
        assertEquals("0 ms", formatVisibleDuration(0))
        assertEquals("1.23 s", formatVisibleDuration(1234))
        assertEquals("1.24 s", formatVisibleDuration(1235))
        assertEquals("2 s", formatVisibleDuration(1999))
        assertEquals("10 s", formatVisibleDuration(10_000))
        assertEquals("9223372036854775.81 s", formatVisibleDuration(Long.MAX_VALUE))
    }

    @Test fun streamingHeaderUsesOnlyTheCurrentRequestAndPrefersItsFrozenMetric() {
        val recorded = mapOf("synthetic-request:1" to 123L)
        val lookup: (String?) -> Long? = { recorded[it] }
        val stream = ready.copy(busy = true, providerRequestId = "synthetic-request:1", firstTextMs = 456L)
        assertEquals("Assistant · 123 ms · not saved yet", assistantStreamingReplyLabel(stream, lookup))
        assertEquals("Assistant · not saved yet",
            assistantStreamingReplyLabel(stream.copy(providerRequestId = "synthetic-request:2", firstTextMs = null), lookup))
        assertEquals("Assistant · not saved yet",
            assistantStreamingReplyLabel(stream.copy(providerRequestId = null), lookup))
        assertEquals("Assistant · not saved yet", assistantStreamingReplyLabel(stream.copy(firstTextMs = -1L)))
    }

    @Test fun completedHeadersJoinByRequestNotTurnIdAndKeepDeclinedAndUnknownLabels() {
        val request = "synthetic-request:1"
        val completed = turn("31").copy(requestKey = request)
        val recorded = mapOf(request to 123L, assistantIdKey(completed.id) to 999L)
        val lookup: (String?) -> Long? = { recorded[it] }
        assertEquals("Assistant · 123 ms", assistantCompletedReplyLabel(completed, lookup))
        assertEquals("Assistant · 123 ms · declined", assistantCompletedReplyLabel(completed.copy(refusal = true), lookup))
        assertEquals("Assistant", assistantCompletedReplyLabel(completed.copy(requestKey = null), lookup))
        assertEquals("Assistant", assistantCompletedReplyLabel(completed.copy(requestKey = null)) { 999L })
        assertEquals("Assistant · declined",
            assistantCompletedReplyLabel(completed.copy(refusal = true, requestKey = "synthetic-unmeasured:2"), lookup))
        assertEquals("Assistant", assistantCompletedReplyLabel(completed))
        assertEquals("Assistant", assistantCompletedReplyLabel(completed) { -1L })
    }

    @Test fun manualSendAcceptsANewDraftWithoutSelectingAnyExistingChatButStillRequiresIdleResolvedState() {
        assertTrue(assistantCanSend(ready, AssistantVoiceState()))
        assertTrue(assistantCanSend(ready.copy(conversation = null), AssistantVoiceState()))
        listOf(ready.copy(foreground = false), ready.copy(busy = true),
            ready.copy(draft = " \n"), ready.copy(branchChoices = listOf(conversation)), ready.copy(model = "synthetic-unsupported"))
            .forEach { assertFalse(assistantCanSend(it, AssistantVoiceState())) }
        assertFalse(assistantCanSend(ready, AssistantVoiceState(dictation = DictationState(phase = DictationPhase.LISTENING))))
    }

    @Test fun edgePrimaryPreservesBusyListeningPlaybackDraftAndIdlePriority() {
        val listening = dictating(DictationPhase.LISTENING, "Synthetic partial")
        val playback = AssistantVoiceState(speech = SpeechState(phase = SpeechPhase.SPEAKING))
        assertEquals(AssistantPrimaryAction.STOP, assistantPrimaryControl(ready.copy(busy = true), listening, true).action)
        assertEquals(AssistantPrimaryAction.STOP, assistantPrimaryControl(ready, listening, true).action)
        assertEquals(AssistantPrimaryAction.STOP, assistantPrimaryControl(ready, playback, true).action)
        assertEquals(AssistantPrimaryAction.SEND, assistantPrimaryControl(ready, AssistantVoiceState(), true).action)
        assertEquals(AssistantPrimaryAction.ASK, assistantPrimaryControl(ready.copy(draft = ""), AssistantVoiceState(), false).action)
        assertEquals(AssistantPrimaryAction.TALK, assistantPrimaryControl(ready.copy(draft = ""), AssistantVoiceState(), true).action)
    }

    @Test fun paragraphTokenUsesOneSmallerSemanticBodyRoleWithoutFontScaleOverrides() {
        val token = Typography().bodyMedium
        assertEquals(14.sp, token.fontSize)
        assertEquals(16.sp, token.lineHeight)
    }

    @Test fun primaryStopRemainsEnabledThroughAdmissionLossAndAllInFlightPhases() {
        val background = ready.copy(foreground = false)
        for (phase in listOf(DictationPhase.CHECKING, DictationPhase.LISTENING, DictationPhase.FINISHING)) {
            val control = assistantPrimaryControl(background, dictating(phase, "Synthetic partial"), false)
            assertEquals(AssistantPrimaryControl(AssistantPrimaryAction.STOP, true), control)
            assertEquals(AssistantPrimaryControl(AssistantPrimaryAction.STOP, true),
                assistantPrimaryControl(background.copy(busy = true), dictating(phase, "Synthetic partial"), false))
        }
        for (phase in listOf(SpeechPhase.CHECKING, SpeechPhase.SPEAKING)) {
            assertEquals(AssistantPrimaryControl(AssistantPrimaryAction.STOP, true),
                assistantPrimaryControl(background, AssistantVoiceState(speech = SpeechState(phase = phase)), false))
        }
    }

    @Test fun pendingLaunchOrPermissionAdmissionOffersStopBeforeNativeRecognitionStarts() {
        val pending = AssistantVoiceState(admissionPending = true, admissionReady = false,
            recognitionServiceAvailable = false, entryMode = AssistantVoiceEntryMode.ASK)
        assertEquals(AssistantPrimaryControl(AssistantPrimaryAction.STOP, true),
            assistantPrimaryControl(ready.copy(foreground = false), pending, false))
        assertFalse(assistantShowsLatest(ready, pending, true))
        val confirmation = pending.copy(admissionPending = false, launchConfirmationRequired = true,
            admissionReady = true, recognitionServiceAvailable = true)
        assertFalse(confirmation.active)
        assertEquals(AssistantPrimaryAction.ASK,
            assistantPrimaryControl(ready.copy(draft = ""), confirmation, false).action)
    }

    @Test fun externalVoiceConfirmationIsReachableWithRetainedDraftAndDoesNotSelectSend() {
        for ((mode, action, oppositeDefault) in listOf(
            Triple(AssistantVoiceEntryMode.ASK, AssistantPrimaryAction.ASK, true),
            Triple(AssistantVoiceEntryMode.TALK, AssistantPrimaryAction.TALK, false),
        )) {
            val confirmation = AssistantVoiceState(launchConfirmationRequired = true, entryMode = mode,
                admissionReady = true, recognitionServiceAvailable = true)
            assertEquals(AssistantPrimaryControl(action, true), assistantPrimaryControl(ready, confirmation, oppositeDefault))
            assertFalse(assistantShowsLatest(ready, confirmation, true))
            assertEquals("Synthetic draft", ready.draft)
            assertEquals(conversation, ready.conversation)
            assertEquals(AssistantPrimaryAction.STOP,
                assistantPrimaryControl(ready.copy(busy = true), confirmation, oppositeDefault).action)
        }
    }

    @Test fun edgePrimaryRetainsTheExactSendAndIdleCapabilityGatesAndAccessibleNames() {
        val voice = AssistantVoiceState(admissionReady = true)
        for (state in listOf(ready, ready.copy(foreground = false), ready.copy(branchChoices = listOf(conversation)),
            ready.copy(model = "synthetic-unsupported"))) {
            assertEquals(assistantCanSend(state, voice), assistantPrimaryControl(state, voice, false).enabled)
        }
        val idle = ready.copy(draft = "")
        assertTrue(assistantPrimaryControl(idle, voice, false).enabled)
        for ((state, capability) in listOf(idle.copy(foreground = false) to voice,
            idle.copy(branchChoices = listOf(conversation)) to voice,
            idle to voice.copy(admissionReady = false), idle to voice.copy(recognitionServiceAvailable = false))) {
            assertFalse(assistantPrimaryControl(state, capability, false).enabled)
        }
        assertTrue("Do not add a model gate to idle Mic", assistantPrimaryControl(idle.copy(model = "synthetic-unsupported"), voice, false).enabled)
        assertEquals("Send draft", AssistantPrimaryAction.SEND.description)
        assertEquals("Stop microphone, response and playback", AssistantPrimaryAction.STOP.description)
        assertEquals("Ask once", AssistantPrimaryAction.ASK.description)
        assertEquals("Talk continuously", AssistantPrimaryAction.TALK.description)
    }

    @Test fun transientAskAndTalkEntriesOverrideButNeverMutateThePersistentDefault() {
        for (default in listOf(false, true)) {
            val preferences = WearVoicePreferences(conversationMode = default)
            assertEquals(default, assistantRepeatsAfterReply(AssistantVoiceState(), preferences))
            assertFalse(assistantRepeatsAfterReply(AssistantVoiceState(entryMode = AssistantVoiceEntryMode.ASK), preferences))
            assertTrue(assistantRepeatsAfterReply(AssistantVoiceState(entryMode = AssistantVoiceEntryMode.TALK), preferences))
            assertEquals(default, preferences.conversationMode)
        }
    }

    @Test fun latestIsAnIdleReadingActionAndCannotReplaceStopInAnySpeechPhase() {
        assertTrue(assistantShowsLatest(ready, AssistantVoiceState(), true))
        assertFalse(assistantShowsLatest(ready, AssistantVoiceState(), false))
        assertFalse(assistantShowsLatest(ready.copy(busy = true), AssistantVoiceState(), true))
        val active = listOf(DictationPhase.CHECKING, DictationPhase.LISTENING, DictationPhase.FINISHING)
            .map { dictating(it, "Synthetic partial") } +
            listOf(SpeechPhase.CHECKING, SpeechPhase.SPEAKING).map { AssistantVoiceState(speech = SpeechState(phase = it)) } +
            listOf(LoopPhase.LISTENING, LoopPhase.GENERATING, LoopPhase.SPEAKING, LoopPhase.BETWEEN_TURNS)
                .map { AssistantVoiceState(loop = ConversationLoopState(phase = it)) }
        for (voice in active) {
            assertFalse(assistantShowsLatest(ready, voice, true))
            assertEquals(AssistantPrimaryControl(AssistantPrimaryAction.STOP, true), assistantPrimaryControl(ready, voice, false))
        }
    }

    @Test fun userReadingPositionIsNotChangedByFollowingStreamOrPrependingPages() = runBlocking {
        val reading = AssistantReadingPosition()
        reading.following { reading.scrolled(true, true) }
        assertTrue(reading.followsLatest)
        reading.scrolled(true, true)
        assertFalse(reading.followsLatest)
        reading.scrolled(false, true)
        assertFalse(reading.followsLatest)
        reading.readOlder()
        assertFalse(reading.followsLatest)
        reading.latest()
        assertTrue(reading.followsLatest)
        reading.scrolled(true, false)
        assertTrue(reading.followsLatest)
    }

    @Test fun chatsExcludeDeletedAndUnresolvedRecordsWithoutFlatteningMessages() {
        fun chat(id: String, title: Any, deleted: Boolean = false) = JSONObject()
            .put("id", JSONObject(id)).put("title", title).put("deleted", deleted)
        val view = JSONObject().put("conversations", JSONArray(listOf(
            chat(conversation, "Synthetic retained chat"),
            chat("{\"replica\":\"synthetic-chat\",\"sequence\":\"2\"}", "Synthetic deleted chat", true),
            chat("{\"replica\":\"synthetic-chat\",\"sequence\":\"3\"}", JSONObject.NULL))))
            .put("records", JSONArray()).toString()
        assertEquals(listOf("Synthetic retained chat"), assistantChats(view).map { it.title })
    }

    @Test fun prependingPagesRestoresTheExactPromptOrAnswerAnchorNotTheOlderButton() {
        val current = AssistantTurn(conversation, "Synthetic question", "Synthetic answer", false)
        val older = AssistantTurn("{\"replica\":\"synthetic-older\",\"sequence\":\"9\"}", "Synthetic earlier question", "Synthetic earlier answer", false)
        val anchor = "answer:${assistantIdKey(current.id)}"
        assertEquals(2, assistantTurnItemIndex(listOf(current), anchor))
        assertEquals(4, assistantTurnItemIndex(listOf(older, current), anchor))
        assertNull(assistantTurnItemIndex(listOf(older, current), "older"))
        assertNull(assistantTurnItemIndex(listOf(older), anchor))
    }

    @Test fun collidingRequestKeysUseEveryDurableTurnIdWhileUniqueRequestsAndLegacyKeysStayStable() {
        val shared = "synthetic-import-request:51"
        val uniqueRequest = "synthetic-import-request:52"
        val repeated = turn("7").copy(prompt = "Synthetic repeated prompt", answer = "Synthetic repeated answer", requestKey = shared)
        val imported = repeated.copy(id = turnId("30"))
        val unique = repeated.copy(id = turnId("31"), requestKey = uniqueRequest)
        val legacy = repeated.copy(id = turnId("32"), requestKey = null)
        val turns = listOf(repeated, imported, unique, legacy)
        val keys = AssistantTurnItemKeys(turns)
        assertEquals(8, keys.rowKeys.size)
        assertEquals("prompt:${assistantIdKey(repeated.id)}", keys.prompt(repeated))
        assertEquals("answer:${assistantIdKey(imported.id)}", keys.answer(imported))
        assertEquals(assistantPromptItemKey(uniqueRequest), keys.prompt(unique))
        assertEquals(assistantAnswerItemKey(uniqueRequest), keys.answer(unique))
        assertEquals(assistantPromptItemKey(legacy), keys.prompt(legacy))
        assertEquals(assistantAnswerItemKey(legacy), keys.answer(legacy))
        assertEquals(2, keys.index(keys.answer(repeated)))
        assertEquals(4, keys.index(keys.answer(imported)))
        assertNull("An ambiguous request is not an anchor for an arbitrary first turn", keys.index(assistantAnswerItemKey(shared)))
        val advanced = ready.copy(selectedHead = legacy.id)
        val shown = AssistantHistoryPresentation().update(advanced,
            AssistantHistoryState(conversation, advanced.selectedHead, turns), false, null)
        assertEquals("Key disambiguation never collapses or reorders durable turns", turns, shown.turns)
        assertEquals(keys.rowKeys, shown.itemKeys.rowKeys)
    }

    @Test fun aReadingAnchorKeepsItsTurnWhenPagingRevealsARequestKeyCollision() {
        val current = turn("30").copy(requestKey = "synthetic-import-request:51")
        val before = AssistantTurnItemKeys(listOf(current))
        val anchor = before.anchorKey(before.answer(current))
        val older = current.copy(id = turnId("7"))
        val after = AssistantTurnItemKeys(listOf(older, current))
        assertEquals("answer:${assistantIdKey(current.id)}", anchor)
        assertEquals(4, after.index(anchor))
        assertEquals(after.answer(current), anchor)
        assertEquals(4, assistantTurnItemIndex(listOf(older, current), anchor))
    }

    @Test fun importedSameRequestCannotHideALiveReplyAndOnlyItsOwnSettledTurnRetiresTheProjection() {
        val shared = "synthetic-import-request:51"
        val imported = turn("30").copy(requestKey = shared)
        val first = ready.copy(selectedHead = imported.id)
        val history = AssistantHistoryState(conversation, imported.id, listOf(imported))
        val presentation = AssistantHistoryPresentation()
        val prompts = AssistantPromptPresentation()
        presentation.update(first, history, false, null)
        val busy = first.copy(busy = true, clientRequestToken = "synthetic-live-token", providerRequestId = shared,
            provisional = "Synthetic live reply")
        val pending = prompts.project(busy, AssistantVoiceState(), history).single()
        val live = presentation.update(busy, history, false, null)
        assertEquals(busy.provisional, live.reply?.text)
        val liveReply = requireNotNull(live.reply)
        assertFalse(liveReply.key in live.itemKeys.rowKeys)
        assertFalse(pending.key in live.itemKeys.rowKeys)
        assertNull(live.retiredPromptKey)
        val waitingForText = presentation.update(busy.copy(provisional = ""), history, false, null)
        assertNull(waitingForText.reply)
        assertFalse("The live prompt still needs its request key before first text", pending.key in waitingForText.itemKeys.rowKeys)
        presentation.update(busy, history, false, null)
        val committed = busy.copy(busy = false, draft = "", provisional = "", selectedHead = turnId("31"))
        presentation.update(committed, history, false, liveReply.key to 17)
        val incomplete = history.copy(head = committed.selectedHead, turns = listOf(imported.copy(id = turnId("7")), imported))
        val notHandedOff = presentation.update(committed, incomplete, false, null)
        assertNotNull("Matching imported requests are not the witnessed committed operation", notHandedOff.reply)
        assertNull(notHandedOff.retiredPromptKey)
        assertEquals(2, notHandedOff.turns.size)
        assertFalse(notHandedOff.reply!!.key in notHandedOff.itemKeys.rowKeys)
        val saved = turn("31").copy(requestKey = shared)
        val canonical = incomplete.copy(turns = incomplete.turns + saved)
        val handedOff = presentation.update(committed, canonical, false, null)
        assertNull(handedOff.reply)
        assertEquals(canonical.turns, handedOff.turns)
        assertEquals(6, handedOff.itemKeys.rowKeys.size)
        assertEquals(pending.key, handedOff.retiredPromptKey)
        assertTrue(prompts.project(committed, AssistantVoiceState(), canonical).isEmpty())
    }

    @Test fun aStreamingAnchorResolvesToItsExactCommittedTurnWhenCanonicalRequestsCollide() {
        val shared = "synthetic-import-request:51"
        val imported = turn("30").copy(requestKey = shared)
        val first = ready.copy(selectedHead = imported.id)
        val history = AssistantHistoryState(conversation, imported.id, listOf(imported))
        val presentation = AssistantHistoryPresentation()
        presentation.update(first, history, false, null)
        val busy = first.copy(busy = true, clientRequestToken = "synthetic-live-token", providerRequestId = shared,
            provisional = "Synthetic live reply")
        val stream = presentation.update(busy, history, false, null).reply!!
        val committed = busy.copy(busy = false, draft = "", provisional = "", selectedHead = turnId("31"))
        presentation.update(committed, history, false, stream.key to 17)
        val saved = turn("31").copy(requestKey = shared)
        val handedOff = presentation.update(committed, history.copy(head = saved.id, turns = listOf(imported, saved)), false, null)
        assertEquals("answer:${assistantIdKey(saved.id)}", handedOff.itemKeys.answer(saved))
        assertEquals(4, handedOff.scroll?.index)
        assertEquals(17, handedOff.scroll?.offset)
        assertNull(handedOff.reply)
    }

    @Test fun restoredReadingModeDoesNotJumpToLatestUntilExplicitlyRequested() {
        val restored = AssistantReadingPosition(false)
        assertFalse(restored.followsLatest)
        restored.scrolled(false, true)
        assertFalse(restored.followsLatest)
        restored.latest()
        assertTrue(restored.followsLatest)
    }

    @Test fun continuationLabelsUseTheExactNativeHeadAndDoNotSortOrMergeBranches() {
        val operation = JSONObject().put("id", JSONObject(conversation)).put("body", JSONObject().put("prompt", "Synthetic\nbranch prompt"))
        val view = JSONObject().put("records", JSONArray(listOf(JSONObject().put("operation", operation)))).toString()
        assertEquals("Continue: Synthetic branch prompt", assistantBranchLabel(view, conversation))
        assertEquals("Continuation awaiting messages", assistantBranchLabel(view, "{\"replica\":\"synthetic-other\",\"sequence\":\"1\"}"))
    }

    @Test fun provisionalAssistantRequiresSubstantiveBusyTextAndKeepsItUnmodified() {
        for (text in listOf("", " \n\t")) assertNull(assistantProvisionalAnswer(ready.copy(busy = true, provisional = text)))
        val text = "  Synthetic actual answer\n"
        assertEquals(text, assistantProvisionalAnswer(ready.copy(busy = true, provisional = text)))
        assertNull(assistantProvisionalAnswer(ready.copy(provisional = text)))
    }

    @Test fun quietNoMatchIsPresentationOnlyAndOtherActionableErrorsRemainVisible() {
        val voice = dictating(DictationPhase.ERROR, "Synthetic unconfirmed partial").copy(error = "recognition_error_7",
            dictation = DictationState(phase = DictationPhase.ERROR, draft = "Synthetic unconfirmed partial", error = "no_speech"),
            speech = SpeechState(error = "speech_cleanup_failed"))
        assertEquals(listOf("speech_cleanup_failed"), assistantVisibleVoiceErrors(voice))
        assertEquals("recognition_error_7", voice.error)
        assertEquals("Synthetic unconfirmed partial", voice.dictation.draft)
        assertTrue(assistantVisibleVoiceErrors(voice.copy(error = "microphone_permission_required")).contains("microphone_permission_required"))
    }

    @Test fun futureRequestPolicyDefaultsOnAndWebOffDisablesEffortNotOrdinarySend() {
        assertTrue(AssistantState().webEnabled)
        assertTrue(AssistantState().speechNetworkFallback)
        val webOff = ready.copy(webEnabled = false)
        assertFalse(assistantCanSelectSearchMode(webOff, AssistantVoiceState()))
        assertTrue(assistantCanSend(webOff, AssistantVoiceState()))
    }

    @Test fun localStreamCommitAndDelayedHistoryKeepOneReplyAndStableRequestKeysAndReadingAnchor() {
        val presentation = AssistantHistoryPresentation()
        val prompts = AssistantPromptPresentation()
        val first = ready.copy(selectedHead = turnId("30"))
        val history = AssistantHistoryState(conversation, first.selectedHead, listOf(turn("7"), turn("30")))
        val anchor = assistantAnswerItemKey(history.turns.first()) to 21
        presentation.update(first, history, false, anchor)
        val busy = first.copy(busy = true, clientRequestToken = "synthetic-stream-token",
            providerRequestId = "synthetic-request:51", provisional = "Synthetic progressive", firstTextMs = 321L)
        val prompt = prompts.project(busy, AssistantVoiceState(), history).single()
        val streaming = presentation.update(busy, history, false, anchor)
        assertEquals("Synthetic progressive", streaming.reply?.text)
        val completeText = "Synthetic progressive answer"
        val progressed = busy.copy(provisional = completeText)
        assertEquals(completeText, presentation.update(progressed, history, false, anchor).reply?.text)
        val committed = progressed.copy(busy = false, draft = "", provisional = "", selectedHead = turnId("31"))
        val waiting = presentation.update(committed, history.copy(loading = true), false, anchor)
        assertFalse("Retained prefix is display-only, not validated at the new head", waiting.ready)
        assertEquals(history.turns, waiting.turns)
        assertEquals(completeText, waiting.reply?.text)
        assertEquals(streaming.reply?.key, waiting.reply?.key)
        assertEquals(321L, waiting.reply?.firstTextMs)
        assertEquals("Repeated idle redraws while history is pending cannot erase the reply", waiting.reply,
            presentation.update(committed, history.copy(loading = true), false, null).reply)
        assertEquals(listOf(prompt), prompts.project(committed, AssistantVoiceState(), history))
        val saved = AssistantTurn(turnId("31"), first.draft, "Synthetic canonical final answer", false, busy.providerRequestId)
        val canonical = history.copy(head = committed.selectedHead, turns = history.turns + saved)
        val handedOff = presentation.update(committed, canonical, false, null)
        assertTrue(handedOff.ready)
        assertNull("Matching canonical request replaces the projection in the same update", handedOff.reply)
        assertEquals(1, handedOff.turns.count { it.requestKey == busy.providerRequestId })
        assertEquals("Synthetic canonical final answer", handedOff.turns.last().answer)
        assertEquals(waiting.reply?.key, assistantAnswerItemKey(saved))
        assertEquals(prompt.key, assistantPromptItemKey(saved))
        assertTrue(prompts.project(committed, AssistantVoiceState(), canonical).isEmpty())
        assertEquals(2, handedOff.scroll?.index)
        assertEquals(21, handedOff.scroll?.offset)
        assertEquals(6, assistantTurnItemIndex(handedOff.turns, assistantAnswerItemKey(saved)))
    }

    @Test fun equalReplyTextOrWrongHeadCannotRetireThePendingReplyAndForkClearsIt() {
        val presentation = AssistantHistoryPresentation()
        val first = ready.copy(selectedHead = turnId("30"))
        val history = AssistantHistoryState(conversation, first.selectedHead, listOf(turn("30")))
        presentation.update(first, history, true, null)
        val busy = first.copy(busy = true, clientRequestToken = "synthetic-stream-token",
            providerRequestId = "synthetic-request:51", provisional = "Synthetic repeated answer")
        presentation.update(busy, history, true, null)
        val committed = busy.copy(busy = false, draft = "", provisional = "", selectedHead = turnId("31"))
        val wrongRequest = AssistantTurn(turnId("31"), first.draft, busy.provisional, false, "synthetic-request:52")
        assertNotNull(presentation.update(committed, history.copy(head = committed.selectedHead,
            turns = history.turns + wrongRequest), true, null).reply)
        val matching = wrongRequest.copy(requestKey = busy.providerRequestId)
        assertNotNull(presentation.update(committed, history.copy(turns = history.turns + matching), true, null).reply)
        val forked = presentation.update(committed.copy(selectedHead = turnId("40")), history, true, null)
        assertNull(forked.reply)
        assertTrue(forked.turns.isEmpty())
    }

    @Test fun readingTheStreamingReplyRestoresItsRequestKeyAndOffsetAfterDurableHandoff() {
        val presentation = AssistantHistoryPresentation()
        val first = ready.copy(selectedHead = turnId("30"))
        val history = AssistantHistoryState(conversation, first.selectedHead, listOf(turn("30")))
        presentation.update(first, history, false, null)
        val busy = first.copy(busy = true, clientRequestToken = "synthetic-stream-token",
            providerRequestId = "synthetic-request:51", provisional = "Synthetic streamed answer")
        val stream = presentation.update(busy, history, false, null).reply!!
        val committed = busy.copy(busy = false, draft = "", provisional = "", selectedHead = turnId("31"))
        presentation.update(committed, history, false, stream.key to 17)
        val saved = AssistantTurn(turnId("31"), first.draft, busy.provisional, false, busy.providerRequestId)
        val handedOff = presentation.update(committed, history.copy(head = committed.selectedHead,
            turns = history.turns + saved), false, null)
        assertEquals(4, handedOff.scroll?.index)
        assertEquals(17, handedOff.scroll?.offset)
        assertNull(handedOff.reply)
    }

    @Test fun stopFailureBackgroundNewChatAndDifferentRequestFenceTheReplyProjection() {
        val first = ready.copy(selectedHead = turnId("30"))
        val history = AssistantHistoryState(conversation, first.selectedHead, listOf(turn("30")))
        val busy = first.copy(busy = true, clientRequestToken = "synthetic-stream-token",
            providerRequestId = "synthetic-request:51", provisional = "Synthetic streamed answer")
        val committed = busy.copy(busy = false, draft = "", provisional = "", selectedHead = turnId("31"))
        for (invalid in listOf(committed.copy(interrupted = true, error = "interrupted"),
            committed.copy(foreground = false), committed.copy(conversation = null, selectedHead = null),
            committed.copy(branchChoices = listOf(turnId("31"), turnId("40"))),
            committed.copy(clientRequestToken = "synthetic-other-token", providerRequestId = "synthetic-request:52"))) {
            val presentation = AssistantHistoryPresentation()
            presentation.update(first, history, true, null)
            presentation.update(busy, history, true, null)
            assertNull(presentation.update(invalid, history, true, null).reply)
        }
        val stopped = AssistantHistoryPresentation()
        stopped.update(first, history, true, null)
        stopped.update(busy, history, true, null)
        stopped.stopReply()
        assertNull("Queued busy redraw cannot resurrect the stopped owner", stopped.update(busy, history, true, null).reply)
        assertNull(stopped.update(committed, history, true, null).reply)
        val next = committed.copy(busy = true, clientRequestToken = "synthetic-next-token",
            providerRequestId = "synthetic-request:52", provisional = "Synthetic next answer")
        assertEquals(next.provisional, stopped.update(next, history, true, null).reply?.text)
    }

    @Test fun nativeHeadAdvanceHidesUnvalidatedHistoryButRestoresTheSameMessageAndPixelOffset() {
        val requests = mutableListOf<(String?, AssistantState) -> Unit>()
        val history = AssistantHistory({ _, callback -> requests += callback }) {}
        val presentation = AssistantHistoryPresentation()
        val first = ready.copy(selectedHead = turnId("30"))
        history.select(first)
        requests[0](page(first, listOf("7", "30")), first)
        val anchor = "answer:${assistantIdKey(turnId("7"))}" to 21
        assertEquals(2, presentation.update(first, history.snapshot, false, anchor).turns.size)
        val advanced = first.copy(selectedHead = turnId("31"))
        val hidden = presentation.update(advanced, history.snapshot, false, anchor)
        assertFalse(hidden.ready)
        assertTrue(hidden.turns.isEmpty())
        assertNull(hidden.scroll)
        history.select(advanced)
        assertTrue(presentation.update(advanced, history.snapshot, false, null).turns.isEmpty())
        requests[1](page(advanced, listOf("30", "31")), advanced)
        val restored = presentation.update(advanced, history.snapshot, false, null)
        assertEquals(3, restored.turns.size)
        assertEquals(2, restored.scroll?.index)
        assertEquals(21, restored.scroll?.offset)
    }

    @Test fun aDifferentValidatedBranchResetsReadingInsteadOfRestoringAnUnrelatedMessage() {
        val presentation = AssistantHistoryPresentation()
        val first = ready.copy(selectedHead = turnId("30"))
        val original = AssistantHistoryState(conversation, first.selectedHead, listOf(turn("7"), turn("30")))
        val anchor = "answer:${assistantIdKey(turnId("7"))}" to 21
        presentation.update(first, original, false, anchor)
        val fork = first.copy(selectedHead = turnId("40"))
        assertTrue(presentation.update(fork, original.copy(loading = true), false, anchor).turns.isEmpty())
        val reset = presentation.update(fork, AssistantHistoryState(conversation, fork.selectedHead, listOf(turn("2"), turn("40"))), false, null)
        assertNotNull(reset.scroll)
        assertNull(reset.scroll?.index)
        assertEquals(0, reset.scroll?.offset)
    }

    @Test fun partialFinishingFinalSubmittedAndHistoryHandoffKeepOnePromptProjection() {
        val presentation = AssistantPromptPresentation()
        val initial = ready.copy(draft = "", selectedHead = turnId("30"))
        val history = AssistantHistoryState(conversation, initial.selectedHead, listOf(turn("30")))
        val listening = dictating(DictationPhase.LISTENING, "Synthetic partial")
        assertEquals("Synthetic partial", presentation.update(initial, listening, history))
        val finalText = "Synthetic partial completed"
        val finishing = dictating(DictationPhase.FINISHING, finalText)
        assertEquals(finalText, presentation.update(initial, finishing, history))
        val final = dictating(DictationPhase.FINAL, finalText)
        assertEquals(finalText, presentation.update(initial, final, history))
        val confirmed = initial.copy(draft = finalText)
        assertEquals(finalText, presentation.update(confirmed, final, history))
        val submitted = confirmed.copy(busy = true, clientRequestToken = "synthetic-request-1")
        assertEquals(finalText, presentation.update(submitted, final, history))
        val settled = submitted.copy(busy = false, draft = "", selectedHead = turnId("31"))
        assertEquals("Pending prompt survives native settlement while history catches up", finalText,
            presentation.update(settled, final, history.copy(loading = true)))
        val canonical = history.copy(head = settled.selectedHead,
            turns = history.turns + AssistantTurn(turnId("31"), finalText, "Synthetic answer", false))
        assertNull("Canonical turn replaces, rather than duplicates, the pending prompt",
            presentation.update(settled, final, canonical))
        assertNull("Late FINAL from the prior utterance cannot resurrect the sent prompt",
            presentation.update(settled, final, canonical))
    }

    @Test fun repeatedTextIsANewPromptAndFailureRetainsTheUnsentChoice() {
        val presentation = AssistantPromptPresentation()
        val text = "Synthetic repeated question"
        val initial = ready.copy(draft = text, selectedHead = turnId("30"))
        val history = AssistantHistoryState(conversation, initial.selectedHead,
            listOf(AssistantTurn(turnId("30"), text, "Synthetic previous answer", false)))
        assertEquals(text, presentation.update(initial, AssistantVoiceState(), history))
        val busy = initial.copy(busy = true, clientRequestToken = "synthetic-retry-1")
        assertEquals(text, presentation.update(busy, AssistantVoiceState(), history))
        val failed = busy.copy(busy = false, error = "transport", interrupted = true)
        assertEquals(text, presentation.update(failed, AssistantVoiceState(), history))
        assertEquals(text, presentation.update(busy.copy(clientRequestToken = "synthetic-retry-2"), AssistantVoiceState(), history))
    }

    @Test fun headOrConversationChangeCannotImportOldDictationAndNewUtteranceWins() {
        val presentation = AssistantPromptPresentation()
        val initial = ready.copy(draft = "", selectedHead = turnId("30"))
        val history = AssistantHistoryState(conversation, initial.selectedHead, listOf(turn("30")))
        val final = dictating(DictationPhase.FINAL, "Synthetic stale dictation")
        assertNull("Unobserved final speech is not an unsent native draft", presentation.update(initial, final, history))
        presentation.update(initial, final.copy(dictation = final.dictation.copy(phase = DictationPhase.LISTENING)), history)
        val branch = initial.copy(selectedHead = turnId("40"))
        assertNull(presentation.update(branch, final, history))
        assertNull(presentation.update(branch.copy(conversation = turnId("99")), final, history))
        val next = dictating(DictationPhase.LISTENING, "Synthetic new partial", utterance = 2)
        assertEquals("Synthetic new partial", presentation.update(branch, next, history))
    }

    @Test fun aMatchingPromptInOldHistoryDoesNotRetireARequestUntilItsNewHeadIsValidated() {
        val presentation = AssistantPromptPresentation()
        val initial = ready.copy(selectedHead = turnId("30"))
        val history = AssistantHistoryState(conversation, initial.selectedHead,
            listOf(AssistantTurn(turnId("30"), initial.draft, "Synthetic prior answer", false)))
        val busy = initial.copy(busy = true, clientRequestToken = "synthetic-request-3")
        assertEquals(initial.draft, presentation.update(busy, AssistantVoiceState(), history))
        val settled = busy.copy(busy = false, draft = "", selectedHead = turnId("31"))
        assertEquals(initial.draft, presentation.update(settled, AssistantVoiceState(), history))
        assertEquals(initial.draft, presentation.update(settled, AssistantVoiceState(), history.copy(head = settled.selectedHead)))
        assertNull(presentation.update(settled, AssistantVoiceState(), history.copy(head = settled.selectedHead,
            turns = history.turns + turn("31"))))
    }

    @Test fun switchingBranchesDuringDelayedHistoryRetiresTheOldPendingRequest() {
        val presentation = AssistantPromptPresentation()
        val initial = ready.copy(selectedHead = turnId("30"))
        val history = AssistantHistoryState(conversation, initial.selectedHead, listOf(turn("30")))
        val busy = initial.copy(busy = true, clientRequestToken = "synthetic-delayed-request")
        presentation.update(busy, AssistantVoiceState(), history)
        val settled = busy.copy(busy = false, draft = "", selectedHead = turnId("31"))
        assertEquals(initial.draft, presentation.update(settled, AssistantVoiceState(), history))
        val switched = settled.copy(selectedHead = turnId("40"))
        assertNull(presentation.update(switched, AssistantVoiceState(), history))
    }

    @Test fun emptyFollowupAndQuietIdleCannotRetireSubmittedPromptBeforeCanonicalHistory() {
        val presentation = AssistantPromptPresentation()
        val initial = ready.copy(selectedHead = turnId("30"))
        val history = AssistantHistoryState(conversation, initial.selectedHead, listOf(turn("30")))
        presentation.update(initial.copy(busy = true, clientRequestToken = "synthetic-delayed-request"), AssistantVoiceState(), history)
        val settled = initial.copy(draft = "", selectedHead = turnId("31"))
        assertEquals(initial.draft, presentation.update(settled, AssistantVoiceState(), history))
        for (phase in listOf(DictationPhase.CHECKING, DictationPhase.LISTENING)) {
            assertEquals("An empty follow-up is not a replacement turn", initial.draft,
                presentation.update(settled, dictating(phase, "", utterance = 2), history))
        }
        val quietIdle = dictating(DictationPhase.IDLE, "", utterance = 2)
        assertEquals("Quiet no-match retirement leaves the submitted turn visible", initial.draft,
            presentation.update(settled, quietIdle, history))
        val canonical = history.copy(head = settled.selectedHead, turns = history.turns + turn("31"))
        assertNull(presentation.update(settled, quietIdle, canonical))
    }

    @Test fun newWordsCoexistWithSubmittedTurnEvenWhenTextRepeatsAndCanonicalHandoffKeepsNewIdentity() {
        val presentation = AssistantPromptPresentation()
        val initial = ready.copy(selectedHead = turnId("30"))
        val history = AssistantHistoryState(conversation, initial.selectedHead, listOf(turn("30")))
        presentation.project(initial.copy(busy = true, clientRequestToken = "synthetic-first-request"), AssistantVoiceState(), history)
        val settled = initial.copy(draft = "", selectedHead = turnId("31"))
        val submitted = presentation.project(settled, AssistantVoiceState(), history).single()
        val followup = dictating(DictationPhase.LISTENING, initial.draft, utterance = 2)
        val both = presentation.project(settled, followup, history)
        assertEquals(listOf(initial.draft, initial.draft), both.map { it.text })
        assertEquals(submitted, both.first())
        assertNotEquals(both[0].key, both[1].key)
        val canonical = history.copy(head = settled.selectedHead, turns = history.turns + turn("31"))
        assertEquals(listOf(both.last()), presentation.project(settled, followup, canonical))
        assertEquals(listOf(both.last()), presentation.project(settled, followup, canonical))
    }

    @Test fun laterRequestsKeepUnvalidatedEarlierSubmissionsAndCanonicalSubsetsRetireOnlyMatchingTurnIds() {
        val presentation = AssistantPromptPresentation()
        val initial = ready.copy(selectedHead = turnId("30"))
        val history = AssistantHistoryState(conversation, initial.selectedHead, listOf(turn("30")))
        presentation.project(initial.copy(busy = true, clientRequestToken = "synthetic-first-request"), AssistantVoiceState(), history)
        val settled = initial.copy(draft = "", selectedHead = turnId("31"))
        val first = presentation.project(settled, AssistantVoiceState(), history).single()
        val next = settled.copy(draft = "Synthetic next", busy = true, clientRequestToken = "synthetic-next-request")
        val two = presentation.project(next, AssistantVoiceState(), history)
        assertEquals(listOf(first.text, next.draft), two.map { it.text })
        val nextSettled = next.copy(draft = "", busy = false, selectedHead = turnId("32"))
        assertEquals(two, presentation.project(nextSettled, AssistantVoiceState(), history))
        val subset = history.copy(head = nextSettled.selectedHead, turns = listOf(turn("32")))
        assertEquals(listOf(first), presentation.project(nextSettled, AssistantVoiceState(), subset))
        assertTrue(presentation.project(nextSettled, AssistantVoiceState(), subset.copy(turns = listOf(turn("31"), turn("32")))).isEmpty())
    }

    @Test fun firstSendConversationCreationPreservesTheOwnerAndSubmittedIdentityUntilHistoryHandoff() {
        val presentation = AssistantPromptPresentation()
        val initial = ready.copy(conversation = null, draft = "", selectedHead = null)
        val history = AssistantHistoryState()
        val voice = dictating(DictationPhase.LISTENING, "Synthetic first prompt")
        presentation.project(initial, voice, history)
        val busy = initial.copy(draft = voice.dictation.draft, busy = true, clientRequestToken = "synthetic-new-chat-request")
        val submitted = presentation.project(busy, voice, history).single()
        val created = busy.copy(conversation = conversation)
        assertEquals(listOf(submitted), presentation.project(created, voice, history))
        val settled = created.copy(busy = false, draft = "", selectedHead = turnId("31"))
        assertEquals(listOf(submitted), presentation.project(settled, AssistantVoiceState(), history))
        assertEquals(listOf(submitted), presentation.project(settled, dictating(DictationPhase.CHECKING, "", 2), history))
        val canonical = AssistantHistoryState(conversation, settled.selectedHead, listOf(turn("31")))
        assertTrue(presentation.project(settled, AssistantVoiceState(), canonical).isEmpty())
    }

    @Test fun aNewRequestTokenCannotInheritAnOlderPendingPromptAcrossCoalescedSnapshots() {
        val presentation = AssistantPromptPresentation()
        val initial = ready.copy(selectedHead = turnId("30"))
        val history = AssistantHistoryState(conversation, initial.selectedHead, listOf(turn("30")))
        presentation.update(initial.copy(busy = true, clientRequestToken = "synthetic-first-request"), AssistantVoiceState(), history)
        val later = initial.copy(draft = "", selectedHead = turnId("32"), clientRequestToken = "synthetic-second-request")
        assertNull(presentation.update(later, AssistantVoiceState(), history))
    }

    @Test fun clearingAnAcknowledgedFinalDraftDoesNotRestoreTheRetainedRecognitionSnapshot() {
        val presentation = AssistantPromptPresentation()
        val initial = ready.copy(draft = "")
        val history = AssistantHistoryState(conversation)
        val partial = dictating(DictationPhase.LISTENING, "Synthetic words")
        presentation.update(initial, partial, history)
        val final = dictating(DictationPhase.FINAL, "Synthetic final")
        assertEquals("Synthetic final", presentation.update(initial.copy(draft = "Synthetic final"), final, history))
        assertNull(presentation.update(initial, final, history))
    }

    @Test fun durableCompletionWithLocalDraftCleanupFailureStillReplacesThePendingPrompt() {
        val presentation = AssistantPromptPresentation()
        val initial = ready.copy(selectedHead = turnId("30"))
        val history = AssistantHistoryState(conversation, initial.selectedHead, listOf(turn("30")))
        presentation.update(initial.copy(busy = true, clientRequestToken = "synthetic-saved-request"), AssistantVoiceState(), history)
        val saved = initial.copy(selectedHead = turnId("31"), error = "saved_local_state_failed")
        assertEquals(initial.draft, presentation.update(saved, AssistantVoiceState(), history))
        val canonical = history.copy(head = saved.selectedHead, turns = history.turns + turn("31"))
        assertNull(presentation.update(saved, AssistantVoiceState(), canonical))
        assertNull(presentation.update(saved, AssistantVoiceState(), canonical))
        assertEquals("An explicit new draft can repeat the completed prompt", initial.draft,
            presentation.update(saved.copy(error = null), AssistantVoiceState(), canonical))
    }

    @Test fun settingsRouteDetailsDistinguishActiveLastSelectionNotSelectedAndEnginePreference() {
        val unselected = assistantSpeechSettings(AssistantVoiceState(), WearVoicePreferences())
        assertEquals("Dictation · not selected", unselected.dictationStatus)
        assertEquals("Replies · not selected", unselected.replyStatus)
        assertNull(unselected.dictationDetails)
        assertNull(unselected.replyDetails)
        val voice = dictating(DictationPhase.LISTENING, "Synthetic partial").copy(
            dictation = DictationState(phase = DictationPhase.LISTENING, route = RecognitionRoute(
                "com.google.synthetic/Recognizer", "en-GB", "en-US", "en-US")),
            speech = SpeechState(phase = SpeechPhase.SPEAKING, enabled = true,
                route = TtsRoute("com.google.synthetic.tts", "en-GB", "en-US", false, providerIsPreference = true)))
        val active = assistantSpeechSettings(voice, WearVoicePreferences())
        assertEquals("Dictation · active selection", active.dictationStatus)
        assertEquals("Replies · active selection", active.replyStatus)
        assertTrue(active.dictationDetails!!.contains("requested en-GB"))
        assertTrue(active.dictationDetails.contains("reported en-US"))
        assertTrue(active.dictationDetails.contains("actual local/network processing: not reported"))
        assertTrue(active.replyDetails!!.contains("Google preference"))
        val last = assistantSpeechSettings(voice.copy(dictation = voice.dictation.copy(phase = DictationPhase.FINAL),
            speech = voice.speech.copy(phase = SpeechPhase.IDLE, enabled = false)), WearVoicePreferences(false, false))
        assertEquals("Dictation · last selection", last.dictationStatus)
        assertEquals("Replies · last selection · disabled", last.replyStatus)
    }

    private fun dictating(phase: DictationPhase, text: String, utterance: Long = 1) =
        AssistantVoiceState(dictation = DictationState(phase = phase, draft = text, utterance = utterance))

    private fun AssistantPromptPresentation.update(state: AssistantState, voice: AssistantVoiceState, history: AssistantHistoryState): String? {
        val prompts = project(state, voice, history)
        assertTrue("Single-turn cases must never hide extra projections", prompts.size <= 1)
        return prompts.singleOrNull()?.text
    }

    private fun turnId(sequence: String) = "{\"replica\":\"synthetic-turn\",\"sequence\":\"$sequence\"}"
    private fun turn(sequence: String) = AssistantTurn(turnId(sequence), "Synthetic question $sequence", "Synthetic answer $sequence", false)
    private fun page(state: AssistantState, sequences: List<String>) = JSONObject()
        .put("revision", "0").put("conversation", JSONObject(conversation)).put("head", JSONObject(state.selectedHead!!))
        .put("older_before", JSONObject.NULL).put("turns", JSONArray(sequences.map { sequence ->
            JSONObject().put("operation", JSONObject().put("id", JSONObject(turnId(sequence)))
                .put("body", JSONObject().put("kind", "turn_completed").put("conversation", JSONObject(conversation))
                    .put("prompt", "Synthetic question $sequence").put("answer", JSONObject().put("kind", "text").put("text", "Synthetic answer $sequence"))))
        })).toString()
}
