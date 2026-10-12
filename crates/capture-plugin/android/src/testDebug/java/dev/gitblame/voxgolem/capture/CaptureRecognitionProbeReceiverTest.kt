package dev.gitblame.voxgolem.capture

import org.junit.Assert.*
import org.junit.Test

class CaptureRecognitionProbeReceiverTest {
    @Test fun onlyTheWholeSyntheticPhraseMatches() {
        assertTrue(CaptureRecognitionProbeReceiver.expectedMatches("Alpha bravo charlie one two three."))
        assertTrue(CaptureRecognitionProbeReceiver.expectedMatches("ALFA, BRAVO CHARLIE 1 2 3"))
        assertTrue(CaptureRecognitionProbeReceiver.expectedMatches("alpha bravo charlie 123"))
        assertFalse(CaptureRecognitionProbeReceiver.expectedMatches("alpha bravo charlie"))
        assertFalse(CaptureRecognitionProbeReceiver.expectedMatches("extra alpha bravo charlie one two three"))
        assertFalse(CaptureRecognitionProbeReceiver.expectedMatches("alpha bravo charlie one two three extra"))
        assertFalse(CaptureRecognitionProbeReceiver.expectedMatches(""))
    }

    @Test fun providerErrorsBecomeNumbersWithoutEchoingProviderContent() {
        assertEquals(7, CaptureRecognitionProbeReceiver.providerErrorNumber("speech-recognizer-7"))
        assertEquals(-1, CaptureRecognitionProbeReceiver.providerErrorNumber("speech-recognizer-7-private-text"))
        assertEquals(-1, CaptureRecognitionProbeReceiver.providerErrorNumber("external-audio-rejected-SecurityException"))
        assertEquals(-1, CaptureRecognitionProbeReceiver.providerErrorNumber("speech-recognizer-999999999999999999999"))
    }
}
