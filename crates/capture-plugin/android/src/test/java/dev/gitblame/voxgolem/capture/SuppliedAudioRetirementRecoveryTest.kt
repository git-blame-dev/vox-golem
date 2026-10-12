package dev.gitblame.voxgolem.capture

import android.Manifest
import android.net.Uri
import android.os.Bundle
import android.os.Looper
import android.speech.SpeechRecognizer
import dev.gitblame.voxgolem.assistant.recognition.*
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.annotation.LooperMode
import org.robolectric.shadows.ShadowSpeechRecognizer
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
@LooperMode(LooperMode.Mode.PAUSED)
class SuppliedAudioRetirementRecoveryTest {
    private val context = RuntimeEnvironment.getApplication()
    private val leases = RecognitionResourceLease()
    private var attempts = 0
    private var failuresRemaining = 1
    private val adapter = SuppliedAudioSpeechRecognizer(context) { sdk ->
        assertEquals(Looper.getMainLooper(), Looper.myLooper())
        assertTrue(leases.current()?.kind in setOf(RecognitionResourceLease.Kind.CAPTURE_LIVE, RecognitionResourceLease.Kind.CAPTURE_PROBE))
        val returned = CountDownLatch(1)
        val failure = AtomicReference<Throwable?>()
        Thread {
            try {
                assertNotNull(leases.current())
                assertFalse(coordinator.tryBeginRecognitionProbe(NEXT_SESSION) {})
            } catch (error: Throwable) { failure.set(error) }
            finally { returned.countDown() }
        }.start()
        assertTrue("SDK destroy held an admission monitor", returned.await(2, TimeUnit.SECONDS))
        failure.get()?.let { throw it }
        attempts++
        if (failuresRemaining-- > 0) error("Synthetic SDK destroy failure")
        sdk.destroy()
    }
    private val coordinator: PhoneCaptureCoordinator = PhoneCaptureCoordinator(context, adapter, { _, _, _ -> },
        { Uri.parse("content://synthetic-retirement/archive.wav") },
        { OrgExportResult(OrgState.EXPORTED, "synthetic-destination") },
        runtimeReady = { true }, recognitionLeases = leases)
    private fun begin(capture: String = FIRST_CAPTURE, session: String = FIRST_SESSION) {
        shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(true)
        coordinator.beginLive(capture, session, "synthetic-node", CaptureFormat(), "2026-01-02T03:04:05Z")
        shadowOf(Looper.getMainLooper()).idle()
    }

