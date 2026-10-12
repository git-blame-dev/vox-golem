package dev.gitblame.voxgolem.capture

import android.net.Uri
import com.google.android.gms.wearable.MessageEvent
import dev.gitblame.voxgolem.assistant.recognition.RecognitionResourceLease
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.After
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.io.File

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class PhoneCaptureOriginAuthorizationTest {
    private val context = RuntimeEnvironment.getApplication()
    private val repository = CaptureRepository(context)
    private val id = "123e4567-e89b-42d3-a456-426614174061"
    private val session = "123e4567-e89b-42d3-a456-426614174062"
    private val a = "synthetic-watch-a"
    private val b = "synthetic-watch-b"
    private val messages = java.util.concurrent.CopyOnWriteArrayList<Pair<String, JSONObject>>()
    private val channels = java.util.concurrent.CopyOnWriteArrayList<Pair<String, JSONObject>>()
    private val exports = java.util.concurrent.CopyOnWriteArrayList<String>()
    private val speech = Speech()
    private var coordinator = coordinator()
    @Before fun setUp() {
        context.deleteDatabase("captures.db")
        File(File(context.filesDir, "captures"), id).deleteRecursively()
        org.robolectric.Shadows.shadowOf(context).grantPermissions(android.Manifest.permission.RECORD_AUDIO)
    }
    private fun coordinator() = PhoneCaptureCoordinator(context, speech,
        { node, _, bytes -> messages += node to JSONObject(String(bytes, Charsets.UTF_8)) },
        { Uri.parse("content://synthetic.notes/audio") },
        { capture -> exports += capture; OrgExportResult(OrgState.EXPORTED, "synthetic-destination") },
        replayPacingMillis = 0, runtimeReady = { true },
        sendOversizedReceipt = { node, _, bytes -> channels += node to JSONObject(String(bytes, Charsets.UTF_8)) },
        recognitionLeases = RecognitionResourceLease())
    @After fun close() { coordinator.shutdownForTests() }
    private fun archived(state: RecognitionState = RecognitionState.QUEUED, text: String? = null): CaptureRecord {
        val wav = File(context.cacheDir, "$id.wav")
        val spool = File(File(context.cacheDir, id), "synthetic.pcm")
        wav.delete(); spool.delete()
        val manifest = PcmRecordingWriter(spool, CaptureFormat()).use { writer ->
            writer.append(byteArrayOf(1, 0, 2, 0)); writer.finish(wav, id)
        }
        return CaptureRecord(id, createdAtUtc = "2026-10-01T00:00:00Z", captureState = CaptureState.SAVED,
            localAudioPath = wav.absolutePath, finalizedByteLength = manifest.wavByteLength, sha256 = manifest.sha256,
            observedFrames = manifest.frameCount, durableFrames = manifest.frameCount,
            recognitionState = state, finalText = text, transcriptVersion = if (text == null) 0 else 1,
            archiveState = ArchiveState.VERIFIED, transferState = TransferState.PHONE_ARCHIVED,
            archivedAudioUri = "content://synthetic.notes/audio", orgState = OrgState.READY)
    }
    @Test fun unauthorizedStatusRetryAndMatchingArchiveCannotSelectRecognitionDeliveryNodeForSmallOrLargeFinals() {
        val record = archived()
        coordinator.onArchiveReceived(record, a)
        assertTrue(coordinator.awaitIdle())
        assertEquals(a, repository.phoneOrigin(id)); assertEquals(1, speech.starts)
        messages.clear(); channels.clear()
        coordinator.respondStatus(id, b); coordinator.retryCapture(id, b)
        assertThrows(CaptureOriginAuthorizationException::class.java) { coordinator.onArchiveReceived(record, b) }
        assertTrue(coordinator.awaitIdle())
        assertEquals(1, speech.starts); assertTrue(channels.isEmpty())
        assertFalse(messages.isEmpty())
        assertTrue(messages.all { it.first == b && it.second.optString("errorCode") == "capture-origin-not-authorized" })
        speech.events!!.onHypothesis("Synthetic private A final", true)
        assertTrue(coordinator.awaitIdle())
        assertTrue(messages.any { it.first == a && it.second.optString("finalText") == "Synthetic private A final" })
        assertTrue(messages.none { it.first == b && (it.second.has("finalText") || it.second.has("fullText") || it.second.has("archiveHash")) })
        val large = "Synthetic private A large final ".repeat(2_000)
        repository.update(id) { it.copy(finalText = large, transcriptVersion = 2) }
        messages.clear(); channels.clear()
        coordinator.shutdownForTests(); coordinator = coordinator()
        coordinator.respondStatus(id, b); assertTrue(coordinator.awaitIdle())
        assertTrue(messages.none { it.second.has("finalText") || it.second.has("archiveHash") }); assertTrue(channels.isEmpty())
        coordinator.respondStatus(id, a); assertTrue(coordinator.awaitIdle())
        assertEquals(a, channels.single().first); assertEquals(large, channels.single().second.getString("finalText"))
        assertEquals(a, CaptureRepository(context).phoneOrigin(id))
        assertNull(repository.admitPhoneCapture(record, b))
        assertEquals(large, repository.get(id)!!.finalText)
    }
    @Test fun legacyUnboundCapturePreservesLocalAudioHistoryAndExportButCannotBindFromRemoteRequests() {
        val record = archived(RecognitionState.FINAL, "Synthetic legacy private transcript")
        repository.save(record)
        val audio = File(record.localAudioPath!!).readBytes()
        for (node in listOf(a, b)) {
            coordinator.respondStatus(id, node); coordinator.retryCapture(id, node)
            assertThrows(CaptureOriginAuthorizationException::class.java) { coordinator.onArchiveReceived(record, node) }
        }
        assertTrue(coordinator.awaitIdle())
        assertEquals(0, speech.starts); assertTrue(channels.isEmpty())
        assertTrue(messages.all { it.second.optString("stage") == "AUTHORIZATION_FAILED" })
        assertTrue(messages.none { it.second.has("finalText") || it.second.has("archiveHash") })
        assertNull(CaptureRepository(context).phoneOrigin(id)); assertEquals(record, repository.get(id))
        assertArrayEquals(audio, File(record.localAudioPath).readBytes())
        coordinator.retryCapture(id)
        messages.clear()
        assertTrue(coordinator.awaitIdle()); assertTrue(exports.contains(id))
        assertTrue(messages.isEmpty()); assertTrue(channels.isEmpty())
        assertEquals(record.finalText, repository.get(id)!!.finalText)
    }
    @Test fun actualGmsSourceBindsLiveCaptureNotJsonClaimAndOtherWatchCannotJoinItsSession() {
        val receiver = CaptureChannelReceiver(context, coordinator)
        val body = JSONObject().put("protocol", CaptureProtocol.PROTOCOL).put("captureId", id).put("sessionId", session)
            .put("sampleRate", 16_000).put("channels", 1).put("bitsPerSample", 16)
            .put("startedAt", "2026-10-01T00:00:00Z").put("sourceNodeId", b)
        fun message(node: String) = object : MessageEvent {
            override fun getSourceNodeId() = node
            override fun getPath() = CaptureProtocol.START
            override fun getData() = body.toString().toByteArray(Charsets.UTF_8)
            override fun getRequestId() = 1
        }
        try {
            receiver.onMessageReceived(message(a)); receiver.onMessageReceived(message(b))
            assertEquals(a, repository.phoneOrigin(id)); assertEquals(1, speech.starts)
            assertFalse(coordinator.registerLiveInput(id, session, b) {})
            speech.events!!.onHypothesis("Synthetic A preview", false)
            assertTrue(coordinator.awaitIdle())
            assertTrue(messages.none { it.first == b && it.second.has("fullText") })
            assertTrue(messages.any { it.first == a && it.second.optString("fullText") == "Synthetic A preview" })
        } finally { receiver.shutdown() }
    }
    @Test fun legacyLocalReplayCanPersistAndExportWithoutInventingARemoteOrigin() {
        repository.save(archived())
        coordinator.retryCapture(id)
        assertTrue(coordinator.awaitIdle()); assertEquals(1, speech.starts)
        speech.events!!.onHypothesis("Synthetic legacy local final", true)
        assertTrue(coordinator.awaitIdle())
        assertEquals("Synthetic legacy local final", repository.get(id)!!.finalText)
        assertTrue(exports.contains(id)); assertNull(repository.phoneOrigin(id))
        assertTrue(messages.isEmpty()); assertTrue(channels.isEmpty())
        assertTrue(File(repository.get(id)!!.localAudioPath!!).isFile)
    }
    @Test fun startCannotClaimOrphanedStagedAudioThenUseMatchingHeaderAsOriginEvidence() {
        val directory = File(File(context.filesDir, "captures"), id).apply { mkdirs() }
        val orphan = File(directory, "received.wav").apply { writeBytes(byteArrayOf(1, 2, 3, 4)) }
        val bytes = orphan.readBytes()
        coordinator.beginLive(id, session, b, CaptureFormat(), "2026-10-01T00:00:00Z")
        assertTrue(coordinator.awaitIdle())
        assertEquals(0, speech.starts); assertNull(repository.get(id)); assertNull(repository.phoneOrigin(id))
        assertFalse(coordinator.canReuseStagedArchive(id, b))
        assertArrayEquals(bytes, orphan.readBytes())
        assertTrue(messages.none { it.second.has("finalText") || it.second.has("fullText") || it.second.has("archiveHash") })
    }
    private class Speech : SpeechRecognizerAdapter {
        var starts = 0
        var events: SpeechRecognizerAdapter.Events? = null
        override fun capabilities() = SpeechRecognizerAdapter.Capabilities(setOf(CaptureFormat()), setOf("en-US"), true, true, true, true, true)
        override fun start(sessionId: String, format: CaptureFormat, language: String?, events: SpeechRecognizerAdapter.Events) { starts++; this.events = events }
        override fun append(sessionId: String, pcm: ByteArray) {}
        override fun finish(sessionId: String) {}
        override fun cancel(sessionId: String) {}
    }
}
