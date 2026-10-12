package dev.gitblame.voxgolem.capture

import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test
import java.io.File
import java.nio.file.Files
import java.security.MessageDigest

class PcmRecordingWriterTest {
    @Test
    fun appendsCheckpointsAndFinalizesValidWavWithExactPcmAndHash() {
        val directory = Files.createTempDirectory("capture-writer-test").toFile()
        try {
            val captureDirectory = File(directory, "123e4567-e89b-42d3-a456-426614174000").apply { mkdirs() }
            val spool = File(captureDirectory, "audio.pcm.part")
            val wav = File(captureDirectory, "audio.wav")
            val pcm = byteArrayOf(0, 1, 2, 3, 4, 5, 6, 7)
            val writer = PcmRecordingWriter(spool, CaptureFormat())
            writer.append(pcm, 4)
            assertEquals(PcmCheckpoint(4, 2), writer.checkpoint())
            writer.append(pcm.copyOfRange(4, 8))
            val manifest = writer.finish(wav)

            val bytes = wav.readBytes()
            assertEquals("RIFF", String(bytes, 0, 4, Charsets.US_ASCII))
            assertEquals(44L + pcm.size, bytes.size.toLong())
            assertEquals(pcm.toList(), bytes.copyOfRange(44, bytes.size).toList())
            assertEquals(4L, manifest.frameCount)
            assertEquals(pcm.size.toLong(), manifest.pcmByteLength)
            assertEquals(bytes.size.toLong(), manifest.wavByteLength)
            assertEquals(MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }, manifest.sha256)
        } finally {
            directory.deleteRecursively()
        }
    }

    @Test
    fun rejectsPartialPcmFramesAndLeavesAnIncompleteSpoolRecoverable() {
        val directory = Files.createTempDirectory("capture-writer-test").toFile()
        try {
            val spool = File(directory, "audio.pcm.part")
            val writer = PcmRecordingWriter(spool, CaptureFormat())
            assertThrows(IllegalArgumentException::class.java) { writer.append(byteArrayOf(1), 1) }
            writer.append(byteArrayOf(1, 2), 2)
            writer.close()
            assertEquals(2L, spool.length())
            assertEquals(PcmCheckpoint(2, 1), PcmRecordingWriter(spool, CaptureFormat()).use { it.checkpoint() })
        } finally {
            directory.deleteRecursively()
        }
    }
}
