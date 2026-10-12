package dev.gitblame.voxgolem.android.wear.transport

import android.os.Vibrator
import com.google.android.gms.tasks.Tasks
import com.google.android.gms.wearable.ChannelClient
import com.google.android.gms.wearable.MessageEvent
import dev.gitblame.voxgolem.android.wear.capture.CaptureRepository
import dev.gitblame.voxgolem.android.wear.capture.CaptureSignals
import dev.gitblame.voxgolem.android.wear.capture.PcmWavWriter
import dev.gitblame.voxgolem.capture.CaptureProtocol
import dev.gitblame.voxgolem.capture.CaptureReceiptChannel
import dev.gitblame.voxgolem.capture.CaptureChannelLifetime
import dev.gitblame.voxgolem.capture.CaptureChannelRetirementOwner
import dev.gitblame.voxgolem.capture.CaptureRecord
import dev.gitblame.voxgolem.capture.CaptureState
import dev.gitblame.voxgolem.capture.RecognitionState
import dev.gitblame.voxgolem.capture.TransferState
import dev.gitblame.voxgolem.capture.CaptureRepository as SharedRepository
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import java.io.ByteArrayInputStream
import java.io.IOException
import java.util.UUID

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class WearReceiptHandlerTest {
    private val context = RuntimeEnvironment.getApplication()
    private val shared = SharedRepository(context)
    private val vibrator = context.getSystemService(Vibrator::class.java)
    private val vibration = shadowOf(vibrator)

    private fun listener() = Robolectric.buildService(CaptureWearListenerService::class.java).get().apply {
        selectedReceiver = { it == "synthetic-selected-phone" }
        receiptRetirements = CaptureChannelRetirementOwner(10)
        observeReceiptClosures = {}
    }

    private fun savedAudio(createdAt: String = "2026-10-04T12:30:00Z"): CaptureRecord {
        val id = UUID.randomUUID().toString()
        val watch = CaptureRepository(context)
        val manifest = PcmWavWriter(shared.createPcmSpool(id), watch.audio(id), 16_000).use { writer ->
            writer.append(byteArrayOf(1, 0, 2, 0))
            writer.finish()
        }
        return shared.save(CaptureRecord(
            captureId = id, createdAtUtc = createdAt, captureState = CaptureState.SAVED,
            localAudioPath = watch.audio(id).absolutePath, finalizedByteLength = manifest.wavByteLength,
            observedFrames = manifest.frameCount, durableFrames = manifest.frameCount,
            sha256 = manifest.sha256, recognitionSessionId = UUID.randomUUID().toString(),
            transferState = TransferState.PENDING,
        ))
    }

    private fun receipt(record: CaptureRecord, text: String = "Synthetic final transcript") = JSONObject()
        .put("protocol", CaptureProtocol.PROTOCOL).put("captureId", record.captureId)
        .put("archiveHash", record.sha256).put("stage", "PHONE_ARCHIVED")
        .put("recognitionState", "FINAL").put("finalText", text).put("transcriptVersion", 1)

    private fun dispatch(service: CaptureWearListenerService, body: JSONObject, path: String = CaptureProtocol.RECEIPT,
                         source: String = "synthetic-selected-phone") {
        service.onMessageReceived(object : MessageEvent {
            override fun getData() = body.toString().toByteArray(Charsets.UTF_8)
            override fun getPath() = path
            override fun getSourceNodeId() = source
            override fun getRequestId() = 1
        })
    }

    private fun signals(state: String) = shadowOf(context).broadcastIntents.filter {
        it.action == CaptureSignals.SIGNAL_ACTION && it.getStringExtra("state") == state
    }

    private fun assertCompleted(record: CaptureRecord, text: String) {
        val persisted = CaptureRepository(context).get(record.captureId)!!
        assertEquals(text, persisted.finalText)
        assertEquals(RecognitionState.FINAL, persisted.recognitionState)
        assertEquals(TransferState.PHONE_ARCHIVED, persisted.transferState)
        assertTrue(persisted.completionNotified)
        assertArrayEquals(longArrayOf(0, 2_000), vibration.pattern)
        assertEquals(-1, vibration.repeat)
        assertTrue(vibration.isVibrating)
        assertEquals(1, signals("completed").size)
        assertEquals(record.captureId, signals("completed").single().getStringExtra("captureId"))
    }

    @Test fun messageReceiptPersistsFinalAndInvokesOneTwoSecondEffectAcrossListenerAndRepositoryRecreation() {
        val record = savedAudio()
        val body = receipt(record)
        dispatch(listener(), body)
        assertCompleted(record, "Synthetic final transcript")
        vibrator.cancel()
        assertFalse(vibration.isVibrating)
        assertTrue(CaptureRepository(context).get(record.captureId)!!.completionNotified)
        dispatch(listener(), body)
        dispatch(listener(), receipt(record).put("recognitionState", "FAILED").put("errorCode", "synthetic-stale-error"))
        assertFalse(vibration.isVibrating)
        assertEquals(1, signals("completed").size)
        assertTrue(signals("failed").isEmpty())
        assertEquals("Synthetic final transcript", CaptureRepository(context).get(record.captureId)!!.finalText)
    }

    @Test fun queuedChannelReceiptOwnsItsStreamAndSurvivesTransientListenerDestruction() {
        val record = savedAudio()
        val bytes = frame(receipt(record).toString().toByteArray(Charsets.UTF_8))
        val opened = java.util.concurrent.CountDownLatch(1)
        val release = java.util.concurrent.CountDownLatch(1)
        val closed = java.util.concurrent.CountDownLatch(1)
        val acknowledged = java.util.concurrent.CountDownLatch(1)
        val nativeClosed = java.util.concurrent.CountDownLatch(1)
        val acknowledgement = object : java.io.ByteArrayOutputStream() {
            override fun close() { acknowledged.countDown() }
        }
        val controller = Robolectric.buildService(CaptureWearListenerService::class.java).create()
        val service = controller.get()
        service.receiptRetirements = CaptureChannelRetirementOwner(1)
        service.observeReceiptClosures = {}
        service.selectedReceiver = { it == "synthetic-selected-phone" }
        service.receiptStream = {
            opened.countDown()
            check(release.await(5, java.util.concurrent.TimeUnit.SECONDS))
            Tasks.forResult(object : java.io.ByteArrayInputStream(bytes) { override fun close() { super.close(); closed.countDown() } })
        }
        service.closeReceiptChannel = { nativeClosed.countDown(); Tasks.forResult(null) }
        service.receiptOutput = { Tasks.forResult(acknowledgement) }
        val incoming = channel(record)
        try {
            service.onChannelOpened(incoming)
            assertTrue(opened.await(2, java.util.concurrent.TimeUnit.SECONDS))
            controller.destroy()
            release.countDown()
            assertTrue(closed.await(2, java.util.concurrent.TimeUnit.SECONDS))
            val deadline = System.nanoTime() + java.util.concurrent.TimeUnit.SECONDS.toNanos(5)
            while (signals("completed").isEmpty()) {
                check(System.nanoTime() < deadline); Thread.sleep(1)
            }
            assertCompleted(record, "Synthetic final transcript")
            assertTrue(acknowledged.await(2, java.util.concurrent.TimeUnit.SECONDS))
            assertArrayEquals(byteArrayOf(CaptureReceiptChannel.CONSUMED.toByte()), acknowledgement.toByteArray())
            assertEquals(1L, nativeClosed.count)
            service.onChannelClosed(incoming, 0, 0)
            await { service.receiptRetirements.size == 0 }
            assertEquals(1L, nativeClosed.count)
            assertTrue(java.io.File(record.localAudioPath!!).isFile)
        } finally { release.countDown(); service.onChannelClosed(incoming, 0, 0) }
    }

    @Test fun productionChannelEntryRejectsUnselectedPhoneWithoutInputAllocationAndRetiresLateInputAfterDeadline() {
        val record = savedAudio()
        val service = listener()
        var acquisitions = 0
        val closed = java.util.concurrent.CountDownLatch(1)
        val requested = java.util.concurrent.CountDownLatch(1)
        val pending = com.google.android.gms.tasks.TaskCompletionSource<java.io.InputStream>()
        service.receiptTimeoutMs = 20
        service.closeReceiptChannel = { closed.countDown(); Tasks.forResult(null) }
        service.receiptStream = { acquisitions++; requested.countDown(); pending.task }
        service.onChannelOpened(channel(record, "synthetic-other-phone"))
        assertTrue(closed.await(2, java.util.concurrent.TimeUnit.SECONDS))
        assertEquals(0, acquisitions)
        val selectedClosed = java.util.concurrent.CountDownLatch(1)
        service.closeReceiptChannel = { selectedClosed.countDown(); Tasks.forResult(null) }
        service.onChannelOpened(channel(record))
        assertTrue(requested.await(2, java.util.concurrent.TimeUnit.SECONDS))
        assertTrue(selectedClosed.await(2, java.util.concurrent.TimeUnit.SECONDS))
        val streamClosed = java.util.concurrent.CountDownLatch(1)
        pending.setResult(object : ByteArrayInputStream(frame(receipt(record).toString().toByteArray())) {
            override fun close() { streamClosed.countDown(); super.close() }
        })
        assertTrue(streamClosed.await(2, java.util.concurrent.TimeUnit.SECONDS))
        assertEquals(record, CaptureRepository(context).get(record.captureId))
        assertTrue(signals("completed").isEmpty())
    }

    @Test fun selectedPhoneChangeOrLateReadCannotCommitReceiptAndPrivatePeerErrorIsRedacted() {
        val record = savedAudio()
        val service = listener()
        val framed = frame(receipt(record).toString().toByteArray())
        val stream = object : ByteArrayInputStream(framed) {
            override fun read(): Int {
                val result = super.read()
                if (result == -1) service.selectedReceiver = { false }
                return result
            }
        }
        localLifetime().use { lifetime ->
            lifetime.own(stream)
            service.receiveChannelReceipt(CaptureReceiptChannel.path(record.captureId), "synthetic-selected-phone", stream, lifetime)
        }
        assertEquals(record, CaptureRepository(context).get(record.captureId))
        localLifetime().use { retired ->
            retired.beginStreamAcquisition()
            retired.close()
            service.selectedReceiver = { true }
            service.receiveChannelReceipt(CaptureReceiptChannel.path(record.captureId), "synthetic-selected-phone", framed.inputStream(), retired)
            retired.streamAcquisitionCompleted(null)
        }
        assertEquals(record, CaptureRepository(context).get(record.captureId))
        val privateError = "Synthetic /storage/synthetic-person/voice.wav content://synthetic.private/tree/note"
        dispatch(listener(), receipt(record).put("recognitionState", "FAILED").put("errorCode", privateError).removeFinalText())
        assertEquals("transcription-unavailable", CaptureRepository(context).get(record.captureId)!!.lastErrorCode)
        assertFalse(signals("failed").single().getStringExtra("message")!!.contains("synthetic-person"))
        assertFalse(signals("failed").single().getStringExtra("message")!!.contains("content://"))
    }

    @Test fun asynchronousNativeCloseFailureRetainsWearAdmissionUntilActualRecoveryCompletes() {
        val record = savedAudio()
        val service = listener().apply { receiptRetirements = CaptureChannelRetirementOwner(1) }
        val incoming = channel(record, "synthetic-other-phone")
        val pending = com.google.android.gms.tasks.TaskCompletionSource<Void>()
        val result = java.util.concurrent.atomic.AtomicReference(pending.task)
        val attempts = java.util.concurrent.CopyOnWriteArrayList<ChannelClient.Channel>()
        var inputAllocated = false
        service.receiptStream = { inputAllocated = true; Tasks.forResult(byteArrayOf(1).inputStream()) }
        service.closeReceiptChannel = { attempts += it; result.get() }
        service.onChannelOpened(incoming)
        await { attempts.size == 1 }
        assertEquals(1, service.receiptRetirements.size)
        pending.setException(IllegalStateException("Synthetic private channel close failure"))
        await { attempts.size == 3 }
        assertEquals(1, service.receiptRetirements.size)
        assertFalse(inputAllocated); assertTrue(attempts.all { it === incoming })
        result.set(Tasks.forResult(null)); service.receiptRetirements.retryFailedRetirements()
        await { service.receiptRetirements.size == 0 }
        assertTrue(attempts.all { it === incoming })
        assertEquals(record, CaptureRepository(context).get(record.captureId))
    }

    @Test fun synchronousNativeCloseFailureAndStaleCallbackCannotFreeReplacementWithALateInput() {
        val record = savedAudio()
        val service = listener().apply { receiptRetirements = CaptureChannelRetirementOwner(1) }
        val old = channel(record, "synthetic-other-phone")
        val attempts = java.util.concurrent.atomic.AtomicInteger()
        service.closeReceiptChannel = { attempts.incrementAndGet(); throw IllegalStateException("Synthetic synchronous close failure") }
        service.onChannelOpened(old)
        await { attempts.get() == 3 }
        assertEquals(1, service.receiptRetirements.size)
        service.onChannelClosed(old, 0, 0)
        await { service.receiptRetirements.size == 0 }
        val replacement = channel(record)
        val pending = com.google.android.gms.tasks.TaskCompletionSource<java.io.InputStream>()
        val requested = java.util.concurrent.CountDownLatch(1)
        service.receiptStream = { requested.countDown(); pending.task }
        service.closeReceiptChannel = { Tasks.forResult(null) }
        service.onChannelOpened(replacement)
        assertTrue(requested.await(2, java.util.concurrent.TimeUnit.SECONDS))
        service.onChannelClosed(old, 0, 0)
        assertEquals(1, service.receiptRetirements.size)
        service.onChannelClosed(replacement, 0, 0)
        assertEquals(1, service.receiptRetirements.size)
        val closed = java.util.concurrent.CountDownLatch(1)
        pending.setResult(object : ByteArrayInputStream(frame(receipt(record).toString().toByteArray())) {
            override fun close() { closed.countDown(); super.close() }
        })
        assertTrue(closed.await(2, java.util.concurrent.TimeUnit.SECONDS))
        await { service.receiptRetirements.size == 0 }
        assertEquals(record, CaptureRepository(context).get(record.captureId))
    }

    @Test fun authorizationFailureIsTranscriptFreeAndCannotPromoteOrEraseRetainedWatchContent() {
        val record = savedAudio()
        CaptureRepository(context).updateTranscript(record.captureId, record.recognitionSessionId!!, 1, "Synthetic retained draft", false)
        val before = CaptureRepository(context).get(record.captureId)!!
        dispatch(listener(), JSONObject().put("protocol", CaptureProtocol.PROTOCOL).put("captureId", record.captureId)
            .put("stage", "AUTHORIZATION_FAILED").put("errorCode", "capture-origin-not-authorized"))
        val after = CaptureRepository(context).get(record.captureId)!!
        assertEquals(before.transferState, after.transferState)
        assertEquals(before.recognitionState, after.recognitionState)
        assertEquals(before.previewText, after.previewText); assertNull(after.finalText)
        assertEquals("capture-origin-not-authorized", after.lastErrorCode)
        assertTrue(java.io.File(after.localAudioPath!!).isFile)
        assertTrue(signals("completed").isEmpty())
    }

    @Test fun localSaveAndLiveFinalRemainSilentPreviewUntilReceipt() {
        val record = savedAudio()
        CaptureSignals.saved(context, record.captureId)
        dispatch(listener(), JSONObject().put("protocol", CaptureProtocol.PROTOCOL)
            .put("captureId", record.captureId).put("sessionId", record.recognitionSessionId)
            .put("revision", 1).put("fullText", "Synthetic live FINAL preview"), CaptureProtocol.FINAL)
        val preview = CaptureRepository(context).get(record.captureId)!!
        assertEquals("Synthetic live FINAL preview", preview.previewText)
        assertNull(preview.finalText)
        assertFalse(preview.completionNotified)
        assertFalse(vibration.isVibrating)
        assertTrue(signals("completed").isEmpty())
        dispatch(listener(), receipt(record))
        assertCompleted(record, "Synthetic final transcript")
    }

    @Test fun liveMessagesRejectWrongSessionStaleRevisionAndUnselectedSourceAcrossListenerRecreation() {
        val record = savedAudio()
        fun live(revision: Long, text: String) = JSONObject().put("protocol", CaptureProtocol.PROTOCOL)
            .put("captureId", record.captureId).put("sessionId", record.recognitionSessionId)
            .put("revision", revision).put("fullText", text)
        dispatch(listener(), live(2, "Synthetic current preview"), CaptureProtocol.PARTIAL)
        val current = CaptureRepository(context).get(record.captureId)!!
        dispatch(listener(), live(1, "Synthetic stale preview"), CaptureProtocol.PARTIAL)
        dispatch(listener(), live(2, "Synthetic duplicate preview"), CaptureProtocol.FINAL)
        dispatch(listener(), live(3, "Synthetic wrong session").put("sessionId", UUID.randomUUID().toString()), CaptureProtocol.FINAL)
        dispatch(listener(), live(3, "Synthetic wrong sender"), CaptureProtocol.PARTIAL, "synthetic-other-phone")
        dispatch(listener(), live(0, "Synthetic invalid revision"), CaptureProtocol.PARTIAL)
        assertEquals(current, CaptureRepository(context).get(record.captureId))
        dispatch(listener(), live(3, "Synthetic newer preview"), CaptureProtocol.FINAL)
        assertEquals("Synthetic newer preview", CaptureRepository(context).get(record.captureId)!!.previewText)
        assertNull(CaptureRepository(context).get(record.captureId)!!.finalText)
        assertTrue(signals("completed").isEmpty())
        assertFalse(vibration.isVibrating)
    }

    @Test fun completionVersionMustNotRegressEvenWhenLocalWavCannotYetClaimFeedback() {
        val record = savedAudio()
        val wav = CaptureRepository(context).audio(record.captureId)
        val bytes = wav.readBytes()
        wav.writeBytes(bytes + byteArrayOf(0, 0))
        dispatch(listener(), receipt(record, "Synthetic newer final").put("transcriptVersion", 4))
        assertFalse(CaptureRepository(context).get(record.captureId)!!.completionNotified)
        dispatch(listener(), receipt(record, "Synthetic older final").put("transcriptVersion", 3))
        assertEquals("Synthetic newer final", CaptureRepository(context).get(record.captureId)!!.finalText)
        assertEquals(4L, CaptureRepository(context).get(record.captureId)!!.transcriptVersion)
        assertTrue(signals("completed").isEmpty())
        wav.writeBytes(bytes)
        dispatch(listener(), receipt(record, "Synthetic newer final").put("transcriptVersion", 4))
        assertCompleted(record, "Synthetic newer final")
    }

    @Test fun archivedNoSpeechReceiptRetainsDraftAndAudioWithoutSuccessAndLeavesNoCompletionPolling() {
        val record = savedAudio()
        CaptureRepository(context).updateTranscript(record.captureId, record.recognitionSessionId!!, 1, "Synthetic retained draft", false)
        dispatch(listener(), receipt(record).put("recognitionState", "NO_SPEECH").removeFinalText())
        val persisted = CaptureRepository(context).get(record.captureId)!!
        assertEquals(RecognitionState.NO_SPEECH, persisted.recognitionState)
        assertEquals("Synthetic retained draft", persisted.previewText)
        assertNull(persisted.finalText)
        assertEquals("no-speech-detected", persisted.lastErrorCode)
        assertFalse(persisted.completionNotified)
        assertTrue(CaptureRepository(context).audio(record.captureId).isFile)
        assertTrue(CaptureRepository(context).pendingTransfers().isEmpty())
        assertTrue(signals("completed").isEmpty())
        assertEquals(1, signals("failed").size)
    }

    @Test fun interruptedRecordingCanBeArchivedButCannotBecomeAutomaticCompletion() {
        val record = savedAudio()
        shared.update(record.captureId) { it.copy(captureState = CaptureState.INTERRUPTED) }
        dispatch(listener(), receipt(record))
        val persisted = CaptureRepository(context).get(record.captureId)!!
        assertEquals(CaptureState.INTERRUPTED, persisted.captureState)
        assertEquals(TransferState.PHONE_ARCHIVED, persisted.transferState)
        assertNull(persisted.finalText)
        assertFalse(persisted.completionNotified)
        assertFalse(vibration.isVibrating)
        assertTrue(signals("completed").isEmpty())
    }

    @Test fun utf8MessageSizeIsBoundedByBytesNotCharacterCount() {
        val record = savedAudio()
        val text = "合成".repeat(6_000)
        val body = receipt(record, text)
        assertTrue(text.length < CaptureProtocol.MAX_MESSAGE_BYTES)
        assertTrue(body.toString().toByteArray(Charsets.UTF_8).size > CaptureProtocol.MAX_MESSAGE_BYTES)
        dispatch(listener(), body)
        assertEquals(record, CaptureRepository(context).get(record.captureId))
        receive(listener(), record, body)
        assertCompleted(record, text)
    }

    private fun JSONObject.removeFinalText(): JSONObject = apply { remove("finalText") }

    @Test fun untrustedSourceProtocolHashPathAndStageCannotPersistOrPulse() {
        val record = savedAudio()
        val service = listener()
        dispatch(service, receipt(record), source = "synthetic-other-phone")
        dispatch(service, receipt(record).put("protocol", "synthetic-invalid-protocol"))
        dispatch(service, receipt(record).put("archiveHash", "b".repeat(64)))
        dispatch(service, receipt(record), path = "${CaptureProtocol.RECEIPT}/unexpected")
        dispatch(service, receipt(record).put("stage", "synthetic-invalid-stage"))
        assertEquals(record, CaptureRepository(context).get(record.captureId))
        assertFalse(vibration.isVibrating)
        assertTrue(signals("completed").isEmpty())
    }

    @Test fun malformedFinalTextCannotBeCoercedIntoASavedTranscriptOrSuccessPulse() {
        val service = listener()
        listOf(JSONObject.NULL, 42, JSONObject().put("synthetic", "not transcript text")).forEach { invalidText ->
            val record = savedAudio()
            dispatch(service, receipt(record).put("finalText", invalidText))
            val persisted = CaptureRepository(context).get(record.captureId)!!
            assertNull(persisted.finalText)
            assertFalse(persisted.completionNotified)
        }
        assertFalse(vibration.isVibrating)
        assertTrue(signals("completed").isEmpty())
    }

    @Test fun oldCaptureErrorPersistsActionableStatusWithoutAttentionPulse() {
        val old = savedAudio()
        savedAudio("2026-10-04T12:31:00Z")
        dispatch(listener(), receipt(old).put("recognitionState", "FAILED").put("errorCode", "speech-recognizer-timeout"))
        val persisted = CaptureRepository(context).get(old.captureId)!!
        assertEquals("speech-recognizer-timeout", persisted.lastErrorCode)
        assertEquals(RecognitionState.FAILED, persisted.recognitionState)
        assertNull(persisted.finalText)
        assertFalse(vibration.isVibrating)
        assertTrue(signals("failed").isEmpty())
    }

    @Test fun currentErrorPulsesThreeTimesOnlyOnceAndLaterFinalCompletes() {
        val record = savedAudio()
        val error = receipt(record).put("recognitionState", "UNAVAILABLE").put("errorCode", "external-audio-provider-unavailable")
        dispatch(listener(), error)
        assertArrayEquals(longArrayOf(0, 100, 100, 100, 100, 100), vibration.pattern)
        assertEquals(-1, vibration.repeat)
        assertEquals(1, signals("failed").size)
        vibrator.cancel()
        dispatch(listener(), error)
        assertFalse(vibration.isVibrating)
        assertEquals(1, signals("failed").size)
        dispatch(listener(), receipt(record))
        assertCompleted(record, "Synthetic final transcript")
        assertNull(CaptureRepository(context).get(record.captureId)!!.lastErrorCode)
    }

    private fun frame(bytes: ByteArray): ByteArray = java.io.ByteArrayOutputStream().apply {
        CaptureReceiptChannel.write(this, bytes)
    }.toByteArray()

    private fun receive(service: CaptureWearListenerService, record: CaptureRecord, body: JSONObject,
                        path: String = CaptureReceiptChannel.path(record.captureId), source: String = "synthetic-selected-phone",
                        stream: TrackedStream = TrackedStream(frame(body.toString().toByteArray(Charsets.UTF_8)))) {
        localLifetime().use { lifetime ->
            lifetime.own(stream)
            service.receiveChannelReceipt(path, source, stream, lifetime)
        }
    }
    private fun localLifetime() = CaptureChannelRetirementOwner(1).reserve()!!.apply {
        acquiredNative { completed -> completed(true) }
    }
    private fun await(condition: () -> Boolean) {
        val deadline = System.nanoTime() + java.util.concurrent.TimeUnit.SECONDS.toNanos(5)
        while (!condition()) { check(System.nanoTime() < deadline) { "Synthetic Wear receipt retirement did not complete" }; Thread.sleep(1) }
    }
    private fun channel(record: CaptureRecord, node: String = "synthetic-selected-phone") = object : ChannelClient.Channel {
        override fun getPath() = CaptureReceiptChannel.path(record.captureId)
        override fun getNodeId() = node
        override fun describeContents() = 0
        override fun writeToParcel(parcel: android.os.Parcel, flags: Int) = Unit
    }

    private open class TrackedStream(bytes: ByteArray) : ByteArrayInputStream(bytes) {
        var closed = false
        val bytesRead: Int get() = pos
        override fun close() { closed = true; super.close() }
    }

    @Test fun boundedChannelPayloadUsesProductionReceiptHandlerWithoutTruncationOrReplayPulse() {
        val record = savedAudio()
        val text = "Synthetic large receipt transcript ".repeat(2_000)
        val body = receipt(record, text)
        val bytes = body.toString().toByteArray(Charsets.UTF_8)
        assertTrue(bytes.size > CaptureProtocol.MAX_MESSAGE_BYTES)
        assertTrue(bytes.size <= CaptureProtocol.MAX_CHANNEL_RESULT_BYTES)
        val stream = TrackedStream(frame(bytes))
        val service = listener()
        dispatch(service, body)
        assertNull(CaptureRepository(context).get(record.captureId)!!.finalText)
        receive(service, record, body, stream = stream)
        assertTrue(stream.closed)
        assertCompleted(record, text)
        vibrator.cancel()
        receive(listener(), record, body)
        assertFalse(vibration.isVibrating)
        assertEquals(1, signals("completed").size)
    }

    @Test fun channelEnvelopeRejectsMismatchedIdentityProtocolSourceStageAndHash() {
        val record = savedAudio()
        val body = receipt(record)
        val path = "${CaptureProtocol.RECEIPT}/${record.captureId}"
        val service = listener()
        receive(service, record, body, path = "$path/extra")
        receive(service, record, body, source = "synthetic-other-phone")
        val invalidBodies = listOf(
            receipt(record).put("protocol", "synthetic-invalid"),
            receipt(record).put("captureId", UUID.randomUUID().toString()),
            receipt(record).put("archiveHash", "b".repeat(64)),
            receipt(record).put("stage", "synthetic-invalid-stage"),
        )
        invalidBodies.forEach { invalidBody ->
            receive(service, record, invalidBody)
        }
        assertEquals(record, CaptureRepository(context).get(record.captureId))
        assertFalse(vibration.isVibrating)
    }

    @Test fun channelAtExactPayloadLimitCompletesAndClosesStream() {
        val record = savedAudio()
        val body = receipt(record)
        val json = body.toString().toByteArray(Charsets.UTF_8)
        val bytes = ByteArray(CaptureProtocol.MAX_CHANNEL_RESULT_BYTES) { 32 }
        json.copyInto(bytes)
        val stream = TrackedStream(frame(bytes))
        receive(listener(), record, body, stream = stream)
        assertTrue(stream.closed)
        assertEquals(CaptureProtocol.MAX_CHANNEL_RESULT_BYTES + 4, stream.bytesRead)
        assertCompleted(record, "Synthetic final transcript")
    }

    @Test fun oversizedMalformedTruncatedAndUnreadableChannelPayloadsAreClosedAndIgnored() {
        val record = savedAudio()
        val body = receipt(record)
        val service = listener()
        val streams = listOf(
            TrackedStream(java.io.ByteArrayOutputStream().apply {
                java.io.DataOutputStream(this).writeInt(CaptureProtocol.MAX_CHANNEL_RESULT_BYTES + 1)
            }.toByteArray()),
            TrackedStream(frame("synthetic non JSON".toByteArray())),
            TrackedStream(frame(body.toString().toByteArray()).dropLast(1).toByteArray()),
            object : TrackedStream(byteArrayOf(1)) {
                override fun read(bytes: ByteArray, offset: Int, length: Int): Int = throw IOException("Synthetic read failure")
            },
        )
        streams.forEach { stream ->
            receive(service, record, body, stream = stream)
        }
        streams.forEach { assertTrue(it.closed) }
        assertEquals(4, streams.first().bytesRead)
        assertEquals(record, CaptureRepository(context).get(record.captureId))
        assertFalse(vibration.isVibrating)
    }
}
