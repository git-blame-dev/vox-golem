package dev.gitblame.voxgolem.assistant.recognition

import java.io.ByteArrayOutputStream
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import org.junit.Assert.*
import org.junit.Test

class PreRollPcmPumpTest {
    @Test fun blockedStopIsOffCallerAndCoalescesRepeatedRequestsBeforeWorkerEof() {
        val stopping = CountDownLatch(1)
        val unblock = CountDownLatch(1)
        val done = CountDownLatch(1)
        val stops = AtomicInteger()
        val caller = Thread.currentThread()
        val pump = PreRollPcmPump(object : PreRollPcmPump.Capture {
            override fun read(buffer: ByteArray): Int { check(unblock.await(5, TimeUnit.SECONDS)); return 0 }
            override fun stop() {
                assertNotSame(caller, Thread.currentThread())
                stops.incrementAndGet(); stopping.countDown()
                check(unblock.await(5, TimeUnit.SECONDS))
            }
            override fun release() { assertNotSame(caller, Thread.currentThread()) }
        }, Output(), 1, 3)
        pump.retire { assertTrue(it); done.countDown() }
        try {
            assertTrue(stopping.await(2, TimeUnit.SECONDS))
            repeat(100) { pump.stopCapture(); pump.close() }
            assertEquals(1, stops.get())
            assertEquals(1L, done.count)
        } finally { unblock.countDown() }
        assertTrue(done.await(2, TimeUnit.SECONDS))
        assertEquals(1, stops.get())
    }

    @Test fun blockedNativeReleaseDoesNotBlockStopOrConcurrentCloseAndAcknowledgesOnlyAfterRelease() {
        val releasing = CountDownLatch(1)
        val allowRelease = CountDownLatch(1)
        val completed = CountDownLatch(1)
        val calls = AtomicInteger()
        val pump = PreRollPcmPump(object : PreRollPcmPump.Capture {
            override fun read(buffer: ByteArray) = 0
            override fun stop() {}
            override fun release() {
                calls.incrementAndGet(); releasing.countDown()
                check(allowRelease.await(5, TimeUnit.SECONDS))
            }
        }, Output(), 1, 3)
        assertTrue(releasing.await(2, TimeUnit.SECONDS))
        val returned = CountDownLatch(2)
        try {
            repeat(2) { Thread { pump.stopCapture(); pump.close(); returned.countDown() }.start() }
            assertTrue("main-facing teardown must not wait for native release", returned.await(1, TimeUnit.SECONDS))
            pump.retire { assertTrue(it); completed.countDown() }
            assertEquals(1L, completed.count)
        } finally { allowRelease.countDown() }
        assertTrue(completed.await(2, TimeUnit.SECONDS))
        assertEquals(1, calls.get())
    }

    @Test fun workerEofStopAndReleaseFailuresRemainExplicitlyRetryableWithoutHotLoop() {
        for (failStop in listOf(true, false)) {
            val eof = CountDownLatch(1)
            val stops = AtomicInteger()
            val releases = AtomicInteger()
            val pump = PreRollPcmPump(object : PreRollPcmPump.Capture {
                override fun read(buffer: ByteArray): Int { check(eof.await(5, TimeUnit.SECONDS)); return 0 }
                override fun stop() {
                    if (stops.incrementAndGet() == 1 && failStop) error("synthetic stop failure")
                    eof.countDown()
                }
                override fun release() {
                    if (releases.incrementAndGet() == 1) error("synthetic release failure")
                }
            }, Output(), 1, 3)
            repeat(if (failStop) 2 else 1) {
                val failed = CountDownLatch(1)
                var success = true
                pump.retire { success = it; failed.countDown() }
                assertTrue(failed.await(2, TimeUnit.SECONDS))
                assertFalse(success)
            }
            val done = CountDownLatch(1)
            pump.retire { assertTrue(it); done.countDown() }
            assertTrue(done.await(2, TimeUnit.SECONDS))
            assertEquals(if (failStop) 2 else 1, stops.get())
            assertEquals(2, releases.get())
        }
    }

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
