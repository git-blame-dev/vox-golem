package dev.gitblame.voxgolem.assistant.recognition

import java.io.ByteArrayOutputStream
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import org.junit.Assert.*
import org.junit.Test

class PreRollPcmPumpTest {
    @Test fun successfulOutputCloseDoesNotReleaseCaptureUntilBlockedWriterActuallyReturns() {
        val capture = Capture(1)
        val writing = CountDownLatch(1); val closed = CountDownLatch(1); val returnWrite = CountDownLatch(1)
        val done = CountDownLatch(1)
        val pump = PreRollPcmPump(capture, object : java.io.OutputStream() {
            override fun write(value: Int) { writing.countDown(); check(returnWrite.await(5, TimeUnit.SECONDS)) }
            override fun close() { closed.countDown() }
        }, 1, 3).apply { startCapture() }
        try {
            assertTrue(capture.waiting.await(2, TimeUnit.SECONDS))
            pump.startStreaming(); pump.stopCapture()
            assertTrue(writing.await(2, TimeUnit.SECONDS))
            pump.retire { assertTrue(it); done.countDown() }
            assertTrue(closed.await(2, TimeUnit.SECONDS))
            assertFalse(done.await(100, TimeUnit.MILLISECONDS))
            assertEquals(1L, capture.released.count)
        } finally { returnWrite.countDown() }
        assertTrue(done.await(2, TimeUnit.SECONDS))
        assertEquals(0L, capture.released.count)
    }
    @Test fun blockedOutputCloseAfterEofCannotAcknowledgeRetirementOrBlockCaller() {
        val closing = CountDownLatch(1); val allowClose = CountDownLatch(1)
        val completed = CountDownLatch(1); val eof = CountDownLatch(1)
        val pump = PreRollPcmPump(object : PreRollPcmPump.Capture {
            override fun read(buffer: ByteArray): Int { check(eof.await(5, TimeUnit.SECONDS)); return 0 }
            override fun stop() { eof.countDown() }
            override fun release() {}
        }, object : java.io.OutputStream() {
            override fun write(value: Int) {}
            override fun close() { closing.countDown(); check(allowClose.await(5, TimeUnit.SECONDS)) }
        }, 1, 3).apply { startCapture() }
        try {
            eof.countDown()
            assertTrue(closing.await(2, TimeUnit.SECONDS))
            val returned = CountDownLatch(1)
            Thread { pump.retire { assertTrue(it); completed.countDown() }; returned.countDown() }.start()
            assertTrue("Retirement request must not wait for close", returned.await(1, TimeUnit.SECONDS))
            assertFalse("Blocked output close is still physically occupied", completed.await(100, TimeUnit.MILLISECONDS))
        } finally { allowClose.countDown() }
        assertTrue(completed.await(2, TimeUnit.SECONDS))
    }

    @Test fun outputCloseFailureIsRetainedUntilExplicitSuccessfulRetry() {
        val reads = CountDownLatch(1); val closes = AtomicInteger()
        val pump = PreRollPcmPump(object : PreRollPcmPump.Capture {
            override fun read(buffer: ByteArray): Int { check(reads.await(5, TimeUnit.SECONDS)); return 0 }
            override fun stop() { reads.countDown() }
            override fun release() {}
        }, object : java.io.OutputStream() {
            override fun write(value: Int) {}
            override fun close() { if (closes.incrementAndGet() == 1) error("synthetic output close failure") }
        }, 1, 3).apply { startCapture() }
        val failed = CountDownLatch(1); var first = true
        pump.retire { first = it; failed.countDown() }
        assertTrue(failed.await(2, TimeUnit.SECONDS)); assertFalse(first)
        assertEquals(1, closes.get())
        val done = CountDownLatch(1)
        pump.retire { assertTrue(it); done.countDown() }
        assertTrue(done.await(2, TimeUnit.SECONDS)); assertEquals(2, closes.get())
    }
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
        }, Output(), 1, 3).apply { startCapture() }
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
        }, Output(), 1, 3).apply { startCapture() }
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
            }, Output(), 1, 3).apply { startCapture() }
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
        val pump = PreRollPcmPump(capture, output, 1, 3).apply { startCapture() }
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
        val pump = PreRollPcmPump(capture, output, 1, 3).apply { startCapture() }
        assertTrue(capture.waiting.await(2, TimeUnit.SECONDS))
        pump.startStreaming()
        pump.stopCapture()
        assertTrue(writing.await(2, TimeUnit.SECONDS))
        pump.close()
        assertTrue(closed.await(2, TimeUnit.SECONDS))
        assertTrue(capture.released.await(2, TimeUnit.SECONDS))
    }

    @Test fun cancelBeforeStreamingDiscardsBufferedAudio() {
        val capture = Capture(5)
        val output = Output()
        val pump = PreRollPcmPump(capture, output, 1, 3).apply { startCapture() }
        assertTrue(capture.waiting.await(2, TimeUnit.SECONDS))
        pump.close()
        assertTrue(capture.released.await(2, TimeUnit.SECONDS))
        assertArrayEquals(byteArrayOf(), output.toByteArray())
    }
}

