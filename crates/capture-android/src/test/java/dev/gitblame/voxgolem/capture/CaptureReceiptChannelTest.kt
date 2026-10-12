package dev.gitblame.voxgolem.capture

import org.junit.Assert.*
import org.junit.Test
import java.io.ByteArrayOutputStream
import java.io.DataOutputStream
import java.io.EOFException
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

class CaptureReceiptChannelTest {
    @Test fun lengthFramingPreservesExactMaximumAndRejectsTruncatedTrailingAndOversizedStreams() {
        val bytes = ByteArray(CaptureProtocol.MAX_CHANNEL_RESULT_BYTES) { 32 }
        val output = ByteArrayOutputStream(); CaptureReceiptChannel.write(output, bytes)
        val framed = output.toByteArray()
        assertArrayEquals(bytes, CaptureReceiptChannel.read(framed.inputStream()))
        assertThrows(EOFException::class.java) { CaptureReceiptChannel.read(framed.copyOf(framed.size - 1).inputStream()) }
        assertThrows(IllegalArgumentException::class.java) { CaptureReceiptChannel.read((framed + byteArrayOf(1)).inputStream()) }
        for (size in listOf(-1, 0, CaptureProtocol.MAX_CHANNEL_RESULT_BYTES + 1, Int.MAX_VALUE)) {
            val header = ByteArrayOutputStream().apply { DataOutputStream(this).writeInt(size) }
            assertThrows(IllegalArgumentException::class.java) { CaptureReceiptChannel.read(header.toByteArray().inputStream()) }
        }
    }
    @Test fun finiteDeadlineRejectsLateCommitAndClosesLateResource() {
        val nativeClosed = CountDownLatch(1)
        val lifetime = CaptureChannelRetirementOwner(1).reserve(20)!!
        lifetime.acquiredNative { completed -> nativeClosed.countDown(); completed(true) }
        lifetime.beginStreamAcquisition()
        assertTrue(nativeClosed.await(2, TimeUnit.SECONDS))
        var committed = false
        assertFalse(lifetime.ifActive { committed = true })
        assertFalse(committed)
        val streamClosed = CountDownLatch(1)
        assertFalse(lifetime.streamAcquisitionCompleted(AutoCloseable { streamClosed.countDown() }))
        assertTrue(streamClosed.await(2, TimeUnit.SECONDS))
        lifetime.close()
    }
}
