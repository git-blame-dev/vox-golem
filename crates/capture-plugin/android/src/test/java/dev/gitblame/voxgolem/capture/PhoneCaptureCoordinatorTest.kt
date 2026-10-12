package dev.gitblame.voxgolem.capture

import android.net.Uri
import dev.gitblame.voxgolem.assistant.recognition.RecognitionResourceLease
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.FileOutputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.TimeUnit
import java.util.concurrent.CountDownLatch
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicBoolean

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class PhoneCaptureCoordinatorTest {
    private val context: android.content.Context = RuntimeEnvironment.getApplication()
    private val captureId = "123e4567-e89b-42d3-a456-426614174010"
    private val sessionId = "123e4567-e89b-42d3-a456-426614174011"
    private lateinit var repository: CaptureRepository
    private lateinit var speech: FakeSpeech
    private lateinit var exports: MutableList<String>
    private lateinit var exportRetryCounts: MutableList<Int?>
    private lateinit var coordinator: PhoneCaptureCoordinator
    private val receipts = CopyOnWriteArrayList<JSONObject>()
    private val messages = CopyOnWriteArrayList<Triple<String, String, JSONObject>>()
    private val oversizedReceipts = CopyOnWriteArrayList<ByteArray>()
    private val leases = RecognitionResourceLease()

    @Before
    fun setUp() {
        context.deleteDatabase("captures.db")
        context.getDatabasePath("captures.db").parentFile?.resolve("captures.db-wal")?.delete()
        context.getDatabasePath("captures.db").parentFile?.resolve("captures.db-shm")?.delete()
        repository = CaptureRepository(context)
        speech = FakeSpeech()
        exports = CopyOnWriteArrayList()
        exportRetryCounts = CopyOnWriteArrayList()
        coordinator = PhoneCaptureCoordinator(
            context,
            speech,
            { node, path, bytes ->
                val body = JSONObject(String(bytes, Charsets.UTF_8))
                messages += Triple(node, path, body)
                if (path == CaptureProtocol.RECEIPT) receipts += body
            },
            { Uri.parse("content://synthetic-notes/recording.wav") },
            { id ->
                exports += id
                exportRetryCounts += repository.get(id)?.retryCount
                repository.update(id) { it.copy(orgState = OrgState.EXPORTED, exportDestinationId = "synthetic-destination") }
                OrgExportResult(OrgState.EXPORTED, "synthetic-destination")
            },
            replayPacingMillis = 0,
            runtimeReady = { true },
            sendOversizedReceipt = { _, id, bytes ->
                assertEquals(captureId, id)
                oversizedReceipts += bytes
            },
            recognitionLeases = leases,
        )
    }

    @After
    fun tearDown() {
        coordinator.shutdownForTests()
    }

    @Test
    fun foregroundRuntimeRequiresTheExplicitVisibleSetupAction() {
        assertEquals("setup-required", coordinator.start().state)
        assertFalse(PhoneCaptureRuntimeService.isForegroundReady())
    }

    @Test fun newLiveNotesUseTheCapturedLanguageAndExplicitPrivacyPolicyWhileLegacyCallsStayLocalOnly() {
        org.robolectric.Shadows.shadowOf(context as android.app.Application).grantPermissions(android.Manifest.permission.RECORD_AUDIO)
        assertTrue(coordinator.beginLive(captureId, sessionId, "synthetic-node", CaptureFormat(), STAMP,
            requestedLanguage = "en-AU", speechNetworkFallback = false))
        assertEquals("en-AU", speech.requestedLanguage)
        assertFalse(speech.networkFallback)
        assertFalse(coordinator.beginLive(captureId, sessionId, "synthetic-node", CaptureFormat(), STAMP,
            requestedLanguage = "en-AU", speechNetworkFallback = true))
        assertFalse(repository.get(captureId)!!.speechNetworkFallback)
        speech.events!!.onFailure("speech-recognizer-7")
        assertTrue(coordinator.awaitIdle())
        val nextId = "123e4567-e89b-42d3-a456-426614174012"
        val nextSession = "123e4567-e89b-42d3-a456-426614174013"
        assertTrue(coordinator.beginLive(nextId, nextSession, "synthetic-node", CaptureFormat(), STAMP,
            requestedLanguage = "de-DE", speechNetworkFallback = true))
        assertEquals("de-DE", speech.requestedLanguage)
        assertTrue(speech.networkFallback)
        assertEquals("de-DE", repository.get(nextId)?.requestedLanguage)
    }

    @Test fun legacyReplayPolicyIsNotUpgradedByCurrentPhoneAssistantPreferences() {
        dev.gitblame.voxgolem.voice.InstallationPreferences(context).write(
            dev.gitblame.voxgolem.voice.InstallationSettings(speechNetworkFallback = true, speechLanguage = "de-DE"))
        savePeerFixture(savedRecord(writeWav(byteArrayOf(1, 0, 2, 0)), CaptureState.SAVED, RecognitionState.QUEUED))
        coordinator.retryCapture(captureId, "synthetic-node")
        assertTrue(coordinator.awaitIdle())
        assertFalse(speech.networkFallback)
        assertNull(speech.requestedLanguage)
    }

    @Test fun providerReadyAndFailureProjectSafeRevisionedStatusAndOldCallbacksCannotReviveIt() {
        org.robolectric.Shadows.shadowOf(context as android.app.Application).grantPermissions(android.Manifest.permission.RECORD_AUDIO)
        assertTrue(coordinator.beginLive(captureId, sessionId, "synthetic-node", CaptureFormat(), STAMP))
        val events = speech.events!!
        events.onAttemptState("provider-ready")
        assertTrue(coordinator.awaitIdle())
        val ready = repository.get(captureId)!!
        assertEquals(LiveTranscriptionState.PROVIDER_READY, ready.liveTranscriptionState)
        assertTrue(ready.liveStatusRevision > 0)
        assertFalse(ready.canTranscribeLive)
        events.onFailure("speech-recognizer-12")
        assertTrue(coordinator.awaitIdle())
        val failed = repository.get(captureId)!!
        assertEquals(LiveTranscriptionState.UNAVAILABLE, failed.liveTranscriptionState)
        assertEquals("speech-recognizer-12", failed.liveErrorCode)
        assertTrue(failed.liveStatusRevision > ready.liveStatusRevision)
        events.onAttemptState("provider-ready")
        assertTrue(coordinator.awaitIdle())
        assertEquals(failed, repository.get(captureId))
        assertTrue(messages.any { (_, path, body) -> path == CaptureProtocol.READY && body.optLong("statusRevision") > 0 &&
            body.optString("errorCode") == "speech-recognizer-12" })
    }

    @Test fun duplicateStartPausedAfterLookupCannotOverwriteTerminalFailureWithAttemptingReadiness() {
        assertPausedDuplicateCannotReviveTerminalAttempt(accepted = false)
    }

    @Test fun duplicateStartPausedAfterLookupCannotOverwriteTerminalFailureWithAcceptedReadiness() {
        assertPausedDuplicateCannotReviveTerminalAttempt(accepted = true)
    }

    private fun assertPausedDuplicateCannotReviveTerminalAttempt(accepted: Boolean) {
        org.robolectric.Shadows.shadowOf(context as android.app.Application).grantPermissions(android.Manifest.permission.RECORD_AUDIO)
        val firstDeliveryStarted = CountDownLatch(1)
        val releaseDelivery = CountDownLatch(1)
        val duplicateLookedUp = CountDownLatch(1)
        val releaseDuplicate = CountDownLatch(1)
        val delivered = CopyOnWriteArrayList<JSONObject>()
        val isolated = PhoneCaptureCoordinator(context, speech,
            { _, path, bytes ->
                val body = JSONObject(String(bytes, Charsets.UTF_8))
                if (path == CaptureProtocol.READY && body.optString("recognitionStatus") == "attempting" && firstDeliveryStarted.count > 0) {
                    firstDeliveryStarted.countDown()
                    assertTrue("Synthetic live-delivery hold timed out", releaseDelivery.await(5, TimeUnit.SECONDS))
                }
                if (path == CaptureProtocol.READY) delivered += body
            },
            { Uri.parse("content://synthetic-notes/recording.wav") },
            { OrgExportResult(OrgState.EXPORTED, "synthetic-destination") },
            runtimeReady = { true }, recognitionLeases = leases)
        val duplicateWorker = java.util.concurrent.Executors.newSingleThreadExecutor()
        try {
            assertTrue(isolated.beginLive(captureId, sessionId, "synthetic-node", CaptureFormat(), STAMP))
            assertTrue(firstDeliveryStarted.await(2, TimeUnit.SECONDS))
            val originalEvents = speech.events!!
            if (accepted) {
                originalEvents.onHypothesis("Synthetic accepted words", false)
                fenceCoordinatorIo(isolated)
                assertEquals(LiveTranscriptionState.TRANSCRIBING, repository.get(captureId)?.liveTranscriptionState)
            }
            isolated.beforeDuplicateReady = {
                duplicateLookedUp.countDown()
                check(releaseDuplicate.await(5, TimeUnit.SECONDS)) { "Synthetic duplicate hold timed out" }
            }
            val duplicate = duplicateWorker.submit<Boolean> {
                isolated.beginLive(captureId, sessionId, "synthetic-node", CaptureFormat(), STAMP)
            }
            assertTrue(duplicateLookedUp.await(2, TimeUnit.SECONDS))
            originalEvents.onFailure("speech-recognizer-12")
            // Fence the real failure persistence/READY enqueue, while live delivery stays held.
            fenceCoordinatorIo(isolated)
            val failed = repository.get(captureId)!!
            assertEquals(RecognitionState.FAILED, failed.recognitionState)
            assertEquals(LiveTranscriptionState.UNAVAILABLE, failed.liveTranscriptionState)
            assertEquals("speech-recognizer-12", failed.lastErrorCode)
            assertEquals("speech-recognizer-12", failed.liveErrorCode)
            assertFalse(failed.canTranscribeLive)
            releaseDuplicate.countDown()
            assertFalse(duplicate.get(2, TimeUnit.SECONDS))
            assertEquals("Stale duplicate changed the terminal record or its revision", failed, repository.get(captureId))
            releaseDelivery.countDown()
            assertTrue(isolated.awaitIdle())
            assertTrue("Failure READY was suppressed by a stale readiness revision", delivered.any {
                it.optString("recognitionStatus") == "unavailable" && it.optString("errorCode") == "speech-recognizer-12" &&
                    it.optLong("statusRevision") == failed.liveStatusRevision
            })
            assertFalse(delivered.any { it.optLong("statusRevision") > failed.liveStatusRevision })
        } finally {
            releaseDuplicate.countDown()
            releaseDelivery.countDown()
            duplicateWorker.shutdownNow()
            assertTrue(duplicateWorker.awaitTermination(2, TimeUnit.SECONDS))
            isolated.shutdownForTests()
        }
    }

    private fun fenceCoordinatorIo(owner: PhoneCaptureCoordinator) {
        val io = PhoneCaptureCoordinator::class.java.getDeclaredField("io").apply { isAccessible = true }
            .get(owner) as java.util.concurrent.ExecutorService
        io.submit {}.get(2, TimeUnit.SECONDS)
    }

    @Test fun queuedLiveReadinessCanProgressAndAnExplicitReplayUsesAFreshAttemptAfterFailure() {
        org.robolectric.Shadows.shadowOf(context as android.app.Application).grantPermissions(android.Manifest.permission.RECORD_AUDIO)
        val chat = leases.tryAcquire(RecognitionResourceLease.Kind.CHAT)!!
        try {
            assertFalse(coordinator.beginLive(captureId, sessionId, "synthetic-node", CaptureFormat(), STAMP))
            assertEquals(RecognitionState.QUEUED, repository.get(captureId)?.recognitionState)
            assertEquals(LiveTranscriptionState.PENDING, repository.get(captureId)?.liveTranscriptionState)
        } finally { chat.close() }
        assertTrue(coordinator.beginLive(captureId, sessionId, "synthetic-node", CaptureFormat(), STAMP))
        speech.events!!.onAttemptState("provider-ready")
        assertTrue(coordinator.awaitIdle())
        assertEquals(LiveTranscriptionState.PROVIDER_READY, repository.get(captureId)?.liveTranscriptionState)
        speech.events!!.onFailure("speech-recognizer-12")
        assertTrue(coordinator.awaitIdle())
        val saved = savedRecord(writeWav(byteArrayOf(1, 0, 2, 0)), CaptureState.SAVED, RecognitionState.FAILED)
        repository.update(captureId) { saved.copy(recognitionSessionId = it.recognitionSessionId,
            liveTranscriptionState = it.liveTranscriptionState, liveStatusRevision = it.liveStatusRevision,
            liveErrorCode = it.liveErrorCode, retryCount = 5) }
        coordinator.retryCapture(captureId, "synthetic-node")
        assertTrue(coordinator.awaitIdle())
        val retried = repository.get(captureId)!!
        assertEquals(RecognitionState.RUNNING, retried.recognitionState)
        assertTrue(retried.recognitionSessionId != sessionId)
        speech.events!!.onAttemptState("provider-ready")
        assertTrue(coordinator.awaitIdle())
        assertEquals(LiveTranscriptionState.PROVIDER_READY, repository.get(captureId)?.liveTranscriptionState)
        assertNull(repository.get(captureId)?.liveErrorCode)
    }

    @Test fun initialProviderUnavailabilityStillPersistsAndDeliversItsFailureStatus() {
        org.robolectric.Shadows.shadowOf(context as android.app.Application).grantPermissions(android.Manifest.permission.RECORD_AUDIO)
        speech.supportsExternalAudio = false
        assertFalse(coordinator.beginLive(captureId, sessionId, "synthetic-node", CaptureFormat(), STAMP))
        assertTrue(coordinator.awaitIdle())
        val unavailable = repository.get(captureId)!!
        assertEquals(RecognitionState.UNAVAILABLE, unavailable.recognitionState)
        assertEquals(LiveTranscriptionState.UNAVAILABLE, unavailable.liveTranscriptionState)
        assertEquals("external-audio-provider-unavailable", unavailable.liveErrorCode)
        assertFalse(unavailable.canTranscribeLive)
        assertTrue(messages.any { (_, path, body) -> path == CaptureProtocol.READY &&
            body.optString("recognitionStatus") == "unavailable" &&
            body.optString("errorCode") == "external-audio-provider-unavailable" &&
            body.optLong("statusRevision") == unavailable.liveStatusRevision })
    }

    @Test fun nonNoteClipsCannotReplayTransferArchiveOrExportEvenWhenTheyHaveFinalText() {
        for ((index, purpose) in CapturePurpose.entries.filter { it != CapturePurpose.NOTE }.withIndex()) {
            val id = "123e4567-e89b-42d3-a456-42661417404$index"
            val clip = savedRecord(writeWav(byteArrayOf(1, 0)), CaptureState.SAVED, RecognitionState.RUNNING)
                .copy(captureId = id, purpose = purpose, orgState = OrgState.NONE, finalText = "Synthetic chat words")
            repository.save(clip)
            coordinator.retryCapture(id)
            coordinator.retryPendingCaptures()
            assertTrue(coordinator.awaitIdle())
            assertEquals(clip, repository.get(id))
            assertFalse(coordinator.canReceiveArchive(id, "synthetic-node"))
            assertThrows(IllegalArgumentException::class.java) { coordinator.onArchiveReceived(clip, "synthetic-node") }
            assertEquals(OrgState.NONE, OrgExportCoordinator(context).process(id).state)
        }
        assertEquals(0, speech.startCount)
        assertTrue(exports.isEmpty())
        assertTrue(messages.isEmpty())
    }

    @Test fun failedFinalUpdateSettlesTheSessionAndAllowsProbeAndExplicitReplayWithoutRestart() {
        savePeerFixture(savedRecord(writeWav(byteArrayOf(1, 2, 3, 4)), CaptureState.SAVED, RecognitionState.QUEUED))
        coordinator.retryCapture(captureId, "synthetic-node")
        assertTrue(coordinator.awaitIdle())
        val oldEvents = speech.events!!
        rejectFinalUpdates()
        oldEvents.onHypothesis("synthetic unpersisted final", true)
        assertTrue(coordinator.awaitIdle())
        assertTrue(speech.activeSessions.isEmpty())
        assertEquals(RecognitionState.FAILED, repository.get(captureId)?.recognitionState)
        assertEquals("final-transcript-persistence-failed", repository.get(captureId)?.lastErrorCode)
        assertNoPublishedFinal()
        assertTrue("Terminal logical session still blocks probe admission", coordinator.tryBeginRecognitionProbe(sessionId) {})
        coordinator.endRecognitionProbe(sessionId)
        allowFinalUpdates()
        coordinator.retryCapture(captureId, "synthetic-node")
        assertTrue(coordinator.awaitIdle())
        assertEquals(2, speech.startCount)
        val replacement = speech.events!!
        oldEvents.onFailure("synthetic late failure")
        oldEvents.onHypothesis("synthetic stale final", true)
        assertTrue(coordinator.awaitIdle())
        assertTrue(replacement.isCurrent())
        replacement.onHypothesis("synthetic persisted retry", true)
        assertTrue(coordinator.awaitIdle())
        assertEquals("synthetic persisted retry", repository.get(captureId)?.finalText)
    }

    @Test fun failedFinalReadAndFailedAttentionWriteStillAllowExplicitReplayAfterStorageRecovery() {
        savePeerFixture(savedRecord(writeWav(byteArrayOf(1, 2, 3, 4)), CaptureState.SAVED, RecognitionState.QUEUED))
        coordinator.retryCapture(captureId, "synthetic-node")
        assertTrue(coordinator.awaitIdle())
        val originalBody = replaceCaptureBodyWithUnreadableJson(captureId)
        speech.events!!.onHypothesis("synthetic unreadable-storage final", true)
        assertTrue(coordinator.awaitIdle())
        assertTrue(coordinator.tryBeginRecognitionProbe(sessionId) {})
        coordinator.endRecognitionProbe(sessionId)
        assertNoPublishedFinal()
        restoreCaptureBody(captureId, originalBody)
        coordinator.retryCapture(captureId, "synthetic-node")
        assertTrue(coordinator.awaitIdle())
        assertEquals(2, speech.startCount)
        speech.events!!.onHypothesis("synthetic recovered storage final", true)
        assertTrue(coordinator.awaitIdle())
        assertEquals("synthetic recovered storage final", repository.get(captureId)?.finalText)
    }

    @Test fun failedLiveFinalStillClosesItsExactInputAndDrainsPendingVerifiedAudioToTheSource() {
        org.robolectric.Shadows.shadowOf(RuntimeEnvironment.getApplication()).grantPermissions(android.Manifest.permission.RECORD_AUDIO)
        val source = "synthetic-original-node"
        var closed = 0
        assertTrue(coordinator.registerLiveInput(captureId, sessionId, source) { closed++ })
        coordinator.beginLive(captureId, sessionId, source, CaptureFormat(), STAMP)
        val pending = savedRecord(writeWav(byteArrayOf(1, 2, 3, 4)), CaptureState.INTERRUPTED, RecognitionState.QUEUED)
        coordinator.onArchiveReceived(pending, source)
        assertNull(repository.get(captureId)?.sha256)
        rejectFinalUpdates()
        speech.events!!.onHypothesis("synthetic pending-archive unpersisted final", true)
        assertTrue(coordinator.awaitIdle())
        assertEquals(1, closed)
        val stored = repository.get(captureId)!!
        assertEquals(CaptureState.INTERRUPTED, stored.captureState)
        assertEquals(pending.sha256, stored.sha256)
        assertEquals(ArchiveState.VERIFIED, stored.archiveState)
        assertNull(stored.finalText)
        assertTrue(exports.isEmpty())
        assertNoPublishedFinal()
        assertTrue(messages.any { (node, path, body) -> node == source && path == CaptureProtocol.RECEIPT &&
            body.optString("stage") == "PHONE_ARCHIVED" && body.optString("archiveHash") == pending.sha256 })
        assertTrue(coordinator.tryBeginRecognitionProbe(sessionId) {})
        coordinator.endRecognitionProbe(sessionId)
        allowFinalUpdates()
    }

    @Test fun failedFinalPersistenceDoesNotReleaseAQuarantinedSdkLease() {
        savePeerFixture(savedRecord(writeWav(byteArrayOf(1, 2, 3, 4)), CaptureState.SAVED, RecognitionState.QUEUED).copy(retryCount = 5))
        coordinator.retryCapture(captureId, "synthetic-node")
        assertTrue(coordinator.awaitIdle())
        val held = leases.current()!!
        speech.onCancel = { error("Synthetic SDK destroy failure") }
        rejectFinalUpdates()
        speech.events!!.onHypothesis("synthetic quarantined final", true)
        assertTrue(coordinator.awaitIdle())
        assertSame(held, leases.current())
        assertFalse(coordinator.tryBeginRecognitionProbe(sessionId) {})
        assertNoPublishedFinal()
        speech.onCancel = null
        allowFinalUpdates()
        assertTrue(coordinator.retryFailedRetirements())
        assertTrue(coordinator.awaitIdle())
        assertTrue(coordinator.tryBeginRecognitionProbe(sessionId) {})
        coordinator.endRecognitionProbe(sessionId)
    }

    @Test fun pendingVerifiedArchiveSurvivesContinuedStorageFailureUntilExplicitRetry() {
        org.robolectric.Shadows.shadowOf(RuntimeEnvironment.getApplication()).grantPermissions(android.Manifest.permission.RECORD_AUDIO)
        coordinator.beginLive(captureId, sessionId, "synthetic-node", CaptureFormat(), STAMP)
        val pending = savedRecord(writeWav(byteArrayOf(1, 2, 3, 4)), CaptureState.INTERRUPTED, RecognitionState.QUEUED)
        coordinator.onArchiveReceived(pending, "synthetic-node")
        context.openOrCreateDatabase("captures.db", android.content.Context.MODE_PRIVATE, null).use {
            it.execSQL("CREATE TRIGGER synthetic_capture_write_failure BEFORE UPDATE ON captures " +
                "BEGIN SELECT RAISE(ABORT, 'Synthetic continued storage failure'); END")
        }
        speech.events!!.onHypothesis("synthetic storage-outage final", true)
        assertTrue(coordinator.awaitIdle())
        assertTrue(coordinator.tryBeginRecognitionProbe(sessionId) {})
        coordinator.endRecognitionProbe(sessionId)
        assertNoPublishedFinal()
        assertNull(repository.get(captureId)?.sha256)
        context.openOrCreateDatabase("captures.db", android.content.Context.MODE_PRIVATE, null).use {
            it.execSQL("DROP TRIGGER synthetic_capture_write_failure")
        }
        coordinator.retryCapture(captureId, "synthetic-node")
        assertTrue(coordinator.awaitIdle())
        assertEquals(pending.sha256, repository.get(captureId)?.sha256)
        assertEquals(ArchiveState.VERIFIED, repository.get(captureId)?.archiveState)
        assertEquals(CaptureState.INTERRUPTED, repository.get(captureId)?.captureState)
        assertNoPublishedFinal()
    }

    @Test fun delayedFailedFinalSettlementCannotRemoveASameIdReplacementOrCloseItsInput() {
        org.robolectric.Shadows.shadowOf(RuntimeEnvironment.getApplication()).grantPermissions(android.Manifest.permission.RECORD_AUDIO)
        coordinator.beginLive(captureId, sessionId, "synthetic-old-node", CaptureFormat(), STAMP)
        repository.update(captureId) { it.copy(retryCount = 5) }
        val oldEvents = speech.events!!
        val io = PhoneCaptureCoordinator::class.java.getDeclaredField("io").apply { isAccessible = true }
            .get(coordinator) as java.util.concurrent.ExecutorService
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        io.execute { entered.countDown(); assertTrue(release.await(5, TimeUnit.SECONDS)) }
        assertTrue(entered.await(2, TimeUnit.SECONDS))
        val replacementId = "123e4567-e89b-42d3-a456-426614174039"
        var replacementClosed = false
        try {
            oldEvents.onHypothesis("synthetic delayed final", true)
            coordinator.onArchiveReceived(savedRecord(writeWav(byteArrayOf(1, 2, 3, 4)), CaptureState.INTERRUPTED,
                RecognitionState.QUEUED), "synthetic-old-node")
            assertTrue(coordinator.registerLiveInput(replacementId, sessionId, "synthetic-new-node") { replacementClosed = true })
            coordinator.beginLive(replacementId, sessionId, "synthetic-new-node", CaptureFormat(), STAMP)
            val replacement = speech.events!!
            val held = leases.current()!!
            val originalBody = replaceCaptureBodyWithUnreadableJson(captureId)
            release.countDown()
            assertTrue(coordinator.awaitIdle())
            restoreCaptureBody(captureId, originalBody)
            oldEvents.onFailure("synthetic delayed failure")
            assertTrue(coordinator.awaitIdle())
            assertTrue(replacement.isCurrent())
            assertSame(held, leases.current())
            assertFalse(replacementClosed)
            assertNoPublishedFinal()
            replacement.onHypothesis("synthetic replacement final", true)
            assertTrue(coordinator.awaitIdle())
            assertEquals("synthetic replacement final", repository.get(replacementId)?.finalText)
            assertTrue(replacementClosed)
        } finally { release.countDown() }
    }

    private fun rejectFinalUpdates() = context.openOrCreateDatabase("captures.db", android.content.Context.MODE_PRIVATE, null).use { database ->
        database.execSQL("CREATE TRIGGER synthetic_final_update_failure BEFORE UPDATE ON captures " +
            "WHEN NEW.body LIKE '%\"recognitionState\":\"FINAL\"%' " +
            "BEGIN SELECT RAISE(ABORT, 'Synthetic final metadata write failure'); END")
    }

    private fun allowFinalUpdates() = context.openOrCreateDatabase("captures.db", android.content.Context.MODE_PRIVATE, null).use {
        it.execSQL("DROP TRIGGER synthetic_final_update_failure")
    }

    private fun replaceCaptureBodyWithUnreadableJson(id: String): String =
        context.openOrCreateDatabase("captures.db", android.content.Context.MODE_PRIVATE, null).use { database ->
            val body = database.rawQuery("SELECT body FROM captures WHERE capture_id = ?", arrayOf(id)).use { cursor ->
                assertTrue(cursor.moveToFirst()); cursor.getString(0)
            }
            database.execSQL("UPDATE captures SET body = ? WHERE capture_id = ?", arrayOf("synthetic-invalid-json", id))
            body
        }

    private fun restoreCaptureBody(id: String, body: String) =
        context.openOrCreateDatabase("captures.db", android.content.Context.MODE_PRIVATE, null).use {
            it.execSQL("UPDATE captures SET body = ? WHERE capture_id = ?", arrayOf(body, id))
        }

    private fun assertNoPublishedFinal() {
        assertTrue(messages.none { it.second == CaptureProtocol.FINAL })
        assertTrue(receipts.none { it.optString("recognitionState") == "FINAL" || it.has("finalText") })
        assertTrue(exports.isEmpty())
    }

    @Test fun chatAdmissionDefersReplayAndLiveWithoutCancellingChat() {
        org.robolectric.Shadows.shadowOf(RuntimeEnvironment.getApplication()).grantPermissions(android.Manifest.permission.RECORD_AUDIO)
        val chat = leases.tryAcquire(RecognitionResourceLease.Kind.CHAT)!!
        try {
            val wav = writeWav(byteArrayOf(1, 2, 3, 4))
            savePeerFixture(savedRecord(wav, CaptureState.SAVED, RecognitionState.QUEUED))
            coordinator.retryCapture(captureId); assertTrue(coordinator.awaitIdle())
            coordinator.beginLive("123e4567-e89b-42d3-a456-426614174029", sessionId,
                "synthetic-node", CaptureFormat(), "2026-10-06T10:00:00Z")
            assertEquals(0, speech.startCount); assertSame(chat, leases.current())
            assertFalse(coordinator.tryBeginRecognitionProbe(sessionId) {})
        } finally { chat.close() }
    }
    @Test fun activeLiveAndProbeExcludeChatAndReleaseOnlyTheirExactOwner() {
        assertTrue(coordinator.tryBeginRecognitionProbe(sessionId) {})
        assertNull(leases.tryAcquire(RecognitionResourceLease.Kind.CHAT))
        coordinator.endRecognitionProbe(sessionId)
        val chat = leases.tryAcquire(RecognitionResourceLease.Kind.CHAT)!!
        coordinator.endRecognitionProbe(sessionId); assertSame(chat, leases.current()); chat.close()
        org.robolectric.Shadows.shadowOf(RuntimeEnvironment.getApplication()).grantPermissions(android.Manifest.permission.RECORD_AUDIO)
        coordinator.beginLive(captureId, sessionId, "synthetic-node", CaptureFormat(), "2026-10-06T10:00:00Z")
        assertNull(leases.tryAcquire(RecognitionResourceLease.Kind.CHAT))
        val oldEvents = speech.events!!
        oldEvents.onFailure("synthetic-failure"); assertTrue(coordinator.awaitIdle())
        val next = leases.tryAcquire(RecognitionResourceLease.Kind.CHAT)!!
        oldEvents.onHypothesis("Synthetic stale final", true); oldEvents.onFailure("synthetic-stale")
        assertTrue(coordinator.awaitIdle()); assertSame(next, leases.current()); next.close()
    }
    @Test fun capturePreemptionCancelsWithOldLeaseThenHandsOffWithoutChatAdmissionGap() {
        org.robolectric.Shadows.shadowOf(RuntimeEnvironment.getApplication()).grantPermissions(android.Manifest.permission.RECORD_AUDIO)
        savePeerFixture(savedRecord(writeWav(byteArrayOf(1, 2, 3, 4)), CaptureState.SAVED, RecognitionState.QUEUED))
        coordinator.retryCapture(captureId); assertTrue(coordinator.awaitIdle())
        speech.onCancel = {
            assertEquals(RecognitionResourceLease.Kind.CAPTURE_REPLAY, leases.current()?.kind)
            assertNull(leases.tryAcquire(RecognitionResourceLease.Kind.CHAT))
        }
        coordinator.beginLive("123e4567-e89b-42d3-a456-426614174029", sessionId,
            "synthetic-node", CaptureFormat(), "2026-10-06T10:00:00Z")
        speech.onCancel = null
        assertEquals(2, speech.startCount); assertEquals(RecognitionResourceLease.Kind.CAPTURE_LIVE, leases.current()?.kind)
    }
    @Test fun releasingChatReschedulesDeferredReplayWithoutAnotherUserRetry() {
        val chat = leases.tryAcquire(RecognitionResourceLease.Kind.CHAT)!!
        savePeerFixture(savedRecord(writeWav(byteArrayOf(1, 2, 3, 4)), CaptureState.SAVED, RecognitionState.QUEUED))
        coordinator.retryCapture(captureId); assertTrue(coordinator.awaitIdle()); assertEquals(0, speech.startCount)
        chat.close(); assertTrue(coordinator.awaitIdle())
        assertEquals(1, speech.startCount); assertEquals(RecognitionResourceLease.Kind.CAPTURE_REPLAY, leases.current()?.kind)
    }
    @Test fun failedNativeRetirementKeepsAdmissionAndCannotHandOffToChatOrAnotherLiveOwner() {
        org.robolectric.Shadows.shadowOf(RuntimeEnvironment.getApplication()).grantPermissions(android.Manifest.permission.RECORD_AUDIO)
        coordinator.beginLive(captureId, sessionId, "synthetic-node", CaptureFormat(), STAMP)
        val held = leases.current()!!
        speech.onCancel = { error("Synthetic native retirement failure") }
        speech.events!!.onFailure("synthetic-failure"); assertTrue(coordinator.awaitIdle())
        assertSame(held, leases.current()); assertNull(leases.tryAcquire(RecognitionResourceLease.Kind.CHAT))
        coordinator.beginLive("123e4567-e89b-42d3-a456-426614174029", "123e4567-e89b-42d3-a456-426614174030",
            "synthetic-node", CaptureFormat(), STAMP)
        assertEquals(1, speech.startCount)
        speech.onCancel = null; speech.cancel(sessionId); held.close()
    }

    @Test
    fun runtimeStoppedDuringCapabilitiesPreventsLiveAdmission() {
        org.robolectric.Shadows.shadowOf(RuntimeEnvironment.getApplication()).grantPermissions(android.Manifest.permission.RECORD_AUDIO)
        speech.beforeCapabilities = { coordinator.onRuntimeStopped() }
        coordinator.beginLive(captureId, sessionId, "synthetic-node", CaptureFormat(), "2026-10-06T10:00:00Z")
        assertEquals(0, speech.startCount)
        assertEquals(RecognitionState.QUEUED, repository.get(captureId)?.recognitionState)
    }

    @Test
    fun runtimeStoppedDuringCapabilitiesPreventsReplayAdmission() {
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        savePeerFixture(savedRecord(wav, CaptureState.SAVED, RecognitionState.QUEUED))
        speech.beforeCapabilities = { coordinator.onRuntimeStopped() }
        coordinator.retryCapture(captureId)
        assertTrue(coordinator.awaitIdle())
        assertEquals(0, speech.startCount)
        assertEquals(RecognitionState.QUEUED, repository.get(captureId)?.recognitionState)
    }

    @Test
    fun runtimeShutdownCancelsExclusiveProbeWithoutHoldingAdmissionLock() {
        var cancelled = false
        assertTrue(coordinator.tryBeginRecognitionProbe(sessionId) {
            coordinator.endRecognitionProbe(sessionId)
            cancelled = true
        })
        coordinator.onRuntimeStopped()
        assertTrue(cancelled)
        assertFalse(coordinator.tryBeginRecognitionProbe(sessionId) {})
        coordinator.onRuntimeStarted()
        assertTrue(coordinator.tryBeginRecognitionProbe(sessionId) {})
        coordinator.endRecognitionProbe(sessionId)
    }

    @Test
    fun replayPreemptionDoesNotHoldAdmissionLockWhileCancellingNativeOwner() {
        org.robolectric.Shadows.shadowOf(RuntimeEnvironment.getApplication()).grantPermissions(android.Manifest.permission.RECORD_AUDIO)
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        savePeerFixture(savedRecord(wav, CaptureState.SAVED, RecognitionState.QUEUED))
        coordinator.retryCapture(captureId)
        assertTrue(coordinator.awaitIdle())
        val first = AtomicBoolean(true)
        val retired = CountDownLatch(1)
        val didNotBlock = AtomicBoolean(false)
        speech.onCancel = {
            if (first.compareAndSet(true, false)) {
                Thread { coordinator.onRuntimeStopped(); retired.countDown() }.start()
                didNotBlock.set(retired.await(2, TimeUnit.SECONDS))
            }
        }
        coordinator.beginLive("123e4567-e89b-42d3-a456-426614174029", sessionId,
            "synthetic-node", CaptureFormat(), "2026-10-06T10:00:00Z")
        assertTrue("Native cancellation held the admission lock", didNotBlock.get())
        assertTrue(coordinator.awaitIdle())
        assertEquals(1, speech.startCount)
        assertTrue(speech.activeSessions.isEmpty())
    }

    @Test
    fun liveInputCleanupRequiresTheRegisteredOwnerBeforeAndAfterReplacement() {
        val first: () -> Unit = {}
        val replacement: () -> Unit = {}
        assertTrue(coordinator.registerLiveInput(captureId, sessionId, "synthetic-node", first))
        coordinator.releaseLiveInput(captureId, sessionId, "synthetic-node", replacement)
        assertFalse(coordinator.registerLiveInput(captureId, sessionId, "synthetic-node", replacement))
        coordinator.releaseLiveInput(captureId, sessionId, "synthetic-node", first)
        assertTrue(coordinator.registerLiveInput(captureId, sessionId, "synthetic-node", replacement))
        coordinator.releaseLiveInput(captureId, sessionId, "synthetic-node", first)
        assertFalse(coordinator.registerLiveInput(captureId, sessionId, "synthetic-node", first))
        coordinator.releaseLiveInput(captureId, sessionId, "synthetic-node", replacement)
        assertTrue(coordinator.registerLiveInput(captureId, sessionId, "synthetic-node", first))
    }

    @Test
    fun blockedSavedReceiptDoesNotDelayLiveTextAndEmptyHypothesisIsNotAcceptance() {
        org.robolectric.Shadows.shadowOf(RuntimeEnvironment.getApplication()).grantPermissions(android.Manifest.permission.RECORD_AUDIO)
        val receiptStarted = CountDownLatch(1)
        val releaseReceipt = CountDownLatch(1)
        val emptyDelivered = CountDownLatch(1)
        val textDelivered = CountDownLatch(1)
        val ready = CopyOnWriteArrayList<JSONObject>()
        val liveId = "123e4567-e89b-42d3-a456-426614174022"
        val liveSession = "123e4567-e89b-42d3-a456-426614174023"
        val isolated = PhoneCaptureCoordinator(context, speech, { _, path, bytes ->
            val body = JSONObject(String(bytes, Charsets.UTF_8))
            when (path) {
                CaptureProtocol.RECEIPT -> { receiptStarted.countDown(); releaseReceipt.await(5, TimeUnit.SECONDS) }
                CaptureProtocol.READY -> ready += body
                CaptureProtocol.PARTIAL -> if (body.optString("fullText").isBlank()) emptyDelivered.countDown() else textDelivered.countDown()
            }
        }, { Uri.parse("content://synthetic-notes/recording.wav") },
            { OrgExportResult(OrgState.EXPORTED, "synthetic-destination") }, runtimeReady = { true })
        try {
            val wav = writeWav(byteArrayOf(1, 2, 3, 4))
            savePeerFixture(savedRecord(wav, CaptureState.SAVED, RecognitionState.FINAL).copy(finalText = "Synthetic saved text"))
            isolated.respondStatus(captureId, "synthetic-node")
            assertTrue(receiptStarted.await(2, TimeUnit.SECONDS))
            isolated.beginLive(liveId, liveSession, "synthetic-node", CaptureFormat(), "2026-10-06T10:00:00Z")
            speech.events!!.onHypothesis("", false)
            assertTrue(emptyDelivered.await(2, TimeUnit.SECONDS))
            assertFalse(ready.any { it.optBoolean("canTranscribeLive") })
            speech.events!!.onHypothesis("Synthetic live words", false)
            assertTrue("A saved receipt held up live text", textDelivered.await(2, TimeUnit.SECONDS))
            assertEquals("Synthetic live words", repository.get(liveId)?.previewText)
            assertTrue(ready.any { it.optBoolean("canTranscribeLive") })
            assertTrue(wav.isFile)
        } finally {
            releaseReceipt.countDown()
            isolated.shutdownForTests()
        }
    }

    @Test
    fun retryReplaysOnlyValidatedWavDataChunkAndPersistsFullFinalHypothesis() {
        val pcm = byteArrayOf(0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x08)
        val wav = writeWav(pcm, includeAncillaryChunk = true)
        savePeerFixture(savedRecord(wav, captureState = CaptureState.SAVED, recognitionState = RecognitionState.QUEUED))

        coordinator.retryCapture(captureId)
        assertTrue(coordinator.awaitIdle())
        assertTrue("replay did not finish: ${repository.get(captureId)}", speech.finished)
        assertArrayEquals(pcm, speech.received.toByteArray())
        assertEquals(RecognitionState.RUNNING, repository.get(captureId)?.recognitionState)

        speech.events!!.onHypothesis("synthetic replay transcript", true)
        assertTrue(coordinator.awaitIdle())
        val result = repository.get(captureId)!!
        assertEquals(RecognitionState.FINAL, result.recognitionState)
        assertEquals("synthetic replay transcript", result.finalText)
        assertEquals(1L, result.transcriptVersion)
        assertEquals(listOf(captureId), exports)
    }

    @Test
    fun emptyPartialPreservesLastDurableDraftWithoutExportingIt() {
        val pcm = byteArrayOf(1, 2, 3, 4)
        val wav = writeWav(pcm)
        savePeerFixture(savedRecord(wav, CaptureState.SAVED, RecognitionState.QUEUED))
        coordinator.retryCapture(captureId)
        assertTrue(coordinator.awaitIdle())
        speech.events!!.onHypothesis("synthetic visible draft", false)
        assertTrue(coordinator.awaitIdle())
        speech.events!!.onHypothesis("  ", false)
        assertTrue(coordinator.awaitIdle())

        val restored = CaptureRepository(context).get(captureId)!!
        assertEquals("synthetic visible draft", restored.previewText)
        assertEquals(null, restored.finalText)
        assertEquals(RecognitionState.RUNNING, restored.recognitionState)
        assertEquals(2L, restored.lastRevision)
        assertEquals(0L, restored.transcriptVersion)
        assertTrue(exports.isEmpty())
        WavPcmReader(wav, restored.format).use { reader ->
            assertArrayEquals(pcm, reader.read())
            assertEquals(null, reader.read())
        }
    }

    @Test
    fun queuedLatestNonemptyDraftSurvivesEmptyPartialAndNoSpeechFinal() {
        org.robolectric.Shadows.shadowOf(RuntimeEnvironment.getApplication()).grantPermissions(android.Manifest.permission.RECORD_AUDIO)
        val blocked = CountDownLatch(1)
        val release = CountDownLatch(1)
        val firstReady = AtomicBoolean(true)
        val delivered = CopyOnWriteArrayList<JSONObject>()
        val isolated = PhoneCaptureCoordinator(context, speech, { _, path, bytes ->
            if (path == CaptureProtocol.READY && firstReady.compareAndSet(true, false)) {
                blocked.countDown()
                release.await(5, TimeUnit.SECONDS)
            }
            if (path == CaptureProtocol.PARTIAL || path == CaptureProtocol.FINAL) delivered += JSONObject(String(bytes, Charsets.UTF_8))
        }, { Uri.parse("content://synthetic-notes/recording.wav") },
            { OrgExportResult(OrgState.EXPORTED, "synthetic-destination") }, runtimeReady = { true })
        try {
            isolated.beginLive(captureId, sessionId, "synthetic-node", CaptureFormat(), "2026-10-06T10:00:00Z")
            val inputOwner: () -> Unit = {}
            assertTrue(isolated.registerLiveInput(captureId, sessionId, "synthetic-node", inputOwner))
            assertTrue(blocked.await(2, TimeUnit.SECONDS))
            speech.events!!.onHypothesis("Synthetic superseded words", false)
            speech.events!!.onHypothesis("Synthetic latest draft", false)
            fenceCoordinatorIo(isolated)
            val oversizedDraft = "Synthetic oversized draft ".repeat(2_000)
            speech.events!!.onHypothesis(oversizedDraft, false)
            speech.events!!.onHypothesis("", false)
            isolated.finishLive(captureId, sessionId, "synthetic-node", inputOwner, 0)
            speech.events!!.onHypothesis("", true)
            val stateIo = PhoneCaptureCoordinator::class.java.getDeclaredField("io").apply { isAccessible = true }
                .get(isolated) as java.util.concurrent.ExecutorService
            stateIo.submit {}.get(2, TimeUnit.SECONDS)
            release.countDown()
            assertTrue(isolated.awaitIdle())
            assertFalse(delivered.any { it.optString("fullText") == "Synthetic superseded words" })
            assertTrue(delivered.any { it.optString("fullText") == "Synthetic latest draft" })
            assertEquals(RecognitionState.NO_SPEECH, repository.get(captureId)?.recognitionState)
            assertEquals(null, repository.get(captureId)?.finalText)
            assertEquals(oversizedDraft.length, repository.get(captureId)?.previewText?.length)
            assertEquals(oversizedDraft, repository.get(captureId)?.previewText)
            assertEquals(0L, repository.get(captureId)?.transcriptVersion)
            assertEquals(LiveTranscriptionState.UNAVAILABLE, repository.get(captureId)?.liveTranscriptionState)
            assertFalse(repository.get(captureId)!!.canTranscribeLive)
            assertTrue(exports.isEmpty())
        } finally { release.countDown(); isolated.shutdownForTests() }
    }

    @Test fun blankFinalRetainsQueuedLatestDraftWhenAllPartialPersistenceIsFencedAfterTermination() {
        val pcm = byteArrayOf(1, 0, 2, 0)
        val wav = writeWav(pcm)
        savePeerFixture(savedRecord(wav, CaptureState.SAVED, RecognitionState.QUEUED))
        coordinator.retryCapture(captureId, "synthetic-node")
        assertTrue(coordinator.awaitIdle())
        val io = PhoneCaptureCoordinator::class.java.getDeclaredField("io").apply { isAccessible = true }
            .get(coordinator) as java.util.concurrent.ExecutorService
        val persistenceHeld = CountDownLatch(1)
        val releasePersistence = CountDownLatch(1)
        val hold = io.submit {
            persistenceHeld.countDown()
            check(releasePersistence.await(5, TimeUnit.SECONDS)) { "Synthetic persistence hold timed out" }
        }
        try {
            assertTrue(persistenceHeld.await(2, TimeUnit.SECONDS))
            val latest = "Synthetic oversized draft ".repeat(2_000)
            speech.events!!.onHypothesis(latest, false)
            speech.events!!.onHypothesis("  ", false)
            speech.events!!.onHypothesis("", true)
            releasePersistence.countDown()
            hold.get(2, TimeUnit.SECONDS)
            assertTrue(coordinator.awaitIdle())
            val retained = repository.get(captureId)!!
            assertEquals(latest, retained.previewText)
            assertNull(retained.finalText)
            assertEquals(RecognitionState.NO_SPEECH, retained.recognitionState)
            assertEquals(LiveTranscriptionState.UNAVAILABLE, retained.liveTranscriptionState)
            assertFalse(retained.canTranscribeLive)
            assertEquals(3L, retained.lastRevision)
            assertEquals(0L, retained.transcriptVersion)
            assertTrue("Queued partials revived a terminal attempt", messages.none { it.second == CaptureProtocol.PARTIAL })
            assertTrue(receipts.any { it.optString("recognitionState") == "NO_SPEECH" })
            assertTrue(receipts.none { it.has("finalText") })
            assertTrue(exports.isEmpty())
            WavPcmReader(wav, retained.format).use { reader -> assertArrayEquals(pcm, reader.read()) }
        } finally {
            releasePersistence.countDown()
            hold.get(2, TimeUnit.SECONDS)
        }
    }

    @Test
    fun replayPacingMatchesActualPcmDurationAcrossSupportedRates() {
        assertEquals(100L, PhoneCaptureCoordinator.replayChunkDelayMillis(3_200, CaptureFormat(), 100))
        assertEquals(66L, PhoneCaptureCoordinator.replayChunkDelayMillis(3_200, CaptureFormat(sampleRate = 24_000), 100))
        assertEquals(33L, PhoneCaptureCoordinator.replayChunkDelayMillis(3_200, CaptureFormat(sampleRate = 48_000), 100))
        assertEquals(16L, PhoneCaptureCoordinator.replayChunkDelayMillis(1_600, CaptureFormat(sampleRate = 48_000), 100))
        assertEquals(0L, PhoneCaptureCoordinator.replayChunkDelayMillis(3_200, CaptureFormat(sampleRate = 48_000), 0))
    }

    @Test
    fun emptyFinalKeepsDraftAccessibleButNoSpeechIsNotAFinalTranscriptOrExport() {
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        savePeerFixture(savedRecord(wav, CaptureState.SAVED, RecognitionState.QUEUED))
        coordinator.retryCapture(captureId)
        assertTrue(coordinator.awaitIdle())
        speech.events!!.onHypothesis("synthetic visible draft", false)
        assertTrue(coordinator.awaitIdle())
        speech.events!!.onHypothesis("", true)
        assertTrue(coordinator.awaitIdle())
        coordinator.respondStatus(captureId, "synthetic-node")
        repeat(2) { assertTrue(coordinator.awaitIdle()) }

        val restored = CaptureRepository(context).get(captureId)!!
        assertEquals("synthetic visible draft", restored.previewText)
        assertEquals(null, restored.finalText)
        assertEquals(RecognitionState.NO_SPEECH, restored.recognitionState)
        assertEquals(0L, restored.transcriptVersion)
        assertTrue(exports.isEmpty())
        assertTrue(receipts.any { it.optString("recognitionState") == "NO_SPEECH" })
        assertTrue(receipts.none { it.has("finalText") || it.optString("recognitionState") == "FINAL" })
        assertTrue(wav.isFile)
    }

    @Test
    fun recoveredRecognitionWithEmptyFinalRetainsThePreviousAttemptsDraftAndAudio() {
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        savePeerFixture(savedRecord(wav, CaptureState.SAVED, RecognitionState.RUNNING).copy(
            recognitionSessionId = sessionId, previewText = "synthetic previous attempt draft", lastRevision = 4,
        ))
        coordinator.retryPendingCaptures()
        repeat(2) { assertTrue(coordinator.awaitIdle()) }
        assertTrue(speech.finished)
        speech.events!!.onHypothesis("", true)
        assertTrue(coordinator.awaitIdle())

        val restored = CaptureRepository(context).get(captureId)!!
        assertEquals("synthetic previous attempt draft", restored.previewText)
        assertEquals(null, restored.finalText)
        assertEquals(RecognitionState.NO_SPEECH, restored.recognitionState)
        assertEquals(wav.absolutePath, restored.localAudioPath)
        assertTrue(wav.isFile)
        assertTrue(exports.isEmpty())
    }

    @Test
    fun finalRecognitionResetsRetryBudgetBeforeOrgExport() {
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        savePeerFixture(savedRecord(wav, CaptureState.SAVED, RecognitionState.FAILED).copy(
            retryCount = 4,
            nextRetryAtUtc = "2099-01-02T03:04:05Z",
        ))

        coordinator.retryCapture(captureId)
        assertTrue(coordinator.awaitIdle())
        speech.events!!.onHypothesis("synthetic recovered transcript", true)
        assertTrue(coordinator.awaitIdle())

        val result = repository.get(captureId)!!
        assertEquals(RecognitionState.FINAL, result.recognitionState)
        assertEquals(0, result.retryCount)
        assertEquals(null, result.nextRetryAtUtc)
        assertEquals(listOf(0), exportRetryCounts)
    }

    @Test
    fun pendingRetryReplaysSavedCaptureLeftRunningByPreviousCoordinator() {
        val pcm = byteArrayOf(1, 2, 3, 4, 5, 6, 7, 8)
        val wav = writeWav(pcm)
        savePeerFixture(savedRecord(wav, CaptureState.SAVED, RecognitionState.RUNNING).copy(
            recognitionSessionId = sessionId,
        ))

        coordinator.retryPendingCaptures()
        repeat(2) { assertTrue(coordinator.awaitIdle()) }
        assertTrue("stale running capture was not replayed", speech.finished)
        assertArrayEquals(pcm, speech.received.toByteArray())
        val replaySessionId = repository.get(captureId)!!.recognitionSessionId
        assertTrue(replaySessionId != null && replaySessionId != sessionId)

        speech.events!!.onHypothesis("synthetic recovered transcript", true)
        assertTrue(coordinator.awaitIdle())
        coordinator.respondStatus(captureId, "synthetic-node")
        repeat(2) { assertTrue(coordinator.awaitIdle()) }
        assertEquals(RecognitionState.FINAL, repository.get(captureId)?.recognitionState)
        assertTrue(receipts.any { it.optString("finalText") == "synthetic recovered transcript" })
    }

    @Test
    fun retryRequestsDoNotCancelOrDuplicateAnActiveRecognitionSession() {
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        savePeerFixture(savedRecord(wav, CaptureState.SAVED, RecognitionState.QUEUED))

        coordinator.retryCapture(captureId)
        assertTrue(coordinator.awaitIdle())
        val activeSessionId = repository.get(captureId)!!.recognitionSessionId
        coordinator.retryPendingCaptures()
        coordinator.retryCapture(captureId)
        repeat(2) { assertTrue(coordinator.awaitIdle()) }

        assertEquals(1, speech.startCount)
        assertEquals(0, speech.cancelCount)
        assertEquals(activeSessionId, repository.get(captureId)?.recognitionSessionId)
        speech.events!!.onHypothesis("synthetic active transcript", true)
        assertTrue(coordinator.awaitIdle())
        assertEquals("synthetic active transcript", repository.get(captureId)?.finalText)
    }

    @Test
    fun unsupportedExternalAudioLeavesTheSavedRecordingAvailableForAnotherRetry() {
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        val record = savedRecord(wav, captureState = CaptureState.SAVED, recognitionState = RecognitionState.QUEUED)
        savePeerFixture(record)
        speech.supportsExternalAudio = false

        coordinator.retryCapture(captureId)
        assertTrue(coordinator.awaitIdle())

        val result = repository.get(captureId)!!
        assertEquals(RecognitionState.UNAVAILABLE, result.recognitionState)
        assertEquals("external-audio-provider-unavailable", result.lastErrorCode)
        assertTrue(File(result.localAudioPath!!).isFile)
        assertFalse(speech.started)
    }

    @Test
    fun archiveFinalizesProvisionalLiveRecordAndPreservesItsFinalTranscript() {
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        savePeerFixture(
            CaptureRecord(
                captureId = captureId,
                createdAtUtc = STAMP,
                startedAtUtc = STAMP,
                captureState = CaptureState.RECORDING,
                recognitionState = RecognitionState.FINAL,
                recognitionSessionId = sessionId,
                previewText = "final synthetic note",
                finalText = "final synthetic note",
                lastRevision = 3,
                transcriptVersion = 1,
                engineId = "synthetic-engine",
                observedFrames = WavPcmReader(wav, CaptureFormat()).use { it.frameCount },
            ),
        )
        val archived = savedRecord(wav, captureState = CaptureState.SAVED, recognitionState = RecognitionState.QUEUED)
            .copy(archiveState = ArchiveState.STAGED, transferState = TransferState.PHONE_STAGED)

        coordinator.onArchiveReceived(archived, "synthetic-node")
        assertTrue(coordinator.awaitIdle())

        val merged = repository.get(captureId)!!
        assertEquals(CaptureState.SAVED, merged.captureState)
        assertEquals(archived.sha256, merged.sha256)
        assertEquals("final synthetic note", merged.finalText)
        assertEquals("final synthetic note", merged.previewText)
        assertEquals(sessionId, merged.recognitionSessionId)
        assertEquals(RecognitionState.FINAL, merged.recognitionState)
        assertEquals("synthetic-engine", merged.engineId)
        assertEquals(ArchiveState.VERIFIED, merged.archiveState)
        assertEquals(listOf(captureId), exports)
        repeat(2) { assertTrue(coordinator.awaitIdle()) }
        val completion = receipts.first { it.optString("recognitionState") == "FINAL" }
        assertEquals("final synthetic note", completion.getString("finalText"))
        assertEquals(archived.sha256, completion.getString("archiveHash"))
    }

    @Test
    fun finalCompletionDoesNotRequireANotesFolderOrOrgExport() {
        coordinator.shutdownForTests()
        coordinator = PhoneCaptureCoordinator(
            context, speech,
            { _, path, bytes -> if (path == CaptureProtocol.RECEIPT) receipts += JSONObject(String(bytes, Charsets.UTF_8)) },
            { throw IllegalStateException("notes-folder-not-selected") },
            { error("Export must not gate capture completion") },
            replayPacingMillis = 0, runtimeReady = { true },
        )
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        val record = savedRecord(wav, CaptureState.SAVED, RecognitionState.FINAL).copy(
            finalText = "synthetic locally saved note", archiveState = ArchiveState.STAGED,
            transferState = TransferState.PHONE_STAGED,
        )
        coordinator.onArchiveReceived(record, "synthetic-node")
        repeat(3) { assertTrue(coordinator.awaitIdle()) }
        assertTrue(receipts.any { it.optString("finalText") == record.finalText && it.optString("recognitionState") == "FINAL" })
        assertTrue(File(repository.get(captureId)!!.localAudioPath!!).isFile)
    }

    @Test
    fun oversizedFinalReceiptUsesFullBoundedTargetedPayloadInsteadOfFailureReceipt() {
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        val text = "x".repeat(CaptureProtocol.MAX_CHANNEL_RESULT_BYTES - 512)
        savePeerFixture(savedRecord(wav, CaptureState.SAVED, RecognitionState.FINAL).copy(
            finalText = text, archiveState = ArchiveState.STAGED, transferState = TransferState.PHONE_STAGED,
        ))

        coordinator.respondStatus(captureId, "synthetic-node")
        repeat(2) { assertTrue(coordinator.awaitIdle()) }

        assertEquals(1, oversizedReceipts.size)
        val receipt = JSONObject(String(oversizedReceipts.single(), Charsets.UTF_8))
        assertEquals(text, receipt.getString("finalText"))
        assertEquals("FINAL", receipt.getString("recognitionState"))
        assertTrue(CaptureProtocol.validatePayloadSize(oversizedReceipts.single().size, channelResult = true))
        assertTrue(receipts.none { it.optString("errorCode") == "transcript-not-sent-exceeds-message-limit" })
    }

    @Test
    fun statusRetriesFailedRecognitionWithBackoffAndDoesNotDuplicateActiveOrImmediateAttempts() {
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        savePeerFixture(savedRecord(wav, CaptureState.SAVED, RecognitionState.FAILED).copy(
            retryCount = 0, nextRetryAtUtc = null,
        ))

        coordinator.respondStatus(captureId, "synthetic-node")
        repeat(2) { assertTrue(coordinator.awaitIdle()) }
        assertTrue(speech.started)
        assertEquals(1, speech.startCount)
        coordinator.respondStatus(captureId, "synthetic-node")
        repeat(2) { assertTrue(coordinator.awaitIdle()) }
        assertEquals(1, speech.startCount)
        speech.events!!.onFailure("synthetic-recognition-failure")
        repeat(2) { assertTrue(coordinator.awaitIdle()) }

        coordinator.respondStatus(captureId, "synthetic-node")
        repeat(2) { assertTrue(coordinator.awaitIdle()) }
        assertEquals(1, speech.startCount)
        assertEquals(1, repository.get(captureId)!!.retryCount)

        repository.update(captureId) { it.copy(nextRetryAtUtc = "2000-01-01T00:00:00Z") }
        coordinator.respondStatus(captureId, "synthetic-node")
        repeat(2) { assertTrue(coordinator.awaitIdle()) }
        assertEquals(2, speech.startCount)
    }

    @Test
    fun finalArrivingAfterVerifiedArchiveAlsoTriggersOrgExport() {
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        savePeerFixture(savedRecord(wav, captureState = CaptureState.SAVED, recognitionState = RecognitionState.QUEUED)
            .copy(archiveState = ArchiveState.VERIFIED, transferState = TransferState.PHONE_ARCHIVED,
                archivedAudioUri = "content://synthetic-notes/recording.wav"))

        coordinator.retryCapture(captureId)
        assertTrue(coordinator.awaitIdle())
        assertTrue(exports.isEmpty())
        speech.events!!.onHypothesis("arrived after archive", true)
        assertTrue(coordinator.awaitIdle())

        assertEquals(listOf(captureId), exports)
        assertEquals("arrived after archive", repository.get(captureId)?.finalText)
    }

    @Test
    fun finalizedHashConflictIsRejectedWithoutChangingDurableRow() {
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        val stored = savedRecord(wav, captureState = CaptureState.SAVED, recognitionState = RecognitionState.QUEUED)
            .copy(sha256 = "a".repeat(64), finalizedByteLength = wav.length(), archiveState = ArchiveState.STAGED)
        savePeerFixture(stored)
        val conflict = stored.copy(sha256 = "b".repeat(64), localAudioPath = wav.absolutePath)

        assertThrows(IllegalStateException::class.java) { coordinator.onArchiveReceived(conflict, "synthetic-node") }
        assertEquals(stored, repository.get(captureId))
    }

    @Test
    fun truncatedLiveFinalIsOnlyAPreviewUntilTheFullArchiveIsRecognized() {
        val wav = writeWav(byteArrayOf(1, 2, 3, 4, 5, 6, 7, 8))
        savePeerFixture(CaptureRecord(
            captureId = captureId, createdAtUtc = STAMP, captureState = CaptureState.RECORDING,
            recognitionSessionId = sessionId, recognitionState = RecognitionState.FINAL,
            observedFrames = 1, finalText = "synthetic truncated prefix",
        ))
        coordinator.onArchiveReceived(savedRecord(wav, CaptureState.SAVED, RecognitionState.QUEUED), "synthetic-node")
        repeat(3) { assertTrue(coordinator.awaitIdle()) }
        val recovering = repository.get(captureId)!!
        assertEquals(null, recovering.finalText)
        assertEquals("synthetic truncated prefix", recovering.previewText)
        assertTrue(exports.isEmpty())
        assertFalse(receipts.any { it.optString("recognitionState") == "FINAL" })
        assertArrayEquals(byteArrayOf(1, 2, 3, 4, 5, 6, 7, 8), speech.received.toByteArray())
        speech.events!!.onHypothesis("synthetic full recording transcript", true)
        assertTrue(coordinator.awaitIdle())
        assertEquals("synthetic full recording transcript", repository.get(captureId)?.finalText)
        assertEquals(listOf(captureId), exports)
    }

    @Test
    fun interruptedArchiveRemainsInterruptedAndNeverAutomaticallyExports() {
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        val interrupted = savedRecord(wav, CaptureState.INTERRUPTED, RecognitionState.FINAL).copy(
            finalText = "synthetic interrupted preview",
            archiveState = ArchiveState.STAGED,
            transferState = TransferState.PHONE_STAGED,
        )

        coordinator.onArchiveReceived(interrupted, "synthetic-node")
        assertTrue(coordinator.awaitIdle())

        assertEquals(CaptureState.INTERRUPTED, repository.get(captureId)?.captureState)
        assertEquals(ArchiveState.VERIFIED, repository.get(captureId)?.archiveState)
        assertTrue(exports.isEmpty())
    }

    @Test
    fun queuedArchiveAutomaticallyReplaysWhenThereIsNoActiveLiveAttempt() {
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        val queued = savedRecord(wav, CaptureState.SAVED, RecognitionState.QUEUED)
            .copy(archiveState = ArchiveState.STAGED, transferState = TransferState.PHONE_STAGED)
        coordinator.onArchiveReceived(queued, "synthetic-node")
        repeat(3) { assertTrue(coordinator.awaitIdle()) }
        assertTrue(speech.started)
        assertTrue(speech.finished)
    }

    @Test
    fun interruptedCaptureRequiresExplicitFinalizationBeforeRecognitionAndExport() {
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        savePeerFixture(savedRecord(wav, CaptureState.INTERRUPTED, RecognitionState.QUEUED))
        coordinator.retryCapture(captureId)
        assertTrue(coordinator.awaitIdle())
        assertFalse(speech.started)

        coordinator.finalizeCapture(captureId)
        assertTrue(coordinator.awaitIdle())
        assertEquals(CaptureState.SAVED, repository.get(captureId)?.captureState)
        assertTrue(speech.started)
    }

    @Test fun duplicateInterruptedArchivePreservesPhoneFinalizationBeforeAndAfterRecognitionAndRejectsConflict() {
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        val interrupted = savedRecord(wav, CaptureState.INTERRUPTED, RecognitionState.QUEUED)
            .copy(stopReason = "ui-interruption", archiveState = ArchiveState.STAGED)
        coordinator.onArchiveReceived(interrupted, "synthetic-node")
        assertTrue(coordinator.awaitIdle())
        assertFalse(speech.started)
        coordinator.finalizeCapture(captureId)
        assertTrue(coordinator.awaitIdle())
        assertTrue(speech.started)
        coordinator.onArchiveReceived(interrupted, "synthetic-node")
        assertTrue(coordinator.awaitIdle())
        assertEquals(CaptureState.SAVED, repository.get(captureId)!!.captureState)
        assertEquals("user-finalized", repository.get(captureId)!!.stopReason)
        assertEquals(1, speech.startCount)
        speech.events!!.onHypothesis("Synthetic finalized transcript", true)
        assertTrue(coordinator.awaitIdle())
        val finalized = repository.get(captureId)!!
        coordinator.onArchiveReceived(interrupted, "synthetic-node")
        assertTrue(coordinator.awaitIdle())
        assertEquals(CaptureState.SAVED, repository.get(captureId)!!.captureState)
        assertEquals("user-finalized", repository.get(captureId)!!.stopReason)
        assertEquals(finalized.finalText, repository.get(captureId)!!.finalText)
        assertEquals(finalized.transcriptVersion, repository.get(captureId)!!.transcriptVersion)
        assertEquals(listOf(captureId), exports)
        val beforeConflict = repository.get(captureId)
        for (conflict in listOf(interrupted.copy(sha256 = "b".repeat(64)),
            interrupted.copy(finalizedByteLength = interrupted.finalizedByteLength!! + 2))) {
            assertThrows(IllegalStateException::class.java) { coordinator.onArchiveReceived(conflict, "synthetic-node") }
            assertEquals(beforeConflict, repository.get(captureId))
        }
    }

    @Test fun duplicateInterruptedArchiveCannotUndoFinalizationWhileRecognitionAdmissionIsBlocked() {
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        val interrupted = savedRecord(wav, CaptureState.INTERRUPTED, RecognitionState.QUEUED).copy(stopReason = "ui-interruption")
        savePeerFixture(interrupted)
        val occupied = leases.tryAcquire(RecognitionResourceLease.Kind.CHAT)!!
        try {
            coordinator.finalizeCapture(captureId)
            assertTrue(coordinator.awaitIdle())
            assertFalse(speech.started)
            coordinator.onArchiveReceived(interrupted, "synthetic-node")
            assertTrue(coordinator.awaitIdle())
            assertEquals(CaptureState.SAVED, repository.get(captureId)!!.captureState)
            assertEquals("user-finalized", repository.get(captureId)!!.stopReason)
            assertFalse(speech.started)
        } finally { occupied.close() }
        assertTrue(coordinator.awaitIdle())
        assertTrue(speech.started)
    }

    @Test fun privateProviderExceptionCodeIsRedactedBeforePersistenceProjectionAndReceipt() {
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        savePeerFixture(savedRecord(wav, CaptureState.SAVED, RecognitionState.QUEUED))
        coordinator.retryCapture(captureId, "synthetic-node")
        assertTrue(coordinator.awaitIdle())
        val privateFailure = java.io.IOException("Synthetic /storage/synthetic-person/voice.wav content://synthetic.private/tree/note")
        speech.events!!.onFailure(privateFailure.message!!)
        assertTrue(coordinator.awaitIdle())
        val stored = repository.get(captureId)!!
        assertEquals("transcription-unavailable", stored.lastErrorCode)
        assertFalse(NativeCaptureProjection.record(stored).toString().contains("synthetic-person"))
        assertFalse(receipts.any { it.toString().contains("content://synthetic.private") })
    }

    @Test
    fun supplyingReplayFramesRefreshesTheInactivityTimeoutUntilEndOfAudio() {
        coordinator.shutdownForTests()
        coordinator = PhoneCaptureCoordinator(
            context, speech, { _, _, _ -> },
            { Uri.parse("content://synthetic-notes/recording.wav") },
            { OrgExportResult(OrgState.EXPORTED, "synthetic-destination") },
            replayPacingMillis = 100,
            recognitionTimeoutSeconds = 1,
            runtimeReady = { true },
        )
        val pcm = ByteArray(3_200 * 15) { (it % 127).toByte() }
        val wav = writeWav(pcm)
        savePeerFixture(savedRecord(wav, CaptureState.SAVED, RecognitionState.QUEUED)
            .copy(durableFrames = pcm.size.toLong() / 2, observedFrames = pcm.size.toLong() / 2))

        coordinator.retryCapture(captureId)
        assertTrue(coordinator.awaitIdle())
        assertArrayEquals(pcm, speech.received.toByteArray())
        speech.events!!.onHypothesis("synthetic long replay completed", true)
        assertTrue(coordinator.awaitIdle())
        assertEquals(RecognitionState.FINAL, repository.get(captureId)?.recognitionState)
    }

    @Test
    fun replayFeedingDoesNotBlockTranscriptPersistenceOrDelivery() {
        coordinator.shutdownForTests()
        val feeding = CountDownLatch(1)
        val release = CountDownLatch(1)
        val delivered = CountDownLatch(1)
        speech.onAppend = { feeding.countDown(); release.await(5, TimeUnit.SECONDS) }
        coordinator = PhoneCaptureCoordinator(
            context, speech, { _, path, _ -> if (path == CaptureProtocol.PARTIAL) delivered.countDown() },
            { Uri.parse("content://synthetic-notes/recording.wav") },
            { OrgExportResult(OrgState.EXPORTED, "synthetic-destination") },
            replayPacingMillis = 0, runtimeReady = { true },
        )
        savePeerFixture(savedRecord(writeWav(byteArrayOf(1, 2, 3, 4)), CaptureState.SAVED, RecognitionState.QUEUED))
        try {
            coordinator.retryCapture(captureId, "synthetic-node")
            assertTrue(feeding.await(2, TimeUnit.SECONDS))
            speech.events!!.onHypothesis("synthetic partial while feeding", false)
            assertTrue("PCM replay blocked transcript delivery", delivered.await(1, TimeUnit.SECONDS))
            assertEquals("synthetic partial while feeding", repository.get(captureId)?.previewText)
        } finally {
            release.countDown()
            assertTrue(coordinator.awaitIdle())
        }
    }

    @Test
    fun freshLiveCapturePreemptsSavedReplayWithoutLosingItsAudioOrRetryState() {
        org.robolectric.Shadows.shadowOf(RuntimeEnvironment.getApplication()).grantPermissions(android.Manifest.permission.RECORD_AUDIO)
        val feeding = CountDownLatch(1)
        val release = CountDownLatch(1)
        speech.onAppend = { feeding.countDown(); release.await(5, TimeUnit.SECONDS) }
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        savePeerFixture(savedRecord(wav, CaptureState.SAVED, RecognitionState.QUEUED))
        val liveId = "123e4567-e89b-42d3-a456-426614174020"
        try {
            coordinator.retryCapture(captureId)
            assertTrue(feeding.await(2, TimeUnit.SECONDS))
            coordinator.beginLive(liveId, sessionId, "synthetic-node", CaptureFormat(), STAMP)
            assertEquals(RecognitionState.RUNNING, repository.get(liveId)?.recognitionState)
            assertEquals(RecognitionState.QUEUED, repository.get(captureId)?.recognitionState)
            assertEquals(1, speech.cancelCount)
            assertTrue(wav.isFile)
            coordinator.retryCapture(captureId)
        } finally {
            release.countDown()
            assertTrue(coordinator.awaitIdle())
        }
        assertEquals(2, speech.startCount)
    }

    @Test
    fun cancellationBeforeReplayAdapterStartCannotLeaveAnOrphanRecognizer() {
        org.robolectric.Shadows.shadowOf(RuntimeEnvironment.getApplication()).grantPermissions(android.Manifest.permission.RECORD_AUDIO)
        val enteringStart = CountDownLatch(1)
        val release = CountDownLatch(1)
        val first = AtomicBoolean(true)
        speech.beforeStart = {
            if (first.compareAndSet(true, false)) {
                enteringStart.countDown()
                assertTrue(release.await(5, TimeUnit.SECONDS))
            }
        }
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        savePeerFixture(savedRecord(wav, CaptureState.SAVED, RecognitionState.QUEUED))
        try {
            coordinator.retryCapture(captureId)
            assertTrue(enteringStart.await(2, TimeUnit.SECONDS))
            coordinator.beginLive("123e4567-e89b-42d3-a456-426614174020", sessionId, "synthetic-node", CaptureFormat(), STAMP)
        } finally {
            release.countDown()
            assertTrue(coordinator.awaitIdle())
        }
        assertEquals(setOf(sessionId), speech.activeSessions.toSet())
        assertEquals(RecognitionState.QUEUED, repository.get(captureId)?.recognitionState)
        assertTrue(wav.isFile)
    }

    @Test
    fun replayAppendCannotSupplyStalePcmToASameIdReplacementAfterItsCoordinatorCheck() {
        assertReplayMutationCannotReachSameIdReplacement(blockFinish = false)
    }

    @Test
    fun replayFinishCannotEndASameIdReplacementAfterItsCoordinatorCheck() {
        assertReplayMutationCannotReachSameIdReplacement(blockFinish = true)
    }

    private fun assertReplayMutationCannotReachSameIdReplacement(blockFinish: Boolean) {
        org.robolectric.Shadows.shadowOf(RuntimeEnvironment.getApplication()).grantPermissions(android.Manifest.permission.RECORD_AUDIO)
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val first = AtomicBoolean(true)
        val beforeMutation: () -> Unit = {
            if (first.compareAndSet(true, false)) {
                entered.countDown()
                assertTrue(release.await(5, TimeUnit.SECONDS))
            }
        }
        if (blockFinish) speech.beforeFinish = beforeMutation else speech.beforeAppend = beforeMutation
        val replayPcm = byteArrayOf(1, 2, 3, 4)
        val wav = writeWav(replayPcm)
        savePeerFixture(savedRecord(wav, CaptureState.SAVED, RecognitionState.QUEUED))
        val replacementId = "123e4567-e89b-42d3-a456-426614174040"
        try {
            coordinator.retryCapture(captureId)
            assertTrue("Replay did not reach the adapter mutation boundary", entered.await(2, TimeUnit.SECONDS))
            val replaySessionId = repository.get(captureId)!!.recognitionSessionId!!
            val oldEvents = speech.events!!
            oldEvents.onFailure("synthetic-replay-retired")
            assertTrue(speech.activeSessions.isEmpty())
            assertTrue(coordinator.beginLive(replacementId, replaySessionId, "synthetic-node", CaptureFormat(), STAMP))
            val replacement = speech.events!!
            val held = leases.current()!!
            assertFalse(oldEvents.isCurrent())
            release.countDown()
            assertTrue(coordinator.awaitIdle())

            val acceptedReplayPcm = if (blockFinish) replayPcm else byteArrayOf()
            assertArrayEquals("Retired replay supplied PCM to the replacement", acceptedReplayPcm, speech.received.toByteArray())
            assertFalse("Retired replay ended the replacement input", speech.finished)
            assertTrue(replacement.isCurrent())
            assertSame(held, leases.current())
            assertEquals(setOf(replaySessionId), speech.activeSessions.toSet())
            assertEquals(2, speech.startCount)
            assertEquals(1, speech.cancelCount)
            assertEquals(RecognitionState.FAILED, repository.get(captureId)?.recognitionState)
            assertEquals(replaySessionId, repository.get(captureId)?.recognitionSessionId)
            assertEquals(RecognitionState.RUNNING, repository.get(replacementId)?.recognitionState)
            assertTrue(wav.isFile)

            val inputOwner: () -> Unit = {}
            assertTrue(coordinator.registerLiveInput(replacementId, replaySessionId, "synthetic-node", inputOwner))
            val freshPcm = byteArrayOf(5, 6, 7, 8)
            coordinator.appendLive(replacementId, replaySessionId, "synthetic-node", inputOwner, freshPcm)
            assertArrayEquals(acceptedReplayPcm + freshPcm, speech.received.toByteArray())
            coordinator.finishLive(replacementId, replaySessionId, "synthetic-node", inputOwner, 2)
            assertTrue(speech.finished)
        } finally {
            release.countDown()
            assertTrue(coordinator.awaitIdle())
        }
    }

    @Test
    fun consecutiveLiveCaptureRequeuesEndedInputWithoutAcceptingItsLateFinal() {
        org.robolectric.Shadows.shadowOf(RuntimeEnvironment.getApplication()).grantPermissions(android.Manifest.permission.RECORD_AUDIO)
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        coordinator.beginLive(captureId, sessionId, "synthetic-node", CaptureFormat(), STAMP)
        val previousEvents = speech.events!!
        val inputOwner: () -> Unit = {}
        assertTrue(coordinator.registerLiveInput(captureId, sessionId, "synthetic-node", inputOwner))
        coordinator.appendLive(captureId, sessionId, "synthetic-node", inputOwner, byteArrayOf(1, 2, 3, 4))
        coordinator.finishLive(captureId, sessionId, "synthetic-node", inputOwner, 2)
        coordinator.onArchiveReceived(savedRecord(wav, CaptureState.SAVED, RecognitionState.QUEUED), "synthetic-node")
        assertTrue(coordinator.awaitIdle())
        val nextId = "123e4567-e89b-42d3-a456-426614174020"
        val nextSession = "123e4567-e89b-42d3-a456-426614174021"
        coordinator.beginLive(nextId, nextSession, "synthetic-node", CaptureFormat(), STAMP)
        assertEquals(setOf(nextSession), speech.activeSessions.toSet())
        previousEvents.onHypothesis("synthetic stale final", true)
        assertTrue(coordinator.awaitIdle())
        assertEquals(RecognitionState.QUEUED, repository.get(captureId)?.recognitionState)
        assertEquals(null, repository.get(captureId)?.finalText)
        assertTrue(File(repository.get(captureId)!!.localAudioPath!!).isFile)
        assertEquals(RecognitionState.RUNNING, repository.get(nextId)?.recognitionState)
    }

    @Test
    fun preemptionCannotDowngradeTerminalPersistenceThatCompletesDuringCancellation() {
        org.robolectric.Shadows.shadowOf(RuntimeEnvironment.getApplication()).grantPermissions(android.Manifest.permission.RECORD_AUDIO)
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        savePeerFixture(savedRecord(wav, CaptureState.SAVED, RecognitionState.QUEUED))
        coordinator.retryCapture(captureId)
        assertTrue(coordinator.awaitIdle())
        val oldSession = repository.get(captureId)!!.recognitionSessionId
        speech.onCancel = { cancelled ->
            if (cancelled == oldSession) repository.update(captureId) {
                it.copy(recognitionState = RecognitionState.FINAL, finalText = "synthetic accepted final", transcriptVersion = 1)
            }
        }
        coordinator.beginLive("123e4567-e89b-42d3-a456-426614174020", sessionId, "synthetic-node", CaptureFormat(), STAMP)
        assertEquals(setOf(sessionId), speech.activeSessions.toSet())
        assertEquals(RecognitionState.FINAL, repository.get(captureId)?.recognitionState)
        assertEquals("synthetic accepted final", repository.get(captureId)?.finalText)
        assertEquals(1L, repository.get(captureId)?.transcriptVersion)
        assertEquals(oldSession, repository.get(captureId)?.recognitionSessionId)
        coordinator.retryPendingCaptures()
        assertTrue(coordinator.awaitIdle())
        assertEquals(2, speech.startCount)
    }

    @Test
    fun acceptedFinalWaitingForPersistenceSurvivesAdmissionOfTheNextLiveCapture() {
        org.robolectric.Shadows.shadowOf(RuntimeEnvironment.getApplication()).grantPermissions(android.Manifest.permission.RECORD_AUDIO)
        coordinator.shutdownForTests()
        val blocked = CountDownLatch(1)
        val release = CountDownLatch(1)
        val first = AtomicBoolean(true)
        coordinator = PhoneCaptureCoordinator(
            context, speech,
            { _, _, _ -> if (first.compareAndSet(true, false)) { blocked.countDown(); assertTrue(release.await(5, TimeUnit.SECONDS)) } },
            { Uri.parse("content://synthetic-notes/recording.wav") },
            { OrgExportResult(OrgState.EXPORTED, "synthetic-destination") },
            replayPacingMillis = 0, runtimeReady = { true },
        )
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        val nextId = "123e4567-e89b-42d3-a456-426614174020"
        val nextSession = "123e4567-e89b-42d3-a456-426614174021"
        try {
            coordinator.beginLive(captureId, sessionId, "synthetic-node", CaptureFormat(), STAMP)
            assertTrue(blocked.await(2, TimeUnit.SECONDS))
            val inputOwner: () -> Unit = {}
            assertTrue(coordinator.registerLiveInput(captureId, sessionId, "synthetic-node", inputOwner))
            coordinator.appendLive(captureId, sessionId, "synthetic-node", inputOwner, byteArrayOf(1, 2, 3, 4))
            coordinator.finishLive(captureId, sessionId, "synthetic-node", inputOwner, 2)
            coordinator.onArchiveReceived(savedRecord(wav, CaptureState.SAVED, RecognitionState.QUEUED), "synthetic-node")
            speech.complete(sessionId, "synthetic accepted final")
            val terminalRetirementCount = speech.cancelCount
            assertEquals(1, terminalRetirementCount)
            coordinator.beginLive(nextId, nextSession, "synthetic-node", CaptureFormat(), STAMP)
            assertEquals(setOf(nextSession), speech.activeSessions.toSet())
            assertEquals(terminalRetirementCount, speech.cancelCount)
        } finally {
            release.countDown()
            assertTrue(coordinator.awaitIdle())
        }
        assertEquals(RecognitionState.FINAL, repository.get(captureId)?.recognitionState)
        assertEquals("synthetic accepted final", repository.get(captureId)?.finalText)
        assertEquals(1L, repository.get(captureId)?.transcriptVersion)
        assertEquals(RecognitionState.RUNNING, repository.get(nextId)?.recognitionState)
    }

    @Test
    fun completingAnotherNoteDoesNotBypassAutomaticRetryBudgetOrDeadline() {
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        val exhaustedId = "123e4567-e89b-42d3-a456-426614174020"
        val delayedId = "123e4567-e89b-42d3-a456-426614174021"
        val future = "2099-01-02T03:04:05Z"
        savePeerFixture(savedRecord(wav, CaptureState.SAVED, RecognitionState.FAILED).copy(
            captureId = exhaustedId, retryCount = 5,
        ))
        savePeerFixture(savedRecord(wav, CaptureState.SAVED, RecognitionState.FAILED).copy(
            captureId = delayedId, retryCount = 1, nextRetryAtUtc = future,
        ))
        savePeerFixture(savedRecord(wav, CaptureState.SAVED, RecognitionState.QUEUED))
        coordinator.retryCapture(captureId)
        assertTrue(coordinator.awaitIdle())
        speech.events!!.onHypothesis("synthetic completed note", true)
        repeat(2) { assertTrue(coordinator.awaitIdle()) }
        assertEquals(1, speech.startCount)
        assertEquals(5, repository.get(exhaustedId)?.retryCount)
        assertEquals(future, repository.get(delayedId)?.nextRetryAtUtc)
        assertTrue(wav.isFile)
        coordinator.retryCapture(exhaustedId)
        assertTrue(coordinator.awaitIdle())
        assertEquals(2, speech.startCount)
    }

    @Test
    fun stagedAudioFinalizesAndSendsCompletionWhileOptionalSafArchivalIsBlocked() {
        coordinator.shutdownForTests()
        val archival = CountDownLatch(1)
        val release = CountDownLatch(1)
        val recognition = CountDownLatch(1)
        val inputFinished = CountDownLatch(1)
        val completion = CountDownLatch(1)
        val finalReceipts = CopyOnWriteArrayList<JSONObject>()
        speech.onStart = { recognition.countDown() }
        speech.onFinish = { inputFinished.countDown() }
        coordinator = PhoneCaptureCoordinator(
            context, speech,
            { _, path, bytes ->
                if (path == CaptureProtocol.RECEIPT) {
                    val body = JSONObject(String(bytes, Charsets.UTF_8))
                    if (body.optString("recognitionState") == "FINAL") { finalReceipts += body; completion.countDown() }
                }
            },
            { archival.countDown(); assertTrue(release.await(5, TimeUnit.SECONDS)); Uri.parse("content://synthetic-notes/recording.wav") },
            { OrgExportResult(OrgState.EXPORTED, "synthetic-destination") },
            replayPacingMillis = 0, runtimeReady = { true },
        )
        val record = savedRecord(writeWav(byteArrayOf(1, 2, 3, 4)), CaptureState.SAVED, RecognitionState.QUEUED)
            .copy(archiveState = ArchiveState.STAGED, transferState = TransferState.PHONE_STAGED, archivedAudioUri = null)
        try {
            coordinator.onArchiveReceived(record, "synthetic-node")
            assertTrue(archival.await(2, TimeUnit.SECONDS))
            assertTrue("SAF archival blocked recognition", recognition.await(1, TimeUnit.SECONDS))
            assertTrue(inputFinished.await(1, TimeUnit.SECONDS))
            speech.events!!.onHypothesis("synthetic independent final transcript", true)
            assertTrue("SAF archival blocked final receipt", completion.await(1, TimeUnit.SECONDS))
            assertEquals("synthetic independent final transcript", repository.get(captureId)?.finalText)
            assertEquals(record.sha256, finalReceipts.first().getString("archiveHash"))
            assertEquals("synthetic independent final transcript", finalReceipts.first().getString("finalText"))
            assertArrayEquals(byteArrayOf(1, 2, 3, 4), speech.received.toByteArray())
            assertEquals(ArchiveState.STAGED, repository.get(captureId)?.archiveState)
        } finally {
            release.countDown()
            assertTrue(coordinator.awaitIdle())
        }
    }

    @Test
    fun savedRecognitionWaitsForForegroundReadinessThenResumesThroughPendingWork() {
        coordinator.shutdownForTests()
        var ready = false
        coordinator = PhoneCaptureCoordinator(
            context, speech, { _, _, _ -> },
            { Uri.parse("content://synthetic-notes/recording.wav") },
            { OrgExportResult(OrgState.EXPORTED, "synthetic-destination") },
            replayPacingMillis = 0,
            runtimeReady = { ready },
        )
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        savePeerFixture(savedRecord(wav, CaptureState.SAVED, RecognitionState.QUEUED))
        coordinator.retryCapture(captureId)
        assertTrue(coordinator.awaitIdle())
        assertFalse(speech.started)
        assertEquals(RecognitionState.QUEUED, repository.get(captureId)?.recognitionState)

        ready = true
        coordinator.retryPendingCaptures()
        repeat(2) { assertTrue(coordinator.awaitIdle()) }
        assertTrue(speech.finished)
    }

    @Test
    fun pendingArchivalAndExportResumeAfterTheDestinationBecomesAvailable() {
        coordinator.shutdownForTests()
        var folderAvailable = false
        coordinator = PhoneCaptureCoordinator(
            context, speech, { _, _, _ -> },
            { if (!folderAvailable) error("notes-folder-not-selected") else Uri.parse("content://synthetic-notes/recording.wav") },
            { id -> exports += id; OrgExportResult(OrgState.EXPORTED, "synthetic-destination") },
            replayPacingMillis = 0,
            runtimeReady = { true },
        )
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        savePeerFixture(savedRecord(wav, CaptureState.SAVED, RecognitionState.FINAL)
            .copy(finalText = "synthetic queued task", archiveState = ArchiveState.STAGED, archivedAudioUri = null))
        coordinator.retryCapture(captureId)
        assertTrue(coordinator.awaitIdle())
        assertEquals(ArchiveState.WAITING_FOR_FOLDER, repository.get(captureId)?.archiveState)
        assertTrue(exports.isEmpty())

        folderAvailable = true
        coordinator.retryPendingCaptures()
        repeat(2) { assertTrue(coordinator.awaitIdle()) }
        assertEquals(ArchiveState.VERIFIED, repository.get(captureId)?.archiveState)
        assertEquals(listOf(captureId), exports)
    }

    private fun savePeerFixture(record: CaptureRecord) {
        if (repository.get(record.captureId) == null) {
            assertEquals(record.copy(captureId = record.captureId.lowercase()), repository.admitPhoneCapture(record, "synthetic-node"))
        } else repository.save(record)
    }

    private fun savedRecord(file: File, captureState: CaptureState, recognitionState: RecognitionState): CaptureRecord {
        val frameCount = WavPcmReader(file, CaptureFormat()).use { it.frameCount }
        return CaptureRecord(
        captureId = captureId,
        createdAtUtc = STAMP,
        startedAtUtc = STAMP,
        finishedAtUtc = "2026-01-02T03:04:06Z",
        finishedLocalDate = "2026-01-02",
        timezoneId = "Etc/UTC",
        captureState = captureState,
        observedFrames = frameCount,
        durableFrames = frameCount,
        format = CaptureFormat(),
        localAudioPath = file.absolutePath,
        finalizedByteLength = file.length(),
        sha256 = "c".repeat(64),
        recognitionState = recognitionState,
        transcriptVersion = 0,
        archiveState = ArchiveState.VERIFIED,
        transferState = TransferState.PHONE_ARCHIVED,
        archivedAudioUri = "content://synthetic-notes/recording.wav",
        )
    }

    private fun writeWav(pcm: ByteArray, includeAncillaryChunk: Boolean = false): File {
        val format = CaptureFormat()
        val ancillary = if (includeAncillaryChunk) "JUNK".toByteArray(Charsets.US_ASCII) + byteArrayOf(2, 0, 0, 0, 0x41, 0x42) else byteArrayOf()
        val bytes = ByteArrayOutputStream().apply {
            write("RIFF".toByteArray(Charsets.US_ASCII)); writeLe32(36 + ancillary.size + pcm.size)
            write("WAVEfmt ".toByteArray(Charsets.US_ASCII)); writeLe32(16); writeLe16(1)
            writeLe16(format.channels); writeLe32(format.sampleRate); writeLe32(format.sampleRate * format.channels * 2)
            writeLe16(format.channels * 2); writeLe16(format.bitsPerSample)
            write(ancillary)
            write("data".toByteArray(Charsets.US_ASCII)); writeLe32(pcm.size); write(pcm)
        }.toByteArray()
        return File(context.cacheDir, "$captureId.wav").apply { FileOutputStream(this).use { it.write(bytes) } }
    }

    private fun ByteArrayOutputStream.writeLe16(value: Int) = write(ByteBuffer.allocate(2).order(ByteOrder.LITTLE_ENDIAN).putShort(value.toShort()).array())
    private fun ByteArrayOutputStream.writeLe32(value: Int) = write(ByteBuffer.allocate(4).order(ByteOrder.LITTLE_ENDIAN).putInt(value).array())

    private class FakeSpeech : SpeechRecognizerAdapter {
        var requestedLanguage: String? = null
        var networkFallback = false
        var supportsExternalAudio = true
        var started = false
        var startCount = 0
        var cancelCount = 0
        var finished = false
        var events: SpeechRecognizerAdapter.Events? = null
        var onAppend: (() -> Unit)? = null
        var beforeAppend: (() -> Unit)? = null
        var beforeFinish: (() -> Unit)? = null
        var beforeStart: (() -> Unit)? = null
        var beforeCapabilities: (() -> Unit)? = null
        var onStart: (() -> Unit)? = null
        var onFinish: (() -> Unit)? = null
        var onCancel: ((String) -> Unit)? = null
        val activeSessions = ConcurrentHashMap.newKeySet<String>()
        val received = ByteArrayOutputStream()
        override fun capabilities(): SpeechRecognizerAdapter.Capabilities {
            beforeCapabilities?.invoke()
            return SpeechRecognizerAdapter.Capabilities(
            setOf(CaptureFormat()), setOf("en-US"), true, true, true, supportsExternalAudio, supportsExternalAudio,
        )
        }
        override fun start(sessionId: String, format: CaptureFormat, language: String?, events: SpeechRecognizerAdapter.Events) {
            beforeStart?.invoke()
            if (!events.isCurrent()) return
            started = true
            startCount += 1
            this.events = events
            activeSessions += sessionId
            onStart?.invoke()
        }
        override fun startWithPolicy(sessionId: String, format: CaptureFormat, language: String?, networkFallback: Boolean,
                                     events: SpeechRecognizerAdapter.Events) {
            requestedLanguage = language
            this.networkFallback = networkFallback
            start(sessionId, format, language, events)
        }
        override fun append(sessionId: String, pcm: ByteArray) { beforeAppend?.invoke(); appendPcm(pcm) }
        override fun appendOwned(sessionId: String, owner: SpeechRecognizerAdapter.Events, pcm: ByteArray): Boolean {
            beforeAppend?.invoke()
            if (events !== owner || sessionId !in activeSessions || !owner.isCurrent()) return false
            appendPcm(pcm)
            return true
        }
        private fun appendPcm(pcm: ByteArray) { onAppend?.invoke(); received.write(pcm) }
        override fun finish(sessionId: String) { beforeFinish?.invoke(); finishInput() }
        override fun finishOwned(sessionId: String, owner: SpeechRecognizerAdapter.Events): Boolean {
            beforeFinish?.invoke()
            if (events !== owner || sessionId !in activeSessions || !owner.isCurrent()) return false
            finishInput()
            return true
        }
        private fun finishInput() { finished = true; onFinish?.invoke() }
        override fun cancel(sessionId: String) { onCancel?.invoke(sessionId); cancelCount += 1; activeSessions -= sessionId }
        fun complete(sessionId: String, text: String) {
            events!!.onHypothesis(text, true)
            activeSessions -= sessionId
        }
    }

    companion object {
        private const val STAMP = "2026-01-02T03:04:05Z"
    }
}
