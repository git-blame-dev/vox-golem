package dev.gitblame.voxgolem.assistant.recognition

import java.io.ByteArrayOutputStream
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import org.junit.Assert.*
import org.junit.Test

class PreRollPcmPumpTest {
    private class Capture(private val frames: Int) : PreRollPcmPump.Capture {
        val waiting = CountDownLatch(1)
        val stopped = CountDownLatch(1)
        val released = CountDownLatch(1)
        private var next = 1
        override fun read(buffer: ByteArray): Int {
            if (next <= frames) { buffer[0] = next++.toByte(); return 1 }
            waiting.countDown()
            check(stopped.await(2, TimeUnit.SECONDS))
            // Model a final read completing after stop, not an empty synthetic EOF.
            buffer[0] = next.toByte()
            return 1
        }
        override fun stop() { stopped.countDown() }
        override fun release() { released.countDown() }
    }
    private class Output : ByteArrayOutputStream() {
        val eof = CountDownLatch(1)
        override fun close() { eof.countDown(); super.close() }
    }

    @Test fun gracefulStopFlushesBoundedPreRollAndInFlightTailBeforeEof() {
        val capture = Capture(5)
        val output = Output()
        val pump = PreRollPcmPump(capture, output, 1, 3)
        assertTrue(capture.waiting.await(2, TimeUnit.SECONDS))
        pump.startStreaming()
        pump.stopCapture()
        assertTrue(output.eof.await(2, TimeUnit.SECONDS))
        assertArrayEquals(byteArrayOf(3, 4, 5, 6), output.toByteArray())
        assertTrue(capture.released.await(2, TimeUnit.SECONDS))
        assertFalse(pump.isRecording)
    }

    @Test fun cancellationDiscardsRingAndRetiresCaptureWithoutWaitingForBlockedWrite() {
        val capture = Capture(2)
        val writing = CountDownLatch(1)
        val closed = CountDownLatch(1)
        val output = object : java.io.OutputStream() {
            override fun write(value: Int) {
                writing.countDown()
                check(closed.await(2, TimeUnit.SECONDS))
                throw java.io.IOException("synthetic retired consumer")
            }
            override fun close() { closed.countDown() }
        }
        val pump = PreRollPcmPump(capture, output, 1, 3)
        assertTrue(capture.waiting.await(2, TimeUnit.SECONDS))
        pump.startStreaming()
        pump.stopCapture()
        assertTrue(writing.await(2, TimeUnit.SECONDS))
        pump.close()
        assertEquals(0L, closed.count)
        assertTrue(capture.released.await(2, TimeUnit.SECONDS))
    }

    @Test fun cancelBeforeStreamingDiscardsBufferedAudio() {
        val capture = Capture(5)
        val output = Output()
        val pump = PreRollPcmPump(capture, output, 1, 3)
        assertTrue(capture.waiting.await(2, TimeUnit.SECONDS))
        pump.close()
        assertTrue(capture.released.await(2, TimeUnit.SECONDS))
        assertArrayEquals(byteArrayOf(), output.toByteArray())
    }
}