@org.junit.runner.RunWith(org.robolectric.RobolectricTestRunner::class)
@org.robolectric.annotation.Config(sdk = [35], manifest = org.robolectric.annotation.Config.NONE)
class AndroidPreRollRetirementTest {
    private fun retire(source: PreRollAudioSource): Boolean {
        val done = CountDownLatch(1); var success = false
        source.retire { success = it; done.countDown() }
        assertTrue(done.await(2, TimeUnit.SECONDS)); return success
    }

    @Test fun failureWithoutReturnedHandlesIsNullWithoutNativeCleanupOrWriterStartup() {
        val source = AndroidPreRollAudioSource.open(object : AndroidPreRollAudioSource.Acquisition {
            override fun pipe(): Pair<android.os.ParcelFileDescriptor, android.os.ParcelFileDescriptor> =
                error("synthetic failure without returned handles")
            override fun capture(): AndroidPreRollAudioSource.AcquiredCapture = error("No capture acquisition")
            override fun startWriter(writer: Thread) { error("No writer startup") }
        })
        assertNull(source)
    }

    @Test fun successfulAcquisitionStartsOnlyOneWriterAndRetiresTheSameCaptureAndDescriptors() {
        val effects = PreRollAcquisitionFixture(blockRead = true)
        try {
            val source = AndroidPreRollAudioSource.open(effects)
            assertNotNull(source); assertTrue(source!!.isRecording)
            assertTrue(effects.reading.await(2, TimeUnit.SECONDS))
            val done = CountDownLatch(1)
            source.retire { assertTrue(it); done.countDown() }
            effects.allowRead.countDown(); assertTrue(done.await(2, TimeUnit.SECONDS))
            assertEquals(1, effects.starts.get()); assertEquals(1, effects.writerStarts.get())
            assertEquals(1, effects.captureCalls.get()); assertEquals(1, effects.releases.get())
            assertFalse(effects.pipe.first.fileDescriptor.valid()); assertFalse(effects.pipe.second.fileDescriptor.valid())
        } finally { effects.close() }
    }

    @Test fun recordConstructionFailureRetainsPipeUntilFailedReadCloseIsExplicitlyRetried() {
        val effects = PreRollAcquisitionFixture().apply { failCapture = true; failReadClose = true }
        try {
            val source = AndroidPreRollAudioSource.open(effects)
            assertNotNull("Acquired pipe must have a cleanup owner", source)
            assertFalse(source!!.isRecording); assertFalse(retire(source))
            assertTrue(effects.pipe.first.fileDescriptor.valid())
            val attempts = effects.readCloses.get()
            repeat(100) { source.close() }
            assertEquals("Failed cleanup waits for explicit retire retry", attempts, effects.readCloses.get())
            effects.failReadClose = false
            assertTrue(retire(source)); assertFalse(effects.pipe.first.fileDescriptor.valid())
            assertFalse(effects.pipe.second.fileDescriptor.valid())
        } finally { effects.close() }
    }

    @Test fun uninitializedRecordReleaseFailureRetainsTheOriginalCaptureAndDescriptors() {
        val effects = PreRollAcquisitionFixture().apply { initialized = false; failRelease = true }
        try {
            val source = AndroidPreRollAudioSource.open(effects)
            assertNotNull(source); assertFalse(source!!.isRecording); assertFalse(retire(source))
            assertEquals(0, effects.starts.get()); assertEquals(0, effects.stops.get())
            effects.failRelease = false
            assertTrue(retire(source)); assertEquals(1, effects.captureCalls.get())
            assertFalse(effects.pipe.first.fileDescriptor.valid()); assertFalse(effects.pipe.second.fileDescriptor.valid())
        } finally { effects.close() }
    }

    @Test fun startFailureAfterActivationRequiresConfirmedStopBeforeCleanupCanSucceed() {
        val effects = PreRollAcquisitionFixture().apply { failStart = true; failStop = true }
        try {
            val source = AndroidPreRollAudioSource.open(effects)
            assertNotNull(source); assertFalse(source!!.isRecording); assertFalse(retire(source))
            assertTrue(effects.recording); assertEquals(0, effects.releases.get())
            effects.failStop = false
            assertTrue(retire(source)); assertFalse(effects.recording)
            assertEquals(1, effects.releases.get())
        } finally { effects.close() }
    }

