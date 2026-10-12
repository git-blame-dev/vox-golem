package dev.gitblame.voxgolem.android.wear.capture

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import dev.gitblame.voxgolem.capture.CaptureFormat
import dev.gitblame.voxgolem.capture.WavPcmReader
import java.io.IOException
import java.security.MessageDigest
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

    @Test fun checkpointedWriterReopensAndEverySelectedCaptureRateRoundTripsThroughProductionReader() {
        withCaptureDirectory { directory ->
            for (rate in listOf(16_000, 24_000, 48_000)) {
                val pcm = File(directory, "audio-$rate.pcm.part")
                val wav = File(directory, "audio-$rate.wav")
                val prefix = byteArrayOf(1, 0, 2, 0)
                val suffix = byteArrayOf(3, 0, 4, 0)
                PcmWavWriter(pcm, wav, rate).use { writer ->
                    writer.append(prefix)
                    writer.checkpoint()
                    assertArrayEquals(prefix, pcm.readBytes())
                }
                val manifest = PcmWavWriter(pcm, wav, rate).use { writer ->
                    assertEquals(2L, writer.frameCount)
                    writer.append(suffix)
                    val finished = writer.finish()
                    assertEquals(finished, writer.finish())
                    assertThrows(IllegalStateException::class.java) { writer.append(prefix) }
                    finished
                }
                WavPcmReader(wav, CaptureFormat(rate, 1, 16)).use { reader ->
                    assertEquals(4L, reader.frameCount)
                    assertArrayEquals(prefix.copyOfRange(0, 2), reader.read(3))
                    assertArrayEquals(prefix.copyOfRange(2, 4) + suffix, reader.read(32))
                    assertNull(reader.read(32))
                }
                assertEquals(MessageDigest.getInstance("SHA-256").digest(wav.readBytes())
                    .joinToString("") { "%02x".format(it) }, manifest.sha256)
                assertFalse(File(directory, "${wav.name}.part").exists())
            }
        }
    }

    @Test fun rejectedByteCountsLeaveTheSpoolAndFrameCountUnchanged() {
        withCaptureDirectory { directory ->
            val pcm = File(directory, "audio.pcm.part")
            val wav = File(directory, "audio.wav")
            PcmWavWriter(pcm, wav, 16_000).use { writer ->
                val samples = byteArrayOf(1, 0, 2, 0)
                writer.append(samples)
                for (count in listOf(-1, 1, 3, 5)) {
                    assertThrows(IllegalArgumentException::class.java) { writer.append(samples, count) }
                    assertEquals(2L, writer.frameCount)
                    assertArrayEquals(samples, pcm.readBytes())
                }
                writer.append(samples, 0)
                assertEquals(2L, writer.finish().frameCount)
            }
        }
    }

    @Test fun existingWavIsNeverOverwrittenAndFailedFinalizationRetainsCheckpointedPcm() {
        withCaptureDirectory { directory ->
            val pcm = File(directory, "audio.pcm.part")
            val wav = File(directory, "audio.wav")
            val existing = "synthetic conflicting WAV".toByteArray()
            wav.writeBytes(existing)
            val samples = byteArrayOf(1, 0, 2, 0)
            PcmWavWriter(pcm, wav, 16_000).use { writer ->
                writer.append(samples)
                writer.checkpoint()
                assertThrows(IOException::class.java) { writer.finish() }
            }
            assertArrayEquals(existing, wav.readBytes())
            assertArrayEquals(samples, pcm.readBytes())
            assertFalse(File(directory, "audio.wav.part").exists())
        }
    }

    @Test fun productionReaderRejectsMetadataMismatchTruncatedDataAndTornPcmSpools() {
        withCaptureDirectory { directory ->
            val pcm = File(directory, "audio.pcm.part")
            val wav = File(directory, "audio.wav")
            PcmWavWriter(pcm, wav, 16_000).use { writer ->
                writer.append(byteArrayOf(1, 0, 2, 0))
                writer.finish()
            }
            assertThrows(IOException::class.java) { WavPcmReader(wav, CaptureFormat(24_000)).close() }
            wav.writeBytes(wav.readBytes().dropLast(1).toByteArray())
            assertThrows(IOException::class.java) { WavPcmReader(wav).close() }
            val torn = byteArrayOf(1, 0, 2)
            pcm.writeBytes(torn)
            assertThrows(IOException::class.java) { PcmWavWriter(pcm, wav, 16_000).close() }
            assertArrayEquals(torn, pcm.readBytes())
        }
    }

    private fun withCaptureDirectory(test: (File) -> Unit) {
        val root = Files.createTempDirectory("synthetic-wav-contract").toFile()
        try { test(File(root, "123e4567-e89b-42d3-a456-426614174060").apply { mkdirs() }) }
        finally { root.deleteRecursively() }
    }
}
