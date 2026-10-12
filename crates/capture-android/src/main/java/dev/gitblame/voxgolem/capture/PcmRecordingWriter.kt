package dev.gitblame.voxgolem.capture

import java.io.BufferedInputStream
import java.io.BufferedOutputStream
import java.io.Closeable
import java.io.DataOutputStream
import java.io.File
import java.io.FileInputStream
import java.io.FileOutputStream
import java.io.IOException
import java.security.DigestOutputStream
import java.security.MessageDigest

data class PcmCheckpoint(val pcmByteLength: Long, val frameCount: Long)

/** Append-only PCM spool writer. A checkpoint is durable before its length is returned. */
class PcmRecordingWriter(private val file: File, private val format: CaptureFormat) : Closeable {
    private val frameBytes: Int
    private var output: FileOutputStream
    private var byteLength: Long
    private var closed = false

    init {
        require(CaptureProtocol.validateFormat(format)) { "Unsupported PCM format" }
        frameBytes = format.channels * format.bitsPerSample / 8
        file.parentFile?.let { if (!it.exists() && !it.mkdirs()) throw IOException("Cannot create PCM spool directory") }
        if (file.exists() && file.length() % frameBytes != 0L) throw IOException("PCM spool ends with a partial frame")
        byteLength = if (file.exists()) file.length() else 0L
        output = FileOutputStream(file, true)
    }

    @Synchronized
    fun append(bytes: ByteArray, count: Int = bytes.size) {
        check(!closed) { "PCM writer is closed" }
        require(count in 0..bytes.size) { "Invalid byte count" }
        require(count % frameBytes == 0) { "PCM bytes must contain whole frames" }
        output.write(bytes, 0, count)
        byteLength += count
    }

    @Synchronized
    fun checkpoint(): PcmCheckpoint {
        check(!closed) { "PCM writer is closed" }
        output.flush()
        output.fd.sync()
        return PcmCheckpoint(byteLength, byteLength / frameBytes)
    }

    fun finish(wavFile: File): CaptureManifest {
        val captureId = file.parentFile?.name
            ?: throw IllegalArgumentException("PCM spool must be stored in its capture-id directory")
        require(CaptureProtocol.isValidUuid(captureId)) { "PCM spool directory is not a capture UUID" }
        return finish(wavFile, captureId)
    }

    @Synchronized
    fun finish(wavFile: File, captureId: String): CaptureManifest {
        check(!closed) { "PCM writer is closed" }
        require(CaptureProtocol.isValidUuid(captureId)) { "Invalid capture UUID" }
        output.flush()
        output.fd.sync()
        output.close()
        closed = true
        val pcmLength = byteLength
        require(pcmLength <= 0xffff_ffffL - 36L) { "PCM recording exceeds RIFF/WAV size limit" }
        val targetParent = wavFile.parentFile
        if (targetParent != null && !targetParent.exists() && !targetParent.mkdirs()) throw IOException("Cannot create WAV directory")
        if (wavFile.exists()) throw IOException("WAV destination already exists")
        val temporary = File(wavFile.parentFile ?: File("."), "${wavFile.name}.part")
        if (temporary.exists() && !temporary.delete()) throw IOException("Cannot replace incomplete WAV temporary")

        val digest = MessageDigest.getInstance("SHA-256")
        try {
            FileOutputStream(temporary).use { raw ->
                DigestOutputStream(BufferedOutputStream(raw), digest).use { digestStream ->
                    val header = DataOutputStream(digestStream)
                    writeWavHeader(header, format, pcmLength)
                    BufferedInputStream(FileInputStream(file)).use { input -> input.copyTo(digestStream, DEFAULT_BUFFER_SIZE) }
                    digestStream.flush()
                    raw.fd.sync()
                }
            }
            if (!temporary.renameTo(wavFile)) throw IOException("Cannot finalize WAV file")
        } catch (failure: Throwable) {
            temporary.delete()
            throw failure
        }
        val wavLength = wavFile.length()
        return CaptureManifest(
            captureId = captureId,
            format = format,
            frameCount = pcmLength / frameBytes,
            pcmByteLength = pcmLength,
            wavByteLength = wavLength,
            sha256 = digest.digest().joinToString("") { "%02x".format(it) },
        )
    }

    @Synchronized
    override fun close() {
        if (!closed) {
            output.flush()
            output.fd.sync()
            output.close()
            closed = true
        }
    }

    private fun writeWavHeader(output: DataOutputStream, format: CaptureFormat, pcmBytes: Long) {
        val byteRate = format.sampleRate * format.channels * format.bitsPerSample / 8
        output.writeBytes("RIFF")
        writeLe32(output, pcmBytes + 36)
        output.writeBytes("WAVE")
        output.writeBytes("fmt ")
        writeLe32(output, 16)
        writeLe16(output, 1)
        writeLe16(output, format.channels)
        writeLe32(output, format.sampleRate.toLong())
        writeLe32(output, byteRate.toLong())
        writeLe16(output, format.channels * format.bitsPerSample / 8)
        writeLe16(output, format.bitsPerSample)
        output.writeBytes("data")
        writeLe32(output, pcmBytes)
    }

    private fun writeLe16(output: DataOutputStream, value: Int) {
        output.writeByte(value and 0xff)
        output.writeByte(value ushr 8 and 0xff)
    }

    private fun writeLe32(output: DataOutputStream, value: Long) {
        repeat(4) { shift -> output.writeByte((value ushr (shift * 8) and 0xff).toInt()) }
    }
}