    @Test fun outputConstructionFailureRetiresRawWriteDescriptorWithExplicitCloseRetry() {
        val effects = PreRollAcquisitionFixture().apply { failOutput = true; failWriteClose = true }
        try {
            val source = AndroidPreRollAudioSource.open(effects)
            assertNotNull(source); assertFalse(source!!.isRecording); assertFalse(retire(source))
            assertTrue(effects.pipe.second.fileDescriptor.valid())
            effects.failWriteClose = false
            assertTrue(retire(source)); assertFalse(effects.pipe.second.fileDescriptor.valid())
        } finally { effects.close() }
    }

    @Test fun writerStartFailureBeforeStartingNeverReadsAndStillRetiresAllAcquiredResources() {
        val effects = PreRollAcquisitionFixture().apply { writerFailure = "before"; failReadClose = true }
        try {
            val source = AndroidPreRollAudioSource.open(effects)
            assertNotNull(source); assertFalse(source!!.isRecording); assertFalse(retire(source))
            assertEquals(0, effects.reads.get()); assertEquals(1, effects.writerStarts.get())
            effects.failReadClose = false
            assertTrue(retire(source)); assertEquals(1, effects.releases.get())
        } finally { effects.close() }
    }

    @Test fun writerStartFailureAfterActualStartCannotRetireUntilTheRealWriterReturns() {
        val effects = PreRollAcquisitionFixture(blockRead = true).apply { writerFailure = "after" }
        val done = CountDownLatch(1)
        try {
            val source = AndroidPreRollAudioSource.open(effects)
            assertNotNull(source); assertFalse(source!!.isRecording)
            assertTrue(effects.reading.await(2, TimeUnit.SECONDS))
            source.retire { assertTrue(it); done.countDown() }
            assertFalse(done.await(100, TimeUnit.MILLISECONDS)); assertEquals(0, effects.releases.get())
            effects.allowRead.countDown()
            assertTrue(done.await(2, TimeUnit.SECONDS)); assertEquals(1, effects.writerStarts.get())
            assertEquals(1, effects.releases.get())
        } finally { effects.close() }
    }

    @Test fun blockedAcquisitionCleanupDoesNotBlockOpenOrLoseItsOwner() {
        val effects = PreRollAcquisitionFixture().apply { failCapture = true; blockReadClose = true }
        val executor = java.util.concurrent.Executors.newSingleThreadExecutor()
        try {
            val result = executor.submit<AndroidPreRollAudioSource?> { AndroidPreRollAudioSource.open(effects) }
            assertTrue(effects.closingRead.await(2, TimeUnit.SECONDS))
            val source = result.get(1, TimeUnit.SECONDS)
            assertNotNull(source); assertFalse(source!!.isRecording)
            val done = CountDownLatch(1)
            source.retire { assertTrue(it); done.countDown() }
            assertFalse(done.await(100, TimeUnit.MILLISECONDS))
            effects.allowReadClose.countDown(); assertTrue(done.await(2, TimeUnit.SECONDS))
        } finally { effects.close(); executor.shutdownNow(); assertTrue(executor.awaitTermination(2, TimeUnit.SECONDS)) }
    }
    @Test fun blockedReadDescriptorCloseIsOffCallerAndCannotAcknowledgeRetirement() {
        val descriptors = android.os.ParcelFileDescriptor.createPipe()
        val stopped = CountDownLatch(1); val closing = CountDownLatch(1); val allowClose = CountDownLatch(1)
        val done = CountDownLatch(1)
        val source = AndroidPreRollAudioSource(descriptors[0], object : PreRollPcmPump.Capture {
            override fun read(buffer: ByteArray): Int { check(stopped.await(5, TimeUnit.SECONDS)); return 0 }
            override fun stop() { stopped.countDown() }
            override fun release() {}
        }, ByteArrayOutputStream(), { descriptor ->
            closing.countDown(); check(allowClose.await(5, TimeUnit.SECONDS)); descriptor.close()
        })
        try {
            source.retire { assertTrue(it); done.countDown() }
            assertTrue(closing.await(2, TimeUnit.SECONDS))
            assertFalse(done.await(100, TimeUnit.MILLISECONDS))
        } finally { allowClose.countDown() }
        assertTrue(done.await(2, TimeUnit.SECONDS))
        source.close(); descriptors.forEach { it.close() }
    }
    @Test fun sourceReadDescriptorCloseFailureRetainsOwnershipUntilExplicitRetryOffCaller() {
        val descriptors = android.os.ParcelFileDescriptor.createPipe()
        val stopped = CountDownLatch(1); val calls = AtomicInteger()
        val caller = Thread.currentThread()
        val closedOnCaller = java.util.concurrent.atomic.AtomicBoolean()
        val source = AndroidPreRollAudioSource(descriptors[0], object : PreRollPcmPump.Capture {
            override fun read(buffer: ByteArray): Int { check(stopped.await(5, TimeUnit.SECONDS)); return 0 }
            override fun stop() { stopped.countDown() }
            override fun release() {}
        }, ByteArrayOutputStream(), { descriptor ->
            if (caller === Thread.currentThread()) closedOnCaller.set(true)
            if (calls.incrementAndGet() == 1) throw java.io.IOException("synthetic read close failure")
            descriptor.close()
        })
        try {
            val failed = CountDownLatch(1); var first = true
            source.retire { first = it; failed.countDown() }
            assertTrue(failed.await(2, TimeUnit.SECONDS)); assertFalse(first)
            assertEquals(1, calls.get())
            val done = CountDownLatch(1)
            source.retire { assertTrue(it); done.countDown() }
            assertTrue(done.await(2, TimeUnit.SECONDS)); assertEquals(2, calls.get())
            assertFalse("Descriptor close must run off the requesting thread", closedOnCaller.get())
            assertFalse(descriptors[0].fileDescriptor.valid())
        } finally { source.close(); descriptors.forEach { it.close() } }
    }
}

