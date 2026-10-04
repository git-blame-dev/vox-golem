package dev.gitblame.voxgolem.capture

import java.io.Closeable
import java.io.File
import java.io.IOException
import java.io.RandomAccessFile

/** Reads the data chunk from a validated PCM16 RIFF/WAVE file without buffering the recording. */
internal class WavPcmReader(file: File, expected: CaptureFormat) : Closeable {
    private val input = RandomAccessFile(file, "r")
    private val dataStart: Long
    private val dataLength: Long
    private val frameBytes: Int
    private var remaining: Long

    val frameCount: Long

    init {
        try {
            val fileLength = input.length()
            if (fileLength < 12 || readFourCc() != "RIFF") throw IOException("Invalid WAV RIFF header")
            val riffLength = readUInt32()
            if (readFourCc() != "WAVE" || riffLength + 8 > fileLength || riffLength < 4) throw IOException("Invalid WAV length or form")
            val riffEnd = riffLength + 8
            var format: CaptureFormat? = null
            var foundData: Pair<Long, Long>? = null
            while (input.filePointer + 8 <= riffEnd) {
                val chunkId = readFourCc()
                val chunkLength = readUInt32()
                val chunkStart = input.filePointer
                val chunkEnd = chunkStart + chunkLength
                if (chunkEnd < chunkStart || chunkEnd > riffEnd) throw IOException("WAV chunk exceeds RIFF bounds")
                when (chunkId) {
                    "fmt " -> {
                        if (chunkLength < 16) throw IOException("WAV fmt chunk is incomplete")
                        val encoding = readUInt16()
                        val channels = readUInt16()
                        val sampleRate = readUInt32()
                        readUInt32() // byte rate
                        readUInt16() // block alignment
                        val bits = readUInt16()
                        if (encoding != 1) throw IOException("WAV audio must be PCM")
                        format = CaptureFormat(sampleRate.toInt(), channels, bits)
                    }
                    "data" -> if (foundData == null) foundData = chunkStart to chunkLength
                }
                input.seek(chunkEnd + (chunkLength and 1L))
            }
            if (format != expected || !CaptureProtocol.validateFormat(format ?: expected)) {
                throw IOException("WAV PCM format does not match capture metadata")
            }
            val (start, length) = foundData ?: throw IOException("WAV data chunk is missing")
            frameBytes = expected.channels * expected.bitsPerSample / 8
            if (length % frameBytes != 0L) throw IOException("WAV data chunk ends inside a PCM frame")
            dataStart = start
            dataLength = length
            remaining = length
            frameCount = length / frameBytes
            input.seek(dataStart)
        } catch (failure: Exception) {
            input.close()
            throw failure
        }
    }

    /** Returns up to the requested complete PCM frames, or null after the data chunk. */
    fun read(maximumBytes: Int = frameBytes * 1600): ByteArray? {
        require(maximumBytes >= frameBytes)
        if (remaining == 0L) return null
        val request = minOf(remaining, (maximumBytes / frameBytes * frameBytes).toLong()).toInt()
        if (request == 0) throw IOException("PCM read size is smaller than a frame")
        val bytes = ByteArray(request)
        input.readFully(bytes)
        remaining -= request
        return bytes
    }

    override fun close() = input.close()

    private fun readFourCc(): String {
        val bytes = ByteArray(4)
        input.readFully(bytes)
        return String(bytes, Charsets.US_ASCII)
    }

    private fun readUInt16(): Int = input.readUnsignedByte() or (input.readUnsignedByte() shl 8)
    private fun readUInt32(): Long = input.readUnsignedByte().toLong() or
        (input.readUnsignedByte().toLong() shl 8) or
        (input.readUnsignedByte().toLong() shl 16) or
        (input.readUnsignedByte().toLong() shl 24)
}
