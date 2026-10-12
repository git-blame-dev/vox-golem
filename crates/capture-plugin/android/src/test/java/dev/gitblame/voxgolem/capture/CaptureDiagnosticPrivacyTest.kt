package dev.gitblame.voxgolem.capture

import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class CaptureDiagnosticPrivacyTest {
    private val context = RuntimeEnvironment.getApplication()
    private val id = "123e4567-e89b-42d3-a456-426614174020"
    private val secret = "Synthetic /storage/synthetic-person/private-note.wav content://synthetic.private/tree/private-document"
    private fun record() = CaptureRecord(id, createdAtUtc = "2026-10-01T00:00:00Z", captureState = CaptureState.SAVED,
        recognitionState = RecognitionState.FINAL, finalText = "Synthetic transcript", sha256 = "a".repeat(64),
        archiveState = ArchiveState.STAGED)

    @Test fun archiveExceptionPersistsFixedCodeAndProjectionAndReceiptDoNotLeakHistoricalExceptions() {
        val repository = CaptureRepository(context)
        repository.admitPhoneCapture(record(), "synthetic-watch")
        val receipts = java.util.concurrent.CopyOnWriteArrayList<JSONObject>()
        val delivery = PhoneCaptureDelivery(repository, { _, _, bytes -> receipts += JSONObject(String(bytes, Charsets.UTF_8)) }, { _, _, _ -> })
        val archive = PhoneCaptureArchive(repository, { throw java.io.IOException(secret) },
            { OrgExportResult(OrgState.EXPORTED, "synthetic-destination") }, delivery)
        try {
            archive.queue(id, "synthetic-watch")
            val done = CountDownLatch(1); archive.barrier { done.countDown() }
            assertTrue(done.await(5, TimeUnit.SECONDS))
            assertEquals(ArchiveState.RETRYABLE_ERROR, repository.get(id)!!.archiveState)
            assertEquals("archive-failed", repository.get(id)!!.lastErrorCode)
            val legacy = record().copy(lastErrorStage = "recognition", lastErrorCode = secret)
            val projection = NativeCaptureProjection.record(legacy)
            assertEquals("capture-operation-failed", projection.getString("error"))
            assertFalse(projection.toString().contains(secret))
            delivery.sendReceipt("synthetic-watch", legacy, "PHONE_STAGED")
            val sent = CountDownLatch(1); delivery.barrier { sent.countDown() }
            assertTrue(sent.await(5, TimeUnit.SECONDS))
            assertEquals("capture-operation-failed", receipts.single().getString("errorCode"))
            assertFalse(receipts.single().toString().contains("synthetic-person"))
            assertFalse(receipts.single().toString().contains("content://synthetic.private"))
        } finally { archive.shutdown(); delivery.shutdown() }
    }
    @Test fun exportedFailureAndRuntimeProjectionDoNotPublishExceptionText() {
        val repository = CaptureRepository(context)
        val saved = record().copy(archiveState = ArchiveState.VERIFIED, archivedAudioUri = "content://synthetic.notes/audio")
        repository.save(saved)
        val delivery = PhoneCaptureDelivery(repository, { _, _, _ -> }, { _, _, _ -> })
        val archive = PhoneCaptureArchive(repository, { android.net.Uri.EMPTY }, { throw SecurityException(secret) }, delivery)
        try {
            archive.maybeExport(saved)
            val done = CountDownLatch(1); archive.barrier { done.countDown() }
            assertTrue(done.await(5, TimeUnit.SECONDS))
            assertEquals("org-export-failed", repository.get(id)!!.lastErrorCode)
            val snapshot = NativeCaptureProjection.snapshot(null, emptyList(), NativeProjectionStatus(true, false, secret, null))
            assertEquals("capture-operation-failed", snapshot.getString("error"))
            assertEquals("capture-operation-failed", snapshot.getString("onboardingError"))
            assertFalse(snapshot.toString().contains(secret))
        } finally { archive.shutdown(); delivery.shutdown() }
    }
}