    @Test fun explicitRecoveryIsBoundedAndReleasesAdmissionOnlyAfterRealSdkDestroySucceeds() {
        failuresRemaining = 2
        try {
            begin(); val sdk = ShadowSpeechRecognizer.getLatestSpeechRecognizer(); val held = leases.current()!!
            shadowOf(sdk).triggerOnError(SpeechRecognizer.ERROR_CLIENT)
            assertEquals(1, attempts); assertSame(held, leases.current()); assertFalse(shadowOf(sdk).isDestroyed)
            assertFalse(coordinator.retryFailedRetirements())
            assertEquals(2, attempts); assertSame(held, leases.current()); assertFalse(shadowOf(sdk).isDestroyed)
            shadowOf(Looper.getMainLooper()).idle(); assertEquals(2, attempts)
            assertTrue(coordinator.retryFailedRetirements())
            assertEquals(3, attempts); assertTrue(shadowOf(sdk).isDestroyed); assertNull(leases.current())
            val chat = leases.tryAcquire(RecognitionResourceLease.Kind.CHAT)!!; chat.close()
            begin(NEXT_CAPTURE, NEXT_SESSION)
            val next = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
            assertNotSame(sdk, next)
            val replacement = leases.current()
            shadowOf(sdk).triggerOnResults(Bundle().apply {
                putStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION, arrayListOf("Synthetic stale final"))
            })
            assertSame(replacement, leases.current()); assertFalse(shadowOf(next).isDestroyed)
        } finally { closeCoordinator() }
    }
    @Test fun aVisibleChatStartRecoversFailedCaptureWithoutDependingOnCaptureApis() {
        var chatStarted = false
        val dictation = SharedDictationOwner(object : LocalRecognizerFactory {
            override fun available() = true
            override fun create() = object : LocalRecognizerSession {
                override fun checkLanguage(language: String, result: (Boolean) -> Unit) { result(true) }
                override fun start(language: String, events: LocalRecognizerEvents) { chatStarted = true }
                override fun stop() {}
                override fun cancel() {}
                override fun destroy() {}
            }
        }, { true }, leases)
        try {
            begin(); val sdk = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
            shadowOf(sdk).triggerOnError(SpeechRecognizer.ERROR_CLIENT)
            assertEquals(1, attempts); assertNotNull(leases.current()); assertFalse(shadowOf(sdk).isDestroyed)
            dictation.setForeground(true); dictation.start("en-US"); shadowOf(Looper.getMainLooper()).idle()
            assertTrue(chatStarted); assertEquals(2, attempts); assertTrue(shadowOf(sdk).isDestroyed)
            assertEquals(RecognitionResourceLease.Kind.CHAT, leases.current()?.kind)
        } finally {
            dictation.stop(); shadowOf(Looper.getMainLooper()).idle(); closeCoordinator()
        }
    }
    @Test fun restartingTheExistingRuntimeRetriesItsFailedOwnerWithoutAProcessRestart() {
        try {
            begin(); val sdk = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
            coordinator.onRuntimeStopped()
            assertEquals(1, attempts); assertNotNull(leases.current()); assertFalse(shadowOf(sdk).isDestroyed)
            coordinator.onRuntimeStarted()
            assertEquals(2, attempts); assertTrue(shadowOf(sdk).isDestroyed); assertNull(leases.current())
            assertTrue(coordinator.tryBeginRecognitionProbe(NEXT_SESSION) {})
            coordinator.endRecognitionProbe(NEXT_SESSION)
            assertNull(leases.current())
        } finally { closeCoordinator() }
    }
    @Test fun failedOneShotProbeRetirementHasAnExplicitRecoveryRouteWithoutReenteringItsFinishCallback() {
        shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(true)
        var finishCalls = 0
        try {
            assertTrue(coordinator.tryBeginRecognitionProbe(FIRST_SESSION) {
                if (finishCalls++ == 0) {
                    runCatching { adapter.cancel(FIRST_SESSION) }
                        .onSuccess { coordinator.endRecognitionProbe(FIRST_SESSION) }
                }
            })
            adapter.start(FIRST_SESSION, CaptureFormat(), "en-US", object : SpeechRecognizerAdapter.Events {
                override fun onHypothesis(text: String, isFinal: Boolean) {}
                override fun onFailure(code: String) {}
            })
            shadowOf(Looper.getMainLooper()).idle()
            val sdk = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
            coordinator.onRuntimeStopped()
            assertEquals(1, attempts); assertEquals(1, finishCalls)
            assertFalse(shadowOf(sdk).isDestroyed)
            assertEquals(RecognitionResourceLease.Kind.CAPTURE_PROBE, leases.current()?.kind)
            assertTrue(leases.retryFailedRetirement())
            assertEquals(2, attempts); assertEquals(1, finishCalls)
            assertTrue(shadowOf(sdk).isDestroyed); assertNull(leases.current())
            coordinator.onRuntimeStarted()
            val chat = leases.tryAcquire(RecognitionResourceLease.Kind.CHAT)!!; chat.close()
        } finally {
            failuresRemaining = 0; adapter.cancel(FIRST_SESSION); coordinator.endRecognitionProbe(FIRST_SESSION)
            closeCoordinator()
        }
    }
    private fun closeCoordinator() {
        failuresRemaining = 0
        assertTrue(coordinator.awaitIdle())
        coordinator.shutdownForTests()
    }
    companion object {
        private const val FIRST_CAPTURE = "123e4567-e89b-42d3-a456-426614174101"
        private const val FIRST_SESSION = "123e4567-e89b-42d3-a456-426614174102"
        private const val NEXT_CAPTURE = "123e4567-e89b-42d3-a456-426614174103"
        private const val NEXT_SESSION = "123e4567-e89b-42d3-a456-426614174104"
    }
}
