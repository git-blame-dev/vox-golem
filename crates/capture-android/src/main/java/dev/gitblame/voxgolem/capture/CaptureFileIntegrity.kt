package dev.gitblame.voxgolem.capture

import java.io.File
import java.io.InputStream
import java.io.OutputStream
import java.security.MessageDigest

/** Shared streaming copy/read-back proof for private and SAF capture files. */
object CaptureFileIntegrity {
    data class Digest(val byteLength: Long, val sha256: String)
    fun digest(file: File): Digest = file.inputStream().buffered().use { digest(it) }
    fun digest(input: InputStream, output: OutputStream? = null): Digest {
        val hash = MessageDigest.getInstance("SHA-256")
        var length = 0L
        val buffer = ByteArray(64 * 1024)
        while (true) {
            val count = input.read(buffer)
            if (count < 0) break
            if (count == 0) continue
            output?.write(buffer, 0, count)
            hash.update(buffer, 0, count)
            length = Math.addExact(length, count.toLong())
        }
        return Digest(length, hash.digest().joinToString("") { "%02x".format(it) })
    }
    fun canonicalManifest(captureId: String, file: File, format: CaptureFormat): CaptureManifest =
        WavPcmReader(file, format, requireCanonical = true).use { reader ->
            CaptureManifest(captureId = captureId, format = format, frameCount = reader.frameCount,
                pcmByteLength = reader.frameCount * format.channels * format.bitsPerSample / 8,
                wavByteLength = file.length(), sha256 = digest(file).sha256)
                .also { require(CaptureProtocol.validateManifest(it)) }
        }
}
