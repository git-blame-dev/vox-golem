package dev.gitblame.voxgolem.capture

import org.json.JSONObject
import java.io.DataInputStream
import java.io.IOException
import java.nio.charset.StandardCharsets
import java.time.LocalDate
import java.time.Instant
import java.time.ZoneId

/** The phone-side decoder for the concrete WearCaptureTransport v1 wire format. */
internal object CaptureWire {
    const val PROTOCOL = CaptureProtocol.PROTOCOL
    const val BASE = CaptureProtocol.BASE
    const val LIVE = CaptureProtocol.LIVE
    const val ARCHIVE = CaptureProtocol.ARCHIVE
    const val PENDING = CaptureProtocol.PENDING
    const val START = CaptureProtocol.START
    const val READY = CaptureProtocol.READY
    const val PARTIAL = CaptureProtocol.PARTIAL
    const val FINAL = CaptureProtocol.FINAL
    const val STOP = CaptureProtocol.STOP
    const val RECEIPT = CaptureProtocol.RECEIPT
    const val STATUS = CaptureProtocol.STATUS
    const val PHONE_CAPABILITY = CaptureProtocol.PHONE_CAPABILITY
    const val MAX_HEADER_BYTES = CaptureProtocol.MAX_HEADER_BYTES
    const val MAX_LIVE_HEADER_BYTES = CaptureProtocol.MAX_MESSAGE_BYTES

    data class LiveHeader(
        val protocol: String,
        val captureId: String,
        val sessionId: String,
        val format: CaptureFormat,
        val startedAt: String,
    )

    data class ArchiveHeader(
        val protocol: String,
        val captureId: String,
        val attemptId: String,
        val format: CaptureFormat,
        val frameCount: Long,
        val wavBytes: Long,
        val sha256: String,
        val finishDate: String,
        val timezone: String,
        val captureState: CaptureState = CaptureState.SAVED,
        val stopReason: String? = null,
        val createdAtUtc: String? = null,
        val startedAtUtc: String? = null,
        val finishedAtUtc: String? = null,
    )

    fun readJsonHeader(input: DataInputStream, maximum: Int): JSONObject {
        val size = input.readInt()
        if (size !in 2..maximum) throw IOException("Invalid capture channel header length")
        val bytes = ByteArray(size)
        input.readFully(bytes)
        return runCatching { JSONObject(String(bytes, StandardCharsets.UTF_8)) }
            .getOrElse { throw IOException("Invalid capture channel JSON header", it) }
    }

    fun isProtocol(json: JSONObject): Boolean = json.optString("protocol") == PROTOCOL

    fun liveHeader(json: JSONObject) = LiveHeader(
        protocol = json.optString("protocol"),
        captureId = json.optString("captureId"),
        sessionId = json.optString("sessionId"),
        format = formatFrom(json),
        startedAt = json.optString("startedAt"),
    )

    fun validLiveStart(header: LiveHeader, captureId: String, sessionId: String): Boolean =
        header.protocol == PROTOCOL && header.captureId == captureId && header.sessionId == sessionId &&
            CaptureProtocol.validateFormat(header.format) && runCatching { Instant.parse(header.startedAt) }.isSuccess

    fun formatFrom(json: JSONObject): CaptureFormat = CaptureFormat(
        sampleRate = json.optInt("sampleRate", -1),
        channels = json.optInt("channels", -1),
        bitsPerSample = json.optInt("bitsPerSample", 16),
    )

    fun archiveHeader(json: JSONObject) = ArchiveHeader(
        protocol = json.optString("protocol"),
        captureId = json.optString("captureId"),
        attemptId = json.optString("attemptId"),
        format = formatFrom(json),
        frameCount = json.optLong("frameCount", -1),
        wavBytes = json.optLong("wavBytes", -1),
        sha256 = json.optString("sha256"),
        finishDate = json.optString("finishDate"),
        timezone = json.optString("timezone"),
        captureState = CaptureState.valueOf(json.optString("captureState", "INTERRUPTED")),
        stopReason = json.optString("stopReason").takeIf(String::isNotBlank),
        createdAtUtc = json.optString("createdAtUtc").takeIf(String::isNotBlank),
        startedAtUtc = json.optString("startedAtUtc").takeIf(String::isNotBlank),
        finishedAtUtc = json.optString("finishedAtUtc").takeIf(String::isNotBlank),
    )

    fun validArchiveHeader(header: ArchiveHeader, captureId: String, attemptId: String): Boolean {
        if (header.protocol != PROTOCOL || header.captureId != captureId || header.attemptId != attemptId ||
            !CaptureProtocol.validateFormat(header.format) ||
            header.captureState !in setOf(CaptureState.SAVED, CaptureState.INTERRUPTED) ||
            (header.stopReason?.length ?: 0) > 160 ||
            listOf(header.createdAtUtc, header.startedAtUtc, header.finishedAtUtc)
                .any { it != null && runCatching { Instant.parse(it) }.isFailure }) return false
        val alignment = header.format.channels * header.format.bitsPerSample / 8
        return header.frameCount >= 0 &&
            header.frameCount <= (Long.MAX_VALUE - 44) / alignment &&
            header.wavBytes == 44L + header.frameCount * alignment &&
            header.sha256.matches(Regex("(?i)^[0-9a-f]{64}$")) &&
            runCatching { LocalDate.parse(header.finishDate); ZoneId.of(header.timezone) }.isSuccess
    }
}
