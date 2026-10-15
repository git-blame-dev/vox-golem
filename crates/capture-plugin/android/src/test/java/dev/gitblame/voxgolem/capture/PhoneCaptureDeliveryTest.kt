package dev.gitblame.voxgolem.capture

import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class PhoneCaptureDeliveryTest {
    @Test fun oversizedFailureQueuedBeforeACorrectionIsSuppressedButCurrentFailureStillDelivers() {
        val context = RuntimeEnvironment.getApplication()
        val repository = CaptureRepository(context)
        val id = "123e4567-e89b-42d3-a456-426614174097"
        val oversized = CaptureRecord(id, createdAtUtc = "2026-01-02T03:04:05Z", captureState = CaptureState.SAVED,
            recognitionState = RecognitionState.FINAL, transcriptVersion = 1,
            finalText = "x".repeat(CaptureProtocol.MAX_CHANNEL_RESULT_BYTES + 1), sha256 = "a".repeat(64))
        assertNotNull(repository.admitPhoneCapture(oversized, "synthetic-node"))
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val sent = CopyOnWriteArrayList<JSONObject>()
        val delivery = PhoneCaptureDelivery(repository, { _, _, bytes ->
            val body = JSONObject(String(bytes, Charsets.UTF_8))
            if (body.optString("stage") == "AUTHORIZATION_FAILED") {
                entered.countDown(); check(release.await(5, TimeUnit.SECONDS))
            }
            sent += body
        }, { _, _, _ -> fail("Over-budget final must not use the receipt channel") })
        fun drain() {
            val done = CountDownLatch(1)
            delivery.barrier { done.countDown() }
            assertTrue(done.await(3, TimeUnit.SECONDS))
        }
        try {
            delivery.sendAuthorizationFailure("synthetic-node", id)
            assertTrue(entered.await(2, TimeUnit.SECONDS))
            delivery.sendReceipt("synthetic-node", oversized, "PHONE_STAGED")
            val correction = repository.correctTranscript(id, "Synthetic small correction", expectedVersion = 1)
            release.countDown(); drain()
            assertTrue(sent.none { it.optString("errorCode") == "final-transcript-exceeds-result-limit" })
            delivery.sendReceipt("synthetic-node", correction, "PHONE_STAGED"); drain()
            assertEquals(correction.finalText, sent.single { it.has("finalText") }.getString("finalText"))
            val currentOversized = repository.update(id) { it.copy(finalText = oversized.finalText, transcriptVersion = 3) }!!
            delivery.sendReceipt("synthetic-node", currentOversized, "PHONE_STAGED"); drain()
            val error = sent.single { it.optString("errorCode") == "final-transcript-exceeds-result-limit" }
            assertEquals("FAILED", error.getString("recognitionState"))
            assertFalse(error.has("finalText"))
        } finally { release.countDown(); delivery.shutdown() }
    }
}
