package dev.gitblame.voxgolem.capture

import android.Manifest
import android.content.Context
import android.net.Uri
import com.google.android.gms.wearable.ChannelClient
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import java.io.ByteArrayOutputStream
import java.io.DataOutputStream
import java.io.DataInputStream
import java.io.File
import java.io.IOException
import java.io.InputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.security.MessageDigest
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.ScheduledThreadPoolExecutor
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class CaptureChannelLifetimeTest {
    private val context: Context = RuntimeEnvironment.getApplication()
    private lateinit var coordinator: PhoneCaptureCoordinator
    private lateinit var receiver: CaptureChannelReceiver
    private lateinit var liveWork: ExecutorService
    private lateinit var archiveWork: ExecutorService
    private lateinit var deadlines: ManualDeadlines
    private lateinit var speech: RecordingSpeech
    private val now = AtomicLong(0)
    private val streams = ConcurrentHashMap<ChannelClient.Channel, InputStream>()
    private val acquired = CopyOnWriteArrayList<ChannelClient.Channel>()
    private val closed = CopyOnWriteArrayList<ChannelClient.Channel>()
    private val receipts = CopyOnWriteArrayList<Pair<String, JSONObject>>()
    private var acquire: ((ChannelClient.Channel) -> InputStream)? = null

    @Before
    fun setUp() {
        CaptureChannelReceiver.stopShared()
        context.deleteDatabase("captures.db")
        context.filesDir.resolve("captures").deleteRecursively()
        shadowOf(RuntimeEnvironment.getApplication()).grantPermissions(Manifest.permission.RECORD_AUDIO)
        speech = RecordingSpeech()
        coordinator = PhoneCaptureCoordinator(context, speech, { node, _, bytes -> receipts += node to JSONObject(String(bytes, Charsets.UTF_8)) },
            { Uri.parse("content://synthetic-capture/audio.wav") },
            { OrgExportResult(OrgState.EXPORTED, "synthetic-destination") }, runtimeReady = { true })
        liveWork = Executors.newSingleThreadExecutor()
        archiveWork = Executors.newSingleThreadExecutor()
        deadlines = ManualDeadlines()
        receiver = CaptureChannelReceiver(context, coordinator,
            acquireInput = { channel -> acquired += channel; acquire?.invoke(channel) ?: streams.getValue(channel) },
            closeChannel = { closed += it }, now = now::get,
            liveExecutor = { liveWork }, archiveExecutor = { archiveWork }, deadlineExecutor = { deadlines })
        CaptureChannelReceiver::class.java.getDeclaredField("instance").apply { isAccessible = true }.set(null, receiver)
    }

    @After
    fun tearDown() {
        CaptureChannelReceiver.stopShared()
        receiver.shutdown()
        liveWork.shutdownNow()
        archiveWork.shutdownNow()
        deadlines.shutdownNow()
        coordinator.shutdownForTests()
    }

    @Test
    fun listenerDestructionAfterFirstPartialKeepsReadingSubsequentFramesUntilRealEof() {
        val stream = ChunkedInput()
        val channel = liveChannel()
        streams[channel] = stream
        val listener = Robolectric.buildService(CaptureListenerService::class.java).create()
        stream.feed(liveBytes(frame(0, byteArrayOf(1, 2, 3, 4))))
        listener.get().onChannelOpened(channel)
        assertTrue(speech.firstPcm.await(2, TimeUnit.SECONDS))
        speech.events!!.onHypothesis("Synthetic first word", false)
        assertTrue(coordinator.awaitIdle())
        assertEquals("Synthetic first word", CaptureRepository(context).get(CAPTURE)?.previewText)

        listener.destroy()

        assertFalse(stream.isClosed.get())
        assertTrue(closed.isEmpty())
        stream.feed(frame(1, byteArrayOf(5, 6, 7, 8)) + eof())
        assertTrue(speech.finished.await(2, TimeUnit.SECONDS))
        fence(liveWork)
        assertArrayEquals(byteArrayOf(1, 2, 3, 4, 5, 6, 7, 8), speech.pcm.toByteArray())
        assertEquals(4L, CaptureRepository(context).get(CAPTURE)?.observedFrames)
        assertEquals(0, speech.cancels.get())
        assertEquals(listOf(channel), closed)
        assertEquals(1, stream.closeCount.get())
        assertReleased()
    }

    @Test
    fun listenerDestructionDoesNotTruncateAnArchiveReader() {
        val channel = archiveChannel()
        val stream = ChunkedInput()
        streams[channel] = stream
        val bytes = archiveBytes(byteArrayOf(11, 12, 13, 14))
        stream.feed(bytes.copyOfRange(0, bytes.size - 2))
        val listener = Robolectric.buildService(CaptureListenerService::class.java).create()
        listener.get().onChannelOpened(channel)
        assertTrue(stream.waiting.await(2, TimeUnit.SECONDS))

        listener.destroy()
        assertFalse(stream.isClosed.get())
        stream.feed(bytes.copyOfRange(bytes.size - 2, bytes.size))
        fence(archiveWork)

        val record = CaptureRepository(context).get(CAPTURE)!!
        assertEquals(48L, record.finalizedByteLength)
        assertEquals(2L, record.durableFrames)
        WavPcmReader(java.io.File(record.localAudioPath!!), record.format).use {
            assertArrayEquals(byteArrayOf(11, 12, 13, 14), it.read())
        }
        assertEquals(listOf(channel), closed)
        assertReleased()
    }

    @Test
    fun completedArchiveStillWaitsForLiveEofAcrossListenerDestruction() {
        val live = liveChannel()
        val stream = ChunkedInput()
        streams[live] = stream
        stream.feed(liveBytes(frame(0, byteArrayOf(1, 2))))
        val listener = Robolectric.buildService(CaptureListenerService::class.java).create()
        listener.get().onChannelOpened(live)
        assertTrue(speech.firstPcm.await(2, TimeUnit.SECONDS))
        val archive = archiveChannel()
        streams[archive] = archiveBytes(byteArrayOf(1, 2, 3, 4)).inputStream()
        listener.get().onChannelOpened(archive)
        fence(archiveWork)
        assertEquals(null, CaptureRepository(context).get(CAPTURE)?.sha256)

        listener.destroy()

        assertEquals(listOf(archive), closed)
        assertEquals(0, speech.cancels.get())
        stream.feed(frame(1, byteArrayOf(3, 4)) + eof())
        assertTrue(speech.finished.await(2, TimeUnit.SECONDS))
        fence(liveWork)
        assertTrue(coordinator.awaitIdle())
        val record = CaptureRepository(context).get(CAPTURE)!!
        assertEquals(2L, record.observedFrames)
        assertEquals(2L, record.durableFrames)
        assertEquals(48L, record.finalizedByteLength)
        assertEquals(SESSION, record.recognitionSessionId)
        assertEquals(0, speech.cancels.get())
        assertEquals(1, speech.starts.get())
        assertReleased()
    }

    @Test
    fun realRuntimeDestructionClosesActiveAndNeverStartedQueuedInputsAndDeadlines() {
        val active = liveChannel()
        val queued = liveChannel(OTHER_CAPTURE, OTHER_SESSION)
        val stream = ChunkedInput()
        streams[active] = stream
        streams[queued] = ChunkedInput()
        stream.feed(liveBytes(frame(0, byteArrayOf(1, 2))))
        receiver.onChannelOpened(active)
        assertTrue(speech.firstPcm.await(2, TimeUnit.SECONDS))
        assertTrue(stream.waiting.await(2, TimeUnit.SECONDS))
        receiver.onChannelOpened(queued)
        val runtime = Robolectric.buildService(PhoneCaptureRuntimeService::class.java).create()

        runtime.destroy()

        assertTrue(liveWork.awaitTermination(2, TimeUnit.SECONDS))
        assertEquals(listOf(active), acquired)
        assertEquals(setOf(active, queued), closed.toSet())
        assertEquals(2, closed.size)
        assertTrue(stream.isClosed.get())
        assertEquals(1, stream.closeCount.get())
        assertTrue(deadlines.isShutdown)
        assertEquals(1, speech.cancels.get())
        assertEquals(1L, speech.finished.count)
        assertTrue(coordinator.awaitIdle())
        assertEquals("live-runtime-destroyed", CaptureRepository(context).get(CAPTURE)?.lastErrorCode)
        assertReleased()
        val inputs = PhoneCaptureCoordinator::class.java.getDeclaredField("liveInputs").apply { isAccessible = true }
            .get(coordinator) as Map<*, *>
        assertTrue(inputs.isEmpty())
    }

    @Test
    fun progressResetsTheFifteenSecondDeadlineButARealStallCancelsRecognition() {
        val stream = ChunkedInput()
        val channel = liveChannel()
        streams[channel] = stream
        stream.feed(liveBytes(frame(0, byteArrayOf(1, 2))))
        receiver.onChannelOpened(channel)
        assertTrue(speech.firstPcm.await(2, TimeUnit.SECONDS))
        now.set(14_999)
        deadlines.tick()
        assertFalse(stream.isClosed.get())
        speech.nextPcm = CountDownLatch(1)
        stream.feed(frame(1, byteArrayOf(3, 4)))
        assertTrue(speech.nextPcm.await(2, TimeUnit.SECONDS))
        now.set(15_000)
        deadlines.tick()
        assertFalse(stream.isClosed.get())

        now.set(29_999)
        deadlines.tick()
        fence(liveWork)
        assertTrue(coordinator.awaitIdle())

        assertTrue(stream.isClosed.get())
        assertEquals(1, speech.cancels.get())
        assertEquals("live-input-timeout", CaptureRepository(context).get(CAPTURE)?.lastErrorCode)
        assertEquals(listOf(channel), closed)
        assertReleased()
    }

    @Test
    fun archiveStallHasItsOwnDeadlineAndDoesNotQueueLiveAudio() {
        val archive = archiveChannel(OTHER_CAPTURE)
        val blocked = ChunkedInput()
        streams[archive] = blocked
        receiver.onChannelOpened(archive)
        assertTrue(blocked.waiting.await(2, TimeUnit.SECONDS))
        val live = liveChannel()
        streams[live] = liveBytes(frame(0, byteArrayOf(1, 2)) + eof()).inputStream()
        receiver.onChannelOpened(live)
        assertTrue(speech.finished.await(2, TimeUnit.SECONDS))
        fence(liveWork)
        assertFalse(blocked.isClosed.get())

        now.set(15_000)
        deadlines.tick()
        fence(archiveWork)

        assertTrue(blocked.isClosed.get())
        assertEquals(setOf(archive, live), closed.toSet())
        assertReleased()
    }

    @Test
    fun inputAcquiredAfterOwnerShutdownIsClosedWithoutReadingOrStartingRecognition() {
        val acquiring = CountDownLatch(1)
        val release = CountDownLatch(1)
        val stream = ChunkedInput()
        acquire = {
            acquiring.countDown()
            var released = false
            while (!released) {
                try { released = release.await(2, TimeUnit.SECONDS) } catch (_: InterruptedException) { }
            }
            stream
        }
        val channel = liveChannel()
        try {
            receiver.onChannelOpened(channel)
            assertTrue(acquiring.await(2, TimeUnit.SECONDS))
            receiver.shutdown()
            assertEquals(listOf(channel), closed)
        } finally {
            release.countDown()
        }
        assertTrue(liveWork.awaitTermination(2, TimeUnit.SECONDS))
        assertTrue(stream.isClosed.get())
        assertEquals(1, stream.closeCount.get())
        assertEquals(1L, stream.waiting.count)
        assertEquals(0, speech.starts.get())
        assertReleased()
    }

    @Test
    fun rejectedReaderSubmissionReleasesItsRegistrationAndDeadlineWithoutAcquiringAnInput() {
        liveWork.shutdown()
        val channel = liveChannel()

        receiver.onChannelOpened(channel)

        assertEquals(listOf(channel), closed)
        assertTrue(acquired.isEmpty())
        assertReleased()
        val inputs = PhoneCaptureCoordinator::class.java.getDeclaredField("liveInputs").apply { isAccessible = true }
            .get(coordinator) as Map<*, *>
        assertTrue(inputs.isEmpty())
    }

    @Test
    fun malformedFrameRetiresOnlyItsOwnReaderAndDoesNotReportSuccessfulEof() {
        val channel = liveChannel()
        streams[channel] = liveBytes(frame(0, byteArrayOf(1, 2, 3))).inputStream()
        receiver.onChannelOpened(channel)
        fence(liveWork)
        assertTrue(coordinator.awaitIdle())
        assertEquals(1L, speech.finished.count)
        assertEquals(0, speech.pcm.size())
        assertEquals("live-input-failed", CaptureRepository(context).get(CAPTURE)?.lastErrorCode)
        assertEquals(listOf(channel), closed)
        assertReleased()
    }

    private fun assertReleased() {
        val channels = CaptureChannelReceiver::class.java.getDeclaredField("openChannels").apply { isAccessible = true }
            .get(receiver) as Map<*, *>
        assertTrue(channels.isEmpty())
        assertTrue(deadlines.queue.isEmpty())
    }

    private fun fence(executor: ExecutorService) = executor.submit {}.get(2, TimeUnit.SECONDS)

    @Test fun activeSessionWithoutAChannelCannotReceiveAnotherBoundCapturesPcmOrTerminalOperations() {
        val a = "synthetic-watch-node"
        val b = "synthetic-other-watch"
        assertTrue(coordinator.beginLive(CAPTURE, SESSION, a, CaptureFormat(), "2026-01-02T03:04:05Z"))
        val repository = CaptureRepository(context)
        repository.admitPhoneCapture(CaptureRecord(OTHER_CAPTURE, createdAtUtc = "2026-01-02T03:04:05Z",
            captureState = CaptureState.RECORDING, recognitionSessionId = SESSION), b)
        val foreign = TestChannel("${CaptureWire.LIVE}/$OTHER_CAPTURE/$SESSION", b)
        streams[foreign] = liveBytes(frame(0, byteArrayOf(9, 0)) + eof(), OTHER_CAPTURE).inputStream()
        receiver.onChannelOpened(foreign); fence(liveWork); assertTrue(coordinator.awaitIdle())
        assertFalse(acquired.contains(foreign)); assertTrue(closed.contains(foreign))
        assertEquals(0, speech.pcm.size()); assertEquals(0, speech.cancels.get()); assertEquals(1L, speech.finished.count)
        assertEquals(1, speech.starts.get())
        assertTrue(receipts.any { it.first == b && it.second.optString("recognitionStatus") == "live-input-ownership-rejected" })
        val forgedOwner: () -> Unit = {}
        assertThrows(CaptureLiveInputOwnershipException::class.java) { coordinator.appendLive(OTHER_CAPTURE, SESSION, b, forgedOwner, byteArrayOf(9, 0)) }
        assertThrows(CaptureLiveInputOwnershipException::class.java) { coordinator.finishLive(OTHER_CAPTURE, SESSION, b, forgedOwner, 1) }
        coordinator.failLiveInput(OTHER_CAPTURE, SESSION, b, forgedOwner, "live-input-failed")
        assertFalse(coordinator.releaseLiveInput(OTHER_CAPTURE, SESSION, b, forgedOwner))
        assertEquals(RecognitionState.RUNNING, repository.get(CAPTURE)!!.recognitionState)
        assertEquals(0, speech.cancels.get()); assertEquals(1L, speech.finished.count)
        // The actual A channel can still attach to A's START-admitted SDK owner after B was rejected.
        val actual = liveChannel()
        streams[actual] = liveBytes(frame(0, byteArrayOf(1, 0, 2, 0)) + eof()).inputStream()
        receiver.onChannelOpened(actual); fence(liveWork)
        assertArrayEquals(byteArrayOf(1, 0, 2, 0), speech.pcm.toByteArray())
        assertEquals(0L, speech.finished.count)
    }

    @Test fun liveAdmissionChecksCaptureNodeAndExactRegisteredOwnerBeforeAnySdkMutation() {
        val a = "synthetic-watch-node"
        val first: () -> Unit = {}
        val replacement: () -> Unit = {}
        assertTrue(coordinator.registerLiveInput(CAPTURE, SESSION, a, first))
        assertFalse(coordinator.beginLive(OTHER_CAPTURE, SESSION, a, CaptureFormat(), "2026-01-02T03:04:05Z"))
        assertFalse(coordinator.beginLive(CAPTURE, SESSION, "synthetic-other-watch", CaptureFormat(), "2026-01-02T03:04:05Z"))
        assertFalse(coordinator.releaseLiveInput(OTHER_CAPTURE, SESSION, a, first))
        assertFalse(coordinator.releaseLiveInput(CAPTURE, SESSION, "synthetic-other-watch", first))
        assertEquals(0, speech.starts.get())
        assertTrue(coordinator.releaseLiveInput(CAPTURE, SESSION, a, first))
        assertTrue(coordinator.registerLiveInput(CAPTURE, SESSION, a, replacement))
        assertFalse(coordinator.beginLive(CAPTURE, SESSION, a, CaptureFormat(), "2026-01-02T03:04:05Z", first))
        assertTrue(coordinator.beginLive(CAPTURE, SESSION, a, CaptureFormat(), "2026-01-02T03:04:05Z", replacement))
        assertThrows(CaptureLiveInputOwnershipException::class.java) { coordinator.appendLive(CAPTURE, SESSION, a, first, byteArrayOf(9, 0)) }
        assertThrows(CaptureLiveInputOwnershipException::class.java) { coordinator.finishLive(CAPTURE, SESSION, a, first, 1) }
        coordinator.failLiveInput(CAPTURE, SESSION, a, first, "live-input-failed")
        assertFalse(coordinator.releaseLiveInput(CAPTURE, SESSION, a, first))
        assertFalse(coordinator.registerLiveInput(OTHER_CAPTURE, SESSION, a) {})
        assertFalse(coordinator.registerLiveInput(CAPTURE, SESSION, "synthetic-other-watch") {})
        assertEquals(0, speech.pcm.size()); assertEquals(0, speech.cancels.get()); assertEquals(1L, speech.finished.count)
        coordinator.appendLive(CAPTURE, SESSION, a, replacement, byteArrayOf(1, 0))
        assertArrayEquals(byteArrayOf(1, 0), speech.pcm.toByteArray())
    }

    @Test fun authorizedChannelWithUnavailableSdkDropsPreviewAndRetainsRecordingMetadataWithoutAnotherSdkOwner() {
        speech.externalAudio = false
        val channel = liveChannel()
        streams[channel] = liveBytes(frame(0, byteArrayOf(1, 0)) + eof()).inputStream()
        receiver.onChannelOpened(channel); fence(liveWork); assertTrue(coordinator.awaitIdle())
        assertTrue(closed.contains(channel)); assertEquals(0, speech.starts.get()); assertEquals(0, speech.pcm.size())
        val record = CaptureRepository(context).get(CAPTURE)!!
        assertEquals(CaptureState.RECORDING, record.captureState)
        assertEquals(RecognitionState.UNAVAILABLE, record.recognitionState)
        assertEquals("synthetic-watch-node", CaptureRepository(context).phoneOrigin(CAPTURE))
    }

    @Test fun matchingArchiveHeaderWithoutBodyCannotAuthorizeAnotherNodeOrClaimALegacyCapture() {
        val original = archiveChannel()
        val full = archiveBytes(byteArrayOf(1, 0, 2, 0))
        streams[original] = full.inputStream()
        receiver.onChannelOpened(original)
        fence(archiveWork); assertTrue(coordinator.awaitIdle())
        speech.events!!.onHypothesis("Synthetic origin private final", true)
        assertTrue(coordinator.awaitIdle())
        val repository = CaptureRepository(context)
        val saved = repository.get(CAPTURE)!!
        val audio = File(saved.localAudioPath!!).readBytes()
        val headerLength = DataInputStream(full.inputStream()).readInt()
        val headerOnly = full.copyOf(4 + headerLength + 8)
        val other = TestChannel(original.path, "synthetic-other-watch")
        val input = headerOnly.inputStream()
        streams[other] = input
        receipts.clear()
        receiver.onChannelOpened(other); fence(archiveWork); assertTrue(coordinator.awaitIdle())
        assertEquals(headerOnly.size, input.available()); assertFalse(acquired.contains(other))
        assertTrue(receipts.all { it.first == "synthetic-other-watch" && it.second.optString("errorCode") == "capture-origin-not-authorized" })
        assertTrue(receipts.none { it.second.has("finalText") || it.second.has("fullText") || it.second.has("archiveHash") })
        assertEquals(saved, repository.get(CAPTURE))
        assertArrayEquals(audio, File(saved.localAudioPath).readBytes())
        context.openOrCreateDatabase("captures.db", 0, null).use { it.execSQL("DELETE FROM phone_capture_origins") }
        val legacy = TestChannel(original.path)
        receipts.clear()
        streams[legacy] = headerOnly.inputStream()
        receiver.onChannelOpened(legacy); fence(archiveWork); assertTrue(coordinator.awaitIdle())
        assertTrue(receipts.all { it.second.optString("stage") == "AUTHORIZATION_FAILED" })
        assertTrue(receipts.none { it.second.has("finalText") || it.second.has("archiveHash") }); assertNull(repository.phoneOrigin(CAPTURE))
        assertEquals(saved, repository.get(CAPTURE)); assertArrayEquals(audio, File(saved.localAudioPath).readBytes())
    }

    private fun liveChannel(id: String = CAPTURE, session: String = SESSION) = TestChannel("${CaptureWire.LIVE}/$id/$session")
    private fun archiveChannel(id: String = CAPTURE) = TestChannel("${CaptureWire.ARCHIVE}/$id/$ATTEMPT")

    private fun liveBytes(frames: ByteArray, captureId: String = CAPTURE): ByteArray = header(JSONObject()
        .put("protocol", CaptureWire.PROTOCOL).put("captureId", captureId).put("sessionId", SESSION)
        .put("sampleRate", 16_000).put("channels", 1).put("bitsPerSample", 16)
        .put("startedAt", "2026-01-02T03:04:05Z")) + frames

    private fun frame(sequence: Long, pcm: ByteArray) = ByteArrayOutputStream().also { bytes ->
        DataOutputStream(bytes).use { it.writeLong(sequence); it.writeInt(pcm.size); it.write(pcm) }
    }.toByteArray()

    private fun eof() = ByteArrayOutputStream().also { bytes -> DataOutputStream(bytes).use { it.writeLong(-1) } }.toByteArray()

    private fun header(json: JSONObject) = ByteArrayOutputStream().also { bytes ->
        val encoded = json.toString().toByteArray(Charsets.UTF_8)
        DataOutputStream(bytes).use { it.writeInt(encoded.size); it.write(encoded) }
    }.toByteArray()

    private fun archiveBytes(pcm: ByteArray): ByteArray {
        val wav = ByteBuffer.allocate(44 + pcm.size).order(ByteOrder.LITTLE_ENDIAN).apply {
            put("RIFF".toByteArray()); putInt(36 + pcm.size); put("WAVEfmt ".toByteArray()); putInt(16)
            putShort(1); putShort(1); putInt(16_000); putInt(32_000); putShort(2); putShort(16)
            put("data".toByteArray()); putInt(pcm.size); put(pcm)
        }.array()
        val hash = MessageDigest.getInstance("SHA-256").digest(wav).joinToString("") { "%02x".format(it) }
        return header(JSONObject().put("protocol", CaptureWire.PROTOCOL).put("captureId", CAPTURE).put("attemptId", ATTEMPT)
            .put("sampleRate", 16_000).put("channels", 1).put("bitsPerSample", 16).put("frameCount", pcm.size / 2)
            .put("wavBytes", wav.size).put("sha256", hash).put("finishDate", "2026-01-02").put("timezone", "UTC")
            .put("captureState", "SAVED")) + ByteArrayOutputStream().also { bytes ->
            DataOutputStream(bytes).use { it.writeLong(wav.size.toLong()); it.write(wav) }
        }.toByteArray()
    }

    private class TestChannel(private val channelPath: String, private val source: String = "synthetic-watch-node") : ChannelClient.Channel {
        override fun getNodeId() = source
        override fun getPath() = channelPath
        override fun describeContents() = 0
        override fun writeToParcel(destination: android.os.Parcel, flags: Int) = Unit
    }

    private class ManualDeadlines : ScheduledThreadPoolExecutor(1) {
        private val checks = CopyOnWriteArrayList<Pair<Runnable, ScheduledFuture<*>>>()
        init { removeOnCancelPolicy = true }
        override fun scheduleWithFixedDelay(command: Runnable, initialDelay: Long, delay: Long, unit: TimeUnit): ScheduledFuture<*> {
            val future = super.schedule(command, 1, TimeUnit.DAYS)
            checks += command to future
            return future
        }
        fun tick() = checks.filterNot { it.second.isCancelled }.forEach { it.first.run() }
    }

    private class ChunkedInput : InputStream() {
        private val bytes = LinkedBlockingQueue<Int>()
        val isClosed = AtomicBoolean(false)
        val closeCount = AtomicInteger()
        val waiting = CountDownLatch(1)
        fun feed(chunk: ByteArray) { chunk.forEach { bytes.add(it.toInt() and 0xff) } }
        override fun read(): Int {
            if (isClosed.get()) throw IOException("synthetic-closed-input")
            if (bytes.isEmpty()) waiting.countDown()
            val next = bytes.take()
            if (isClosed.get()) throw IOException("synthetic-closed-input")
            return next
        }
        override fun close() {
            closeCount.incrementAndGet()
            isClosed.set(true)
            bytes.offer(-1)
        }
    }

    private class RecordingSpeech : SpeechRecognizerAdapter {
        var externalAudio = true
        val pcm = ByteArrayOutputStream()
        val firstPcm = CountDownLatch(1)
        val finished = CountDownLatch(1)
        val cancels = AtomicInteger()
        val starts = AtomicInteger()
        @Volatile var nextPcm = CountDownLatch(1)
        @Volatile var events: SpeechRecognizerAdapter.Events? = null
        override fun capabilities() = SpeechRecognizerAdapter.Capabilities(
            setOf(CaptureFormat()), setOf("en-US"), true, true, true, externalAudio, true)
        override fun start(sessionId: String, format: CaptureFormat, language: String?, events: SpeechRecognizerAdapter.Events) {
            starts.incrementAndGet()
            this.events = events
        }
        override fun append(sessionId: String, pcm: ByteArray) { this.pcm.write(pcm); firstPcm.countDown(); nextPcm.countDown() }
        override fun finish(sessionId: String) { finished.countDown() }
        override fun cancel(sessionId: String) { cancels.incrementAndGet() }
    }

    companion object {
        private const val CAPTURE = "123e4567-e89b-42d3-a456-426614174101"
        private const val SESSION = "123e4567-e89b-42d3-a456-426614174102"
        private const val ATTEMPT = "123e4567-e89b-42d3-a456-426614174103"
        private const val OTHER_CAPTURE = "123e4567-e89b-42d3-a456-426614174104"
        private const val OTHER_SESSION = "123e4567-e89b-42d3-a456-426614174105"
    }
}
