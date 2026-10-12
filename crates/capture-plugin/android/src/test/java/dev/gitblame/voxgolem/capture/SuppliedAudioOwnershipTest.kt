package dev.gitblame.voxgolem.capture

import android.Manifest
import android.speech.SpeechRecognizer
import android.os.Bundle
import android.os.Looper
import org.junit.Assert.*
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowSpeechRecognizer

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class SuppliedAudioOwnershipTest {
    private val context = RuntimeEnvironment.getApplication()

    @Before fun setUp() {
        shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(true)
    }

    @Test fun replacementRetiresNoLongerOwnedRecognizerBeforeDeferredCancellation() {
        val adapter = SuppliedAudioSpeechRecognizer(context)
        val previous = Ownership()
        try {
            adapter.start("synthetic-previous", CaptureFormat(), "en-US", previous)
            val oldRecognizer = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
            previous.current = false
            adapter.start("synthetic-next", CaptureFormat(), "en-US", Ownership())
            val nextRecognizer = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
            assertNotSame(oldRecognizer, nextRecognizer)
            assertTrue("Replacement retained the retired recognizer", shadowOf(oldRecognizer).isDestroyed)
            assertFalse(shadowOf(nextRecognizer).isDestroyed)
            adapter.cancel("synthetic-previous")
            assertFalse("Delayed old cancellation destroyed the replacement", shadowOf(nextRecognizer).isDestroyed)
        } finally {
            adapter.cancel("synthetic-previous")
            adapter.cancel("synthetic-next")
        }
    }

    @Test fun aCancelledStartCannotAllocateANativeRecognizer() {
        val adapter = SuppliedAudioSpeechRecognizer(context)
        val previous = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
        adapter.start("synthetic-cancelled", CaptureFormat(), "en-US", Ownership(false))
        assertSame(previous, ShadowSpeechRecognizer.getLatestSpeechRecognizer())
    }

    @Test fun failedDestroyRetainsTheRealSdkOwnerUntilAnExplicitRetryActuallyDestroysIt() {
        var attempts = 0
        val destroyed = mutableListOf<SpeechRecognizer>()
        val adapter = SuppliedAudioSpeechRecognizer(context) { sdk ->
            attempts++; destroyed += sdk
            if (attempts == 1) error("Synthetic one-time SDK destroy failure")
            sdk.destroy()
        }
        adapter.start("synthetic-retirement", CaptureFormat(), "en-US", Ownership())
        val sdk = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
        assertThrows(IllegalStateException::class.java) { adapter.cancel("synthetic-retirement") }
        assertFalse(shadowOf(sdk).isDestroyed)
        adapter.cancel("synthetic-retirement")
        assertEquals(2, attempts); assertEquals(listOf(sdk, sdk), destroyed)
        assertTrue(shadowOf(sdk).isDestroyed)
    }
    @Test fun callbacksFromFailedOrRetiredSdkCannotDestroySameIdReplacement() {
        var attempts = 0
        var hypotheses = 0
        val adapter = SuppliedAudioSpeechRecognizer(context) { sdk ->
            if (++attempts == 1) error("Synthetic one-time SDK destroy failure")
            sdk.destroy()
        }
        val events = object : SpeechRecognizerAdapter.Events {
            override fun onHypothesis(text: String, isFinal: Boolean) { hypotheses++ }
            override fun onFailure(code: String) { error("Synthetic stale callback escaped ownership") }
        }
        try {
            adapter.start("synthetic-reused-id", CaptureFormat(), "en-US", events)
            shadowOf(Looper.getMainLooper()).idle()
            val old = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
            assertThrows(IllegalStateException::class.java) { adapter.cancel("synthetic-reused-id") }
            shadowOf(old).triggerOnPartialResults(Bundle().apply {
                putStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION, arrayListOf("Synthetic stale partial"))
            })
            assertEquals(0, hypotheses)
            adapter.cancel("synthetic-reused-id")
            adapter.start("synthetic-reused-id", CaptureFormat(), "en-US", Ownership())
            shadowOf(Looper.getMainLooper()).idle()
            val next = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
            shadowOf(old).triggerOnError(SpeechRecognizer.ERROR_CLIENT)
            assertNotSame(old, next); assertFalse(shadowOf(next).isDestroyed)
        } finally { adapter.cancel("synthetic-reused-id") }
    }

    @Test fun suppliedAudioMutationsRequireTheExactSdkEventsOwnerEvenWhenSessionIdIsReused() {
        val adapter = SuppliedAudioSpeechRecognizer(context)
        val old = Ownership()
        val replacement = Ownership()
        try {
            adapter.start("synthetic-reused-input-id", CaptureFormat(), "en-US", old)
            old.current = false
            adapter.start("synthetic-reused-input-id", CaptureFormat(), "en-US", replacement)
            val sdk = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
            assertFalse(adapter.appendOwned("synthetic-reused-input-id", old, byteArrayOf(9, 0)))
            assertFalse(adapter.finishOwned("synthetic-reused-input-id", old))
            adapter.cancelOwned("synthetic-reused-input-id", old)
            assertFalse(shadowOf(sdk).isDestroyed)
            assertTrue(adapter.appendOwned("synthetic-reused-input-id", replacement, byteArrayOf(1, 0)))
            assertTrue(adapter.finishOwned("synthetic-reused-input-id", replacement))
        } finally { adapter.cancelOwned("synthetic-reused-input-id", replacement) }
    }

    private class Ownership(var current: Boolean = true) : SpeechRecognizerAdapter.Events {
        override fun isCurrent() = current
        override fun onHypothesis(text: String, isFinal: Boolean) = Unit
        override fun onFailure(code: String) = Unit
    }
}
