package dev.gitblame.voxgolem.android.wear.capture

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File
import java.nio.file.Files

class PcmWavWriterTest {
    @Test fun writesMonoPcm16WaveAndCountsFrames() {
        val temporaryRoot = Files.createTempDirectory("capture-writer").toFile()
        val directory = File(temporaryRoot, "123e4567-e89b-42d3-a456-426614174000").apply { mkdirs() }
        try {
            val pcm = File(directory, "audio.pcm.part")
            val wav = File(directory, "audio.wav")
            val samples = byteArrayOf(0, 1, 0xff.toByte(), 0x7f, 0, 0x80.toByte(), 42, 0)
            val writer = PcmWavWriter(pcm, wav, 16_000)
            writer.append(samples, 4)
            writer.checkpoint()
            writer.append(samples.copyOfRange(4, samples.size))
            assertEquals(4, writer.frameCount)
            val manifest = writer.finish()

            val result = wav.readBytes()
            assertEquals("RIFF", String(result, 0, 4))
            assertEquals(44 + samples.size, result.size)
            assertEquals("WAVEfmt ", String(result, 8, 8))
            assertEquals(16_000, readLe32(result, 24))
            assertEquals("data", String(result, 36, 4))
            assertArrayEquals(samples, result.copyOfRange(44, result.size))
            assertTrue(pcm.exists())
            assertEquals(4, manifest.frameCount)
            assertEquals(result.size.toLong(), manifest.wavByteLength)
            assertEquals(64, manifest.sha256.length)
        } finally {
            temporaryRoot.deleteRecursively()
        }
    }

    private fun readLe32(bytes: ByteArray, offset: Int): Int =
        (bytes[offset].toInt() and 255) or ((bytes[offset + 1].toInt() and 255) shl 8) or
            ((bytes[offset + 2].toInt() and 255) shl 16) or ((bytes[offset + 3].toInt() and 255) shl 24)
}