/** Real descriptor ownership with synthetic native capture and injectable acquisition failures. */
internal class PreRollAcquisitionFixture(blockRead: Boolean = false) : AndroidPreRollAudioSource.Acquisition, AutoCloseable {
    val pipe = android.os.ParcelFileDescriptor.createPipe().let { it[0] to it[1] }
    var failCapture = false
    var initialized = true
    var failStart = false
    @Volatile var failStop = false
    @Volatile var failRelease = false
    @Volatile var failReadClose = false
    @Volatile var failWriteClose = false
    var failOutput = false
    var writerFailure = ""
    var blockReadClose = false
    @Volatile var recording = false
    val starts = AtomicInteger(); val stops = AtomicInteger(); val releases = AtomicInteger()
    val reads = AtomicInteger(); val captureCalls = AtomicInteger(); val writerStarts = AtomicInteger()
    val readCloses = AtomicInteger()
    val reading = CountDownLatch(1); val allowRead = CountDownLatch(if (blockRead) 1 else 0)
    val closingRead = CountDownLatch(1); val allowReadClose = CountDownLatch(1)
    override fun pipe() = pipe
    override fun capture(): AndroidPreRollAudioSource.AcquiredCapture {
        captureCalls.incrementAndGet()
        if (failCapture) error("synthetic capture construction failure")
        return object : AndroidPreRollAudioSource.AcquiredCapture {
            override val initialized get() = this@PreRollAcquisitionFixture.initialized
            override val recording get() = this@PreRollAcquisitionFixture.recording
            override fun start() {
                starts.incrementAndGet(); this@PreRollAcquisitionFixture.recording = true
                if (failStart) error("synthetic start failure after activation")
            }
            override fun read(buffer: ByteArray): Int {
                reads.incrementAndGet(); reading.countDown(); check(allowRead.await(5, TimeUnit.SECONDS)); return 0
            }
            override fun stop() {
                stops.incrementAndGet(); if (failStop) error("synthetic stop failure")
                this@PreRollAcquisitionFixture.recording = false
            }
            override fun release() { releases.incrementAndGet(); if (failRelease) error("synthetic release failure") }
        }
    }
    override fun output(write: android.os.ParcelFileDescriptor): java.io.OutputStream {
        if (failOutput) error("synthetic output construction failure")
        return android.os.ParcelFileDescriptor.AutoCloseOutputStream(write)
    }
    override fun closeRead(read: android.os.ParcelFileDescriptor) {
        readCloses.incrementAndGet()
        closingRead.countDown()
        if (blockReadClose) check(allowReadClose.await(5, TimeUnit.SECONDS))
        if (failReadClose) error("synthetic read close failure")
        read.close()
    }
    override fun closeWrite(write: android.os.ParcelFileDescriptor) {
        if (failWriteClose) error("synthetic write close failure")
        write.close()
    }
    override fun startWriter(writer: Thread) {
        writerStarts.incrementAndGet()
        if (writerFailure == "before") error("synthetic failure before writer start")
        writer.start()
        if (writerFailure == "after") {
            check(reading.await(2, TimeUnit.SECONDS))
            error("synthetic failure after actual writer start")
        }
    }
    override fun close() { allowRead.countDown(); allowReadClose.countDown(); pipe.first.close(); pipe.second.close() }
}
