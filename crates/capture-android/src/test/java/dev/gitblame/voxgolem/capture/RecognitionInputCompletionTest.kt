package dev.gitblame.voxgolem.capture

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.assertThrows
import org.junit.Test
import java.io.IOException
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

class RecognitionInputCompletionTest {
    @Test
    fun queuedAudioCannotProduceAFinalUntilDrainAndEofSucceed() {
        val completion = RecognitionInputCompletion()
        assertFalse(completion.canAcceptFinal())
        completion.closeAfterDrain { }
        assertTrue(completion.canAcceptFinal())
    }

    @Test
    fun failedDrainNeverMakesATerminalResultAcceptable() {
        val completion = RecognitionInputCompletion()
        assertThrows(IOException::class.java) { completion.closeAfterDrain { throw IOException("synthetic pipe failure") } }
        assertFalse(completion.canAcceptFinal())
    }

    @Test
    fun terminalCallbackCannotRaceAheadOfPipeClosure() {
        val completion = RecognitionInputCompletion()
        val enteredClose = CountDownLatch(1)
        val allowClose = CountDownLatch(1)
        val executor = Executors.newFixedThreadPool(2)
        try {
            val writer = executor.submit { completion.closeAfterDrain {
                enteredClose.countDown()
                check(allowClose.await(2, TimeUnit.SECONDS))
            } }
            assertTrue(enteredClose.await(2, TimeUnit.SECONDS))
            val terminalEntered = CountDownLatch(1)
            val terminal = executor.submit<Boolean> { terminalEntered.countDown(); completion.canAcceptFinal() }
            assertTrue(terminalEntered.await(2, TimeUnit.SECONDS))
            assertThrows(java.util.concurrent.TimeoutException::class.java) { terminal.get(100, TimeUnit.MILLISECONDS) }
            allowClose.countDown()
            writer.get(2, TimeUnit.SECONDS)
            assertTrue(terminal.get(2, TimeUnit.SECONDS))
        } finally {
            allowClose.countDown()
            executor.shutdownNow()
        }
    }
}
