package dev.gitblame.voxgolem.assistant.inputtransport

import org.junit.Assert.*
import org.junit.Test

class VoiceAudioContractTest {
    @Test fun localeIsValidatedWithoutChangingTheRequestedTag() {
        for (language in listOf("en-US", "en-AU", "zh-Hant-TW")) assertEquals(language, VoiceAudioContract.language(language))
        for (invalid in listOf("und", "en--AU", "en_US", "en-us", "", "x".repeat(65))) {
            assertThrows(IllegalArgumentException::class.java) { VoiceAudioContract.language(invalid) }
        }
    }
    @Test fun boundedQueueRejectsOverflowAndClonesAudio() {
        val queue = BoundedPcmQueue(2)
        val bytes = ByteArray(640) { 1 }
        assertTrue(queue.offer(bytes))
        bytes[0] = 2
        assertTrue(queue.offer(bytes))
        assertFalse(queue.offer(bytes))
        assertEquals(1.toByte(), queue.poll(1)!![0])
        queue.cancel()
        assertFalse(queue.offer(bytes))
        assertNull(queue.poll(1))
    }
    @Test fun finishedQueueDrainsButNeverAcceptsMoreOrMalformedPcm() {
        val queue = BoundedPcmQueue(1)
        for (bytes in listOf(ByteArray(0), ByteArray(3), ByteArray(642))) {
            assertThrows(IllegalArgumentException::class.java) { queue.offer(bytes) }
        }
        assertTrue(queue.offer(ByteArray(640)))
        queue.finish()
        assertFalse(queue.offer(ByteArray(2)))
        assertFalse(queue.drained())
        assertNotNull(queue.poll(1))
        assertTrue(queue.drained())
    }
}
