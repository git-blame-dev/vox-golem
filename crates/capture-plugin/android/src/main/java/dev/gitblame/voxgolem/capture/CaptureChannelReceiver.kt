package dev.gitblame.voxgolem.capture

import android.content.Context
import com.google.android.gms.tasks.Tasks
import com.google.android.gms.wearable.ChannelClient
import com.google.android.gms.wearable.DataEvent
import com.google.android.gms.wearable.DataEventBuffer
import com.google.android.gms.wearable.MessageEvent
import com.google.android.gms.wearable.Wearable
import org.json.JSONObject
import java.io.DataInputStream
import java.io.File
import java.io.FileInputStream
import java.io.FileOutputStream
import java.io.IOException
import java.io.InputStream
import java.security.MessageDigest
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.ExecutorService
import java.util.concurrent.RejectedExecutionException
import java.util.concurrent.ScheduledExecutorService
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicReference

/** Process-owned Data Layer IO; the transient GMS listener only forwards callbacks. */
internal class CaptureChannelReceiver(
    context: Context,
    private val coordinator: PhoneCaptureCoordinator = PhoneCaptureCoordinator.shared(context.applicationContext),
    private val acquireInput: (ChannelClient.Channel) -> InputStream = channelInput(context.applicationContext),
    private val closeChannel: (ChannelClient.Channel) -> Unit = channelCloser(context.applicationContext),
    private val now: () -> Long = { android.os.SystemClock.elapsedRealtime() },
    liveExecutor: (() -> ExecutorService) = { readerExecutor() },
    archiveExecutor: (() -> ExecutorService) = { readerExecutor() },
    deadlineExecutor: (() -> ScheduledExecutorService) = {
        java.util.concurrent.ScheduledThreadPoolExecutor(1).apply {
            removeOnCancelPolicy = true
            setKeepAliveTime(30, TimeUnit.SECONDS)
            allowCoreThreadTimeOut(true)
        }
    },
) {
    private val appContext = context.applicationContext
    private val repository by lazy { CaptureRepository(appContext) }
    private val liveWorkDelegate = lazy(liveExecutor)
    private val archiveWorkDelegate = lazy(archiveExecutor)
    private val deadlinesDelegate = lazy(deadlineExecutor)
    private val liveWork: ExecutorService by liveWorkDelegate
    private val archiveWork: ExecutorService by archiveWorkDelegate
    private val deadlines by deadlinesDelegate
    private val openChannels = ConcurrentHashMap<ChannelClient.Channel, ChannelReader>()
    @Volatile private var destroyed = false

    fun onDataChanged(events: DataEventBuffer) {
        if (isWatch()) return
        events.forEach { event ->
            if (event.type != DataEvent.TYPE_CHANGED) return@forEach
            val uri = event.dataItem.uri
            val pathId = uri.path.orEmpty().removePrefix("${CaptureWire.PENDING}/")
            if (!uri.path.orEmpty().startsWith("${CaptureWire.PENDING}/") || !CaptureProtocol.isValidUuid(pathId)) return@forEach
            val map = runCatching { com.google.android.gms.wearable.DataMapItem.fromDataItem(event.dataItem).dataMap }.getOrNull() ?: return@forEach
            val id = map.getString("captureId") ?: return@forEach
            if (!CaptureProtocol.isValidUuid(id) || !id.equals(pathId, ignoreCase = true) || map.getString("protocol") != CaptureWire.PROTOCOL) return@forEach
            // The item is discovery metadata only; audio and durable receipts use the channel/message paths.
        }
    }

    fun onMessageReceived(event: MessageEvent) {
        if (isWatch()) return
        if (event.data.size > CaptureProtocol.MAX_MESSAGE_BYTES) return
        val json = runCatching { JSONObject(String(event.data, Charsets.UTF_8)) }.getOrNull() ?: return
        if (!CaptureWire.isProtocol(json)) return
        val captureId = json.optString("captureId")
        if (!CaptureProtocol.isValidUuid(captureId)) return
        when (event.path) {
            CaptureWire.START -> {
                val header = runCatching { CaptureWire.liveHeader(json) }.getOrNull() ?: return
                if (event.sourceNodeId.isBlank() || !CaptureProtocol.isValidUuid(header.sessionId) ||
                    !CaptureWire.validLiveStart(header, captureId, header.sessionId)) return
                coordinator.beginLive(captureId, header.sessionId, event.sourceNodeId, header.format, header.startedAt,
                    requestedLanguage = header.requestedLanguage, speechNetworkFallback = header.speechNetworkFallback,
                    purpose = header.purpose)
            }
            CaptureWire.STOP -> {
                val sessionId = json.optString("sessionId")
                val frames = json.optLong("frameCount", -1)
                if (event.sourceNodeId.isNotBlank() && CaptureProtocol.isValidUuid(sessionId) && frames >= 0) {
                    coordinator.noteLiveStop(captureId, sessionId, event.sourceNodeId, frames)
                }
            }
            CaptureWire.STATUS -> handleStatus(event.sourceNodeId, json)
        }
    }

    fun onChannelOpened(channel: ChannelClient.Channel) {
        if (isWatch()) {
            runCatching { closeChannel(channel) }
            return
        }
        val liveMatch = Regex("^${Regex.escape(CaptureWire.LIVE)}/([0-9a-fA-F-]{36})/([0-9a-fA-F-]{36})$").matchEntire(channel.path)
        val archiveMatch = Regex("^${Regex.escape(CaptureWire.ARCHIVE)}/([0-9a-fA-F-]{36})/([0-9a-fA-F-]{36})$").matchEntire(channel.path)
        if (liveMatch == null && archiveMatch == null) {
            runCatching { closeChannel(channel) }
            return
        }
        val match = liveMatch ?: archiveMatch!!
        if (!coordinator.canReceiveArchive(match.groupValues[1], channel.nodeId)) {
            coordinator.rejectOrigin(match.groupValues[1], channel.nodeId)
            runCatching { closeChannel(channel) }
            return
        }
        val owned = ChannelReader(channel, match.groupValues[1], match.groupValues[2], liveMatch != null)
        val admitted = synchronized(openChannels) {
            if (openChannels.containsKey(channel)) return
            if (destroyed || !owned.register()) false
            else { openChannels[channel] = owned; true }
        }
        if (!admitted) {
            if (liveMatch != null) coordinator.rejectLiveInput(match.groupValues[1], match.groupValues[2], channel.nodeId)
            runCatching { closeChannel(channel) }
            return
        }
        val started = synchronized(openChannels) {
            if (destroyed) false else { owned.start(); true }
        }
        if (!started) owned.stopRuntime()
    }

    private inner class ChannelReader(
        private val channel: ChannelClient.Channel,
        private val captureId: String,
        private val sessionId: String,
        private val live: Boolean,
    ) {
        private val timing by lazy { CaptureTiming(appContext, if (live) "phone-live-channel" else "phone-archive-channel") }
        private val lastProgress = AtomicLong(now())
        private val retired = AtomicBoolean(false)
        private val openedInput = AtomicReference<InputStream?>()
        private val deadline = AtomicReference<ScheduledFuture<*>?>()
        private val closeInput: () -> Unit = { retire(null) }

        fun register(): Boolean = !live || coordinator.registerLiveInput(captureId, sessionId, channel.nodeId, closeInput)

        fun start() {
            timing.mark("channel-opened")
            if (retired.get()) return
            try {
                deadline.set(deadlines.scheduleWithFixedDelay({
                    if (now() - lastProgress.get() >= INPUT_IDLE_TIMEOUT_MILLIS) retire(if (live) "live-input-timeout" else "archive-input-timeout")
                }, 1, 1, TimeUnit.SECONDS))
                if (retired.get()) { deadline.getAndSet(null)?.cancel(false); return }
                (if (live) liveWork else archiveWork).execute { read() }
            } catch (_: RejectedExecutionException) {
                retire(if (live) "live-reader-unavailable" else "archive-reader-unavailable")
            }
        }

        private fun read() {
            if (destroyed) { stopRuntime(); return }
            if (retired.get()) return
            timing.mark("reader-started")
            val input = if (retired.get()) null else runCatching {
                acquireInput(channel)
            }.getOrNull()
            if (input == null) {
                retire(if (live) "live-input-unavailable" else "archive-input-unavailable")
                timing.finish("input-unavailable")
                return
            }
            openedInput.set(input)
            if (retired.get()) {
                runCatching { openedInput.getAndSet(null)?.close() }
                return
            }
            timing.mark("input-ready")
            try {
                if (live) receiveLive(channel, DataInputStream(input), captureId, sessionId, closeInput, timing) {
                    lastProgress.set(now())
                }
                else receiveArchive(channel, DataInputStream(input), captureId, sessionId) { lastProgress.set(now()) }
            } catch (_: CaptureOriginAuthorizationException) {
                coordinator.rejectOrigin(captureId, channel.nodeId)
                retire(null)
            } catch (_: CaptureLiveInputOwnershipException) {
                coordinator.rejectLiveInput(captureId, sessionId, channel.nodeId)
                retire(null)
            } catch (_: Exception) {
                retire(if (live) "live-input-failed" else "archive-input-failed")
            } finally {
                retire(null)
                timing.finish("reader-closed")
            }
        }

        fun retire(code: String?) {
            if (!retired.compareAndSet(false, true)) return
            deadline.getAndSet(null)?.cancel(false)
            runCatching { openedInput.getAndSet(null)?.close() }
            runCatching { closeChannel(channel) }
            try {
                if (live) {
                    if (code != null) coordinator.failLiveInput(captureId, sessionId, channel.nodeId, closeInput, code)
                } else if (code != null && CaptureProtocol.isValidUuid(captureId)) {
                    coordinator.markArchiveFailure(captureId, code)
                }
            } finally {
                if (live) coordinator.releaseLiveInput(captureId, sessionId, channel.nodeId, closeInput)
                openChannels.remove(channel, this)
                timing.finish("reader-closed")
            }
        }

        fun stopRuntime() = retire(if (live) "live-runtime-destroyed" else "archive-runtime-destroyed")
    }

    private fun receiveLive(channel: ChannelClient.Channel, input: DataInputStream, captureId: String, sessionId: String,
                             inputOwner: () -> Unit, timing: CaptureTiming, progress: () -> Unit) {
        if (channel.nodeId.isBlank() || !CaptureProtocol.isValidUuid(captureId) || !CaptureProtocol.isValidUuid(sessionId)) throw IOException("invalid-live-identity")
        val header = CaptureWire.liveHeader(CaptureWire.readJsonHeader(input, CaptureWire.MAX_LIVE_HEADER_BYTES))
        if (!CaptureWire.validLiveStart(header, captureId, sessionId)) throw IOException("invalid-live-header")
        timing.mark("header-read")
        progress()
        val format = header.format
        if (!coordinator.beginLive(captureId, sessionId, channel.nodeId, format, header.startedAt, inputOwner,
                header.requestedLanguage, header.speechNetworkFallback, header.purpose)) {
            // No admitted SDK preview is not permission to feed another session; local Watch audio is independent.
            throw CaptureLiveInputOwnershipException()
        }
        if (!coordinator.canReuseStagedArchive(captureId, channel.nodeId)) throw CaptureOriginAuthorizationException()
        var previous: Long? = null
        var totalPcmBytes = 0L
        val maxPcmBytes = format.sampleRate.toLong() * format.channels * format.bitsPerSample / 8 * MAX_CAPTURE_MILLIS / 1_000
        while (true) {
            val sequence = input.readLong()
            if (sequence == -1L) break
            val count = input.readInt()
            if (!CaptureProtocol.validateFrame(format, sequence, count, previous)) throw IOException("invalid-live-frame")
            if (totalPcmBytes > maxPcmBytes - count) throw IOException("live-audio-exceeds-capture-limit")
            val pcm = ByteArray(count)
            input.readFully(pcm)
            progress()
            timing.mark("first-pcm-received")
            coordinator.appendLive(captureId, sessionId, channel.nodeId, inputOwner, pcm)
            timing.mark("first-pcm-appended")
            previous = sequence
            totalPcmBytes += count
        }
        // The stop message is a separate channel and may arrive before EOF; count received aligned samples.
        val frames = totalPcmBytes / (format.channels * format.bitsPerSample / 8)
        timing.mark("input-eof")
        coordinator.finishLive(captureId, sessionId, channel.nodeId, inputOwner, frames)
    }

    private fun receiveArchive(channel: ChannelClient.Channel, input: DataInputStream, captureId: String, attemptId: String,
                               progress: () -> Unit) {
        if (channel.nodeId.isBlank() || !CaptureProtocol.isValidUuid(captureId) || !CaptureProtocol.isValidUuid(attemptId)) throw IOException("invalid-archive-identity")
        val header = CaptureWire.archiveHeader(CaptureWire.readJsonHeader(input, CaptureWire.MAX_HEADER_BYTES))
        if (!CaptureWire.validArchiveHeader(header, captureId, attemptId)) throw IOException("invalid-archive-header")
        if (!coordinator.canReceiveArchive(captureId, channel.nodeId)) throw CaptureOriginAuthorizationException()
        progress()
        val format = header.format
        val expectedLength = header.wavBytes
        val streamLength = input.readLong()
        val maximumPcmBytes = format.sampleRate.toLong() * format.channels * format.bitsPerSample / 8 * MAX_CAPTURE_MILLIS / 1_000
        if (streamLength != expectedLength || streamLength !in 44..(44 + maximumPcmBytes)) throw IOException("invalid-archive-length")

        val old = repository.get(captureId)
        if (old != null && (old.purpose != header.purpose || old.requestedLanguage != header.requestedLanguage ||
                old.speechNetworkFallback != header.speechNetworkFallback)) throw IOException("capture-policy-conflict")
        if (old?.sha256 != null && (!old.sha256.equals(header.sha256, true) || old.finalizedByteLength != expectedLength)) {
            throw IOException("capture-id-content-conflict")
        }
        if (old != null && old.sha256 == null && old.localAudioPath != null) {
            throw IOException("capture-id-has-nonprovisional-record")
        }

        val directory = File(File(appContext.filesDir, "captures"), captureId.lowercase())
        if (!directory.exists() && !directory.mkdirs()) throw IOException("archive-directory-create-failed")
        val finalWav = File(directory, "received.wav")
        val part = File(directory, "received.wav.part")
        if (finalWav.exists()) {
            if (!coordinator.canReuseStagedArchive(captureId, channel.nodeId)) throw CaptureOriginAuthorizationException()
            if (finalWav.length() != expectedLength || !sha256(finalWav).equals(header.sha256, ignoreCase = true)) {
                throw IOException("existing-staged-audio-conflict")
            }
            verifyWav(finalWav, format, header.frameCount)
            coordinator.onArchiveReceived(archiveRecord(header, old, finalWav), channel.nodeId)
            return
        }
        val digest = MessageDigest.getInstance("SHA-256")
        FileOutputStream(part).use { output ->
            val buffer = ByteArray(64 * 1024)
            var remaining = streamLength
            while (remaining > 0) {
                val count = input.read(buffer, 0, minOf(buffer.size.toLong(), remaining).toInt())
                if (count < 0) throw IOException("truncated-archive")
                digest.update(buffer, 0, count)
                output.write(buffer, 0, count)
                remaining -= count
                if (count > 0) progress()
            }
            output.fd.sync()
        }
        val actualHash = digest.digest().joinToString("") { "%02x".format(it) }
        if (part.length() != expectedLength || !actualHash.equals(header.sha256, ignoreCase = true)) {
            part.delete()
            throw IOException("archive-sha256-or-length-mismatch")
        }
        verifyWav(part, format, header.frameCount)
        if (!coordinator.canReceiveArchive(captureId, channel.nodeId)) throw CaptureOriginAuthorizationException()
        if (!part.renameTo(finalWav)) throw IOException("cannot-finalize-staged-archive")

        coordinator.onArchiveReceived(archiveRecord(header, old, finalWav), channel.nodeId)
    }

    private fun archiveRecord(header: CaptureWire.ArchiveHeader, old: CaptureRecord?, wav: File): CaptureRecord {
        val finishedAt = header.finishedAtUtc ?: old?.finishedAtUtc ?: Instant.now().toString()
        return (old ?: CaptureRecord(captureId = header.captureId, createdAtUtc = header.createdAtUtc ?: finishedAt)).copy(
            sourceKind = "watch",
            startedAtUtc = header.startedAtUtc ?: old?.startedAtUtc,
            finishedAtUtc = finishedAt,
            finishedLocalDate = LocalDate.parse(header.finishDate).toString(),
            timezoneId = ZoneId.of(header.timezone).id,
            captureState = header.captureState,
            stopReason = header.stopReason ?: old?.stopReason,
            observedFrames = header.frameCount,
            durableFrames = header.frameCount,
            format = header.format,
            localAudioPath = wav.absolutePath,
            finalizedByteLength = header.wavBytes,
            sha256 = header.sha256.lowercase(),
            transferState = TransferState.PHONE_STAGED,
            archiveState = ArchiveState.STAGED,
            orgState = if (old?.finalText.isNullOrBlank()) OrgState.WAITING_FOR_TRANSCRIPT else OrgState.READY,
            purpose = header.purpose, requestedLanguage = header.requestedLanguage,
            speechNetworkFallback = header.speechNetworkFallback,
        )
    }

    private fun verifyWav(file: File, format: CaptureFormat, frames: Long) {
        if (file.length() != 44L + frames * format.channels * format.bitsPerSample / 8) throw IOException("wav-frame-length-mismatch")
        WavPcmReader(file, format).use { if (it.frameCount != frames) throw IOException("wav-header-does-not-match-manifest") }
    }

    private fun sha256(file: File): String = FileInputStream(file).use { input ->
        val digest = MessageDigest.getInstance("SHA-256")
        val buffer = ByteArray(64 * 1024)
        while (true) {
            val count = input.read(buffer)
            if (count < 0) break
            digest.update(buffer, 0, count)
        }
        digest.digest().joinToString("") { "%02x".format(it) }
    }

    private fun handleStatus(nodeId: String, json: JSONObject) {
        val requestId = json.optString("requestId")
        if (!CaptureProtocol.isValidUuid(requestId)) return
        coordinator.respondStatus(json.optString("captureId"), nodeId)
    }

    fun shutdown() {
        val close = synchronized(openChannels) {
            destroyed = true
            openChannels.values.toList().also {
                openChannels.clear()
            }
        }
        close.forEach { runCatching { it.stopRuntime() } }
        if (deadlinesDelegate.isInitialized()) deadlines.shutdownNow()
        if (liveWorkDelegate.isInitialized()) liveWork.shutdownNow()
        if (archiveWorkDelegate.isInitialized()) archiveWork.shutdownNow()
    }

    private fun isWatch(): Boolean = appContext.packageManager.hasSystemFeature("android.hardware.type.watch")

    companion object {
        private const val MAX_CAPTURE_MILLIS = 10 * 60 * 1_000L
        private const val INPUT_IDLE_TIMEOUT_MILLIS = 15_000L
        private const val MAX_QUEUED_READERS = 16
        @Volatile private var instance: CaptureChannelReceiver? = null

        fun shared(context: Context): CaptureChannelReceiver = instance ?: synchronized(this) {
            instance ?: CaptureChannelReceiver(context.applicationContext).also { instance = it }
        }

        fun stopShared() {
            val owned = synchronized(this) { instance.also { instance = null } }
            owned?.shutdown()
        }

        private fun readerExecutor(): ExecutorService = ThreadPoolExecutor(
            1, 1, 30, TimeUnit.SECONDS, ArrayBlockingQueue(MAX_QUEUED_READERS),
        ).apply { allowCoreThreadTimeOut(true) }

        private fun channelInput(context: Context): (ChannelClient.Channel) -> InputStream = { channel ->
            Tasks.await(Wearable.getChannelClient(context).getInputStream(channel), 5, TimeUnit.SECONDS)
        }

        private fun channelCloser(context: Context): (ChannelClient.Channel) -> Unit = { channel ->
            Wearable.getChannelClient(context).close(channel)
            Unit
        }
    }
}

internal class CaptureOriginAuthorizationException : SecurityException("capture-origin-not-authorized")
internal class CaptureLiveInputOwnershipException : SecurityException("live-input-ownership-rejected")
