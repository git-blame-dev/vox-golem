package dev.gitblame.voxgolem.capture

import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import java.io.File
import java.io.IOException
import java.nio.ByteBuffer
import java.nio.ByteOrder

class WavPcmReaderTest {
    @get:Rule val temporary = TemporaryFolder()
    private val id = "123e4567-e89b-42d3-a456-426614174000"

    private fun wav(format: CaptureFormat = CaptureFormat(), pcm: ByteArray = byteArrayOf(1, 2, 3, 4, 5, 6, 7, 8)): File {
        val directory = temporary.newFolder()
        val file = File(directory, "audio.wav")
        PcmRecordingWriter(File(directory, "audio.pcm.part"), format).use {
            it.append(pcm)
            it.finish(file, id)
        }
        return file
    }

    @Test fun writerOutputStreamsCompleteFramesAndStopsAtDataEnd() {
        val file = wav(CaptureFormat(channels = 2))
        WavPcmReader(file, CaptureFormat(channels = 2)).use {
            assertEquals(2L, it.frameCount)
            assertEquals(CaptureFormat(channels = 2), it.format)
            assertArrayEquals(byteArrayOf(1, 2, 3, 4), it.read(7))
            assertArrayEquals(byteArrayOf(5, 6, 7, 8), it.read(7))
            assertNull(it.read())
        }
    }

    @Test fun rejectsMismatchedFormatTruncatedRiffAndPartialFrames() {
        val file = wav()
        assertThrows(IOException::class.java) { WavPcmReader(file, CaptureFormat(sampleRate = 8_000)).close() }
        val canonical = file.readBytes()
        file.writeBytes(canonical.copyOf(canonical.size - 1))
        assertThrows(IOException::class.java) { WavPcmReader(file).close() }
        val partial = canonical.copyOf(canonical.size - 1)
        ByteBuffer.wrap(partial).order(ByteOrder.LITTLE_ENDIAN).putInt(4, partial.size - 8).putInt(40, 7)
        file.writeBytes(partial)
        assertThrows(IOException::class.java) { WavPcmReader(file).close() }
    }

    @Test fun skipsPaddedAncillaryChunksAndIgnoresBytesOutsideData() {
        val file = wav()
        val original = file.readBytes()
        val ancillary = byteArrayOf(74, 85, 78, 75, 1, 0, 0, 0, 99, 0)
        val extended = original.copyOfRange(0, 12) + ancillary + original.copyOfRange(12, original.size)
        ByteBuffer.wrap(extended).order(ByteOrder.LITTLE_ENDIAN).putInt(4, extended.size - 8)
        file.writeBytes(extended + byteArrayOf(9, 9))
        WavPcmReader(file).use {
            assertArrayEquals(original.copyOfRange(44, original.size), it.read())
            assertNull(it.read())
        }
    }

    @Test fun emptyDataHasNoFramesAndSubFrameRequestsAreRejected() {
        WavPcmReader(wav(pcm = byteArrayOf())).use {
            assertEquals(0L, it.frameCount)
            assertNull(it.read())
            assertThrows(IllegalArgumentException::class.java) { it.read(1) }
        }
    }
}
