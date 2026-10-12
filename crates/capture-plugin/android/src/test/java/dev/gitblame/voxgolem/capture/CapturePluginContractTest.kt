package dev.gitblame.voxgolem.capture

import android.app.Activity
import android.Manifest
import androidx.activity.ComponentActivity
import app.tauri.plugin.Invoke
import org.junit.After
import org.junit.Assert.assertEquals
import org.json.JSONObject
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class CapturePluginContractTest {
    @Test fun updateTranscriptCommandRejectsEveryNonNotePurposeWithoutChangingAnyRecordFields() {
        val activity = Robolectric.buildActivity(Activity::class.java).setup().get()
        val repository = CaptureRepository(activity)
        val plugin = CapturePlugin(activity)
        for ((index, purpose) in listOf(CapturePurpose.ASK, CapturePurpose.TALK, CapturePurpose.DRAFT).withIndex()) {
            val record = repository.save(CaptureRecord(
                captureId = "123e4567-e89b-42d3-a456-42661417405$index", createdAtUtc = "2026-01-02T03:04:05Z",
                purpose = purpose, captureState = CaptureState.SAVED, orgState = OrgState.NONE,
                recognitionState = RecognitionState.FINAL, finalText = "Synthetic original words", previewText = "Synthetic original words",
                transcriptVersion = 3, requestedLanguage = "en-AU", speechNetworkFallback = false,
                localAudioPath = "/synthetic/private/retained.wav", finalizedByteLength = 48, sha256 = "a".repeat(64),
            ))
            for (replacement in listOf("Synthetic unauthorized edit", "")) {
                val responses = mutableListOf<Pair<Long, String>>()
                val args = JSONObject().put("captureId", record.captureId).put("text", replacement).toString()
                plugin.update_transcript(invoke("update_transcript", args) { callback, value -> responses += callback to value })
                assertEquals(11L, responses.single().first)
                assertEquals("transcript-update-unavailable", JSONObject(responses.single().second).getString("message"))
                assertEquals("Rejected $purpose edit changed stored capture metadata", record, repository.get(record.captureId))
            }
        }
    }

    @Test fun updateTranscriptCommandStillAcceptsFinalizedNoteEditsAndBlankCorrections() {
        val activity = Robolectric.buildActivity(Activity::class.java).setup().get()
        val repository = CaptureRepository(activity)
        val plugin = CapturePlugin(activity)
        val original = repository.save(CaptureRecord("123e4567-e89b-42d3-a456-426614174053",
            createdAtUtc = "2026-01-02T03:04:05Z", captureState = CaptureState.SAVED, purpose = CapturePurpose.NOTE,
            recognitionState = RecognitionState.FINAL, finalText = "Synthetic original note", transcriptVersion = 3,
            orgState = OrgState.WAITING_FOR_TRANSCRIPT))
        val responses = mutableListOf<Pair<Long, String>>()
        val sender: (Long, String) -> Unit = { callback, value -> responses += callback to value }
        plugin.update_transcript(invoke("update_transcript", JSONObject().put("captureId", original.captureId)
            .put("text", "Synthetic corrected note").toString(), sender))
        assertEquals(10L, responses.single().first)
        assertTrue(PhoneCaptureCoordinator.shared(activity).awaitIdle())
        val corrected = repository.get(original.captureId)!!
        assertEquals("Synthetic corrected note", corrected.finalText)
        assertEquals(corrected.finalText, corrected.previewText)
        assertEquals(RecognitionState.FINAL, corrected.recognitionState)
        assertEquals(4L, corrected.transcriptVersion)
        assertEquals(OrgState.READY, corrected.orgState)
        responses.clear()
        plugin.update_transcript(invoke("update_transcript", JSONObject().put("captureId", original.captureId)
            .put("text", "").toString(), sender))
        assertEquals(10L, responses.single().first)
        val empty = repository.get(original.captureId)!!
        assertEquals("", empty.finalText)
        assertEquals("", empty.previewText)
        assertEquals(RecognitionState.NO_SPEECH, empty.recognitionState)
        assertEquals(5L, empty.transcriptVersion)
        assertEquals(OrgState.WAITING_FOR_TRANSCRIPT, empty.orgState)
    }

    @After fun tearDown() {
        PhoneCaptureCoordinator.shared(RuntimeEnvironment.getApplication()).shutdownForTests()
        PhoneCaptureCoordinator.resetSharedForTests()
    }

    @Test fun consentWithoutForegroundReadinessProjectsStartingNotSetupFailure() {
        val activity = Robolectric.buildActivity(Activity::class.java).setup().get()
        shadowOf(RuntimeEnvironment.getApplication()).grantPermissions(
            Manifest.permission.RECORD_AUDIO, Manifest.permission.BLUETOOTH_CONNECT, Manifest.permission.POST_NOTIFICATIONS,
        )
        activity.getSharedPreferences(PhoneCaptureCoordinator.PREFERENCES, Activity.MODE_PRIVATE).edit()
            .putBoolean(PhoneCaptureCoordinator.KEY_OPTED_IN, true).commit()
        val plugin = CapturePlugin(activity)
        val method = CapturePlugin::class.java.getDeclaredMethod("snapshotValue").apply { isAccessible = true }
        val snapshot = method.invoke(plugin) as JSONObject
        assertTrue(snapshot.getBoolean("setupRequired"))
        assertEquals("starting", snapshot.getString("onboardingState"))
        assertTrue(snapshot.isNull("onboardingError"))
    }

    @Test fun foregroundReadinessAndAsynchronousFailureOverrideStartingWithoutConflatingFolderReadiness() {
        val starting = NativeProjectionStatus(true, false, null, null)
        assertEquals("starting", starting.onboarding().state)
        val ready = starting.copy(foregroundReady = true)
        val snapshot = NativeCaptureProjection.snapshot(null, emptyList(), ready,
            PhoneCaptureCoordinator.EnableResult("starting"))
        assertEquals("enabled", snapshot.getString("onboardingState"))
        assertFalse(snapshot.getBoolean("setupRequired"))
        assertTrue(snapshot.isNull("notesFolderUri"))
        val failed = NativeCaptureProjection.snapshot(null, emptyList(), ready.copy(runtimeError = "synthetic-service-denied"),
            PhoneCaptureCoordinator.EnableResult("starting"))
        assertEquals("failed", failed.getString("onboardingState"))
        assertEquals("capture-operation-failed", failed.getString("onboardingError"))
        assertTrue(failed.getBoolean("setupRequired"))
    }

    @Test fun revokedPermissionsAreNotProjectedAsEnabledEvenIfTheServiceWasReady() {
        val status = NativeProjectionStatus(true, true, null, "record-audio-permission-required")
        val snapshot = NativeCaptureProjection.snapshot(null, emptyList(), status)
        assertEquals("permission-required", snapshot.getString("onboardingState"))
        assertEquals("record-audio-permission-required", snapshot.getString("onboardingError"))
        assertTrue(snapshot.getBoolean("setupRequired"))
    }

    @Test fun publicRecordProjectionDoesNotLeakPrivateStorageOrPromotePreview() {
        val record = CaptureRecord("123e4567-e89b-42d3-a456-426614174030", createdAtUtc = "2026-01-02T03:04:05Z",
            localAudioPath = "/synthetic/private/audio.wav", sha256 = "a".repeat(64),
            recognitionSessionId = "123e4567-e89b-42d3-a456-426614174031", previewText = "synthetic preview",
            exportDestinationId = "synthetic-private-destination")
        val projected = NativeCaptureProjection.record(record)
        for (field in listOf("localAudioPath", "sha256", "recognitionSessionId", "exportDestinationId", "completionNotified")) {
            assertFalse("Private field $field", projected.has(field))
        }
        assertEquals("synthetic preview", projected.getString("previewText"))
        assertTrue(projected.isNull("finalText"))
        assertEquals("NOTE", projected.getString("purpose"))
        assertFalse(projected.getBoolean("speechNetworkFallback"))
        assertTrue(projected.isNull("requestedLanguage"))
        assertEquals("NOT_REQUESTED", projected.getString("liveTranscriptionState"))
        assertFalse(projected.getBoolean("canTranscribeLive"))
        assertTrue(projected.isNull("liveErrorCode"))
    }

    @Test fun nativeCommandsReturnTheSnapshotAndListEnvelopesExpectedByTheRustBridge() {
        val activity = Robolectric.buildActivity(Activity::class.java).setup().get()
        val plugin = CapturePlugin(activity)
        val responses = mutableListOf<Pair<Long, String>>()
        val sender: (Long, String) -> Unit = { callback, value -> responses += callback to value }
        plugin.snapshot(invoke("snapshot", "{}", sender))
        val snapshot = JSONObject(responses.single().second)
        assertEquals(10L, responses.single().first)
        assertTrue(snapshot.getBoolean("available"))
        assertEquals("setup-required", snapshot.getString("onboardingState"))
        assertTrue(snapshot.isNull("onboardingError"))
        responses.clear()
        plugin.list_captures(invoke("list_captures", "{\"offset\":0,\"limit\":10}", sender))
        assertEquals(10L, responses.single().first)
        assertEquals(setOf("captures"), JSONObject(responses.single().second).keys().asSequence().toSet())
        assertEquals(0, JSONObject(responses.single().second).getJSONArray("captures").length())
    }

    @Test fun explicitEnableFromAHiddenActivityCannotPersistConsentOrStartTheRuntime() {
        val activity = Robolectric.buildActivity(ComponentActivity::class.java).create().start()
        val responses = mutableListOf<Pair<Long, String>>()
        val sender: (Long, String) -> Unit = { callback, value -> responses += callback to value }
        CapturePlugin(activity.get()).enable_capture(invoke("enable_capture", "{}", sender))
        assertEquals(11L, responses.single().first)
        assertEquals("Phone capture can only be enabled from the visible app", JSONObject(responses.single().second).getString("message"))
        assertFalse(activity.get().getSharedPreferences(PhoneCaptureCoordinator.PREFERENCES, Activity.MODE_PRIVATE)
            .getBoolean(PhoneCaptureCoordinator.KEY_OPTED_IN, false))
        assertEquals(null, shadowOf(RuntimeEnvironment.getApplication()).nextStartedService)
        activity.stop().destroy()
    }

    private fun invoke(command: String, args: String, sender: (Long, String) -> Unit): Invoke {
        // Tauri owns Jackson as an implementation dependency; do not add a direct test dependency.
        val mapper = Class.forName("com.fasterxml.jackson.databind.ObjectMapper").getConstructor().newInstance()
        return Invoke::class.java.constructors.single().newInstance(1L, command, 10L, 11L, sender, args, mapper) as Invoke
    }
    @Test
    fun emptySnapshotRetainsRequiredNullableFieldsForFrontendValidation() {
        val activity = Robolectric.buildActivity(Activity::class.java).setup().get()
        val plugin = CapturePlugin(activity)
        val method = CapturePlugin::class.java.getDeclaredMethod("snapshotValue").apply { isAccessible = true }
        val snapshot = JSONObject((method.invoke(plugin) as JSONObject).toString())

        assertTrue(snapshot.has("notesFolderUri"))
        assertTrue(snapshot.isNull("notesFolderUri"))
        assertTrue(snapshot.has("error"))
        assertTrue(snapshot.isNull("error"))
        assertTrue(snapshot.getBoolean("setupRequired"))
        assertTrue(snapshot.getJSONArray("captures").length() == 0)
    }

    @Test
    fun pendingRecordRetainsEveryNullableFieldWithoutPromotingPreviewToFinal() {
        val activity = Robolectric.buildActivity(Activity::class.java).setup().get()
        val plugin = CapturePlugin(activity)
        val record = CaptureRecord(
            captureId = "123e4567-e89b-42d3-a456-426614174030",
            createdAtUtc = "2026-01-02T03:04:05Z",
            previewText = "synthetic preview",
        )
        val method = CapturePlugin::class.java.getDeclaredMethod("recordValue", CaptureRecord::class.java)
            .apply { isAccessible = true }
        val result = JSONObject((method.invoke(plugin, record) as JSONObject).toString())

        for (field in listOf("finishedAtUtc", "finalText", "archivedAudioUri", "error")) {
            assertTrue("Missing required field $field", result.has(field))
            assertTrue("Expected null field $field", result.isNull(field))
        }
        assertFalse(result.isNull("previewText"))
        assertTrue(result.getString("previewText") == "synthetic preview")
    }
}
