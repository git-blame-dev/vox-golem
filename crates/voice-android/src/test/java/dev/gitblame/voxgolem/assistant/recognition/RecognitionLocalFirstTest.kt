package dev.gitblame.voxgolem.assistant.recognition

import android.content.ComponentName
import android.content.Context
import android.content.ContextWrapper
import android.content.Intent
import android.content.pm.ResolveInfo
import android.content.pm.ServiceInfo
import android.content.res.Resources
import android.os.Looper
import android.provider.Settings
import android.speech.RecognitionService
import android.speech.RecognitionSupport
import android.speech.SpeechRecognizer
import android.speech.RecognizerIntent
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.annotation.Implementation
import org.robolectric.annotation.Implements
import org.robolectric.annotation.LooperMode
import org.robolectric.shadows.ShadowSpeechRecognizer
import java.time.Duration

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [33], manifest = Config.NONE)
@LooperMode(LooperMode.Mode.PAUSED)
class RecognitionLocalFirstTest {
    private val google = ComponentName("com.google.android.tts", "synthetic.GoogleRecognitionService")
    private val dedicated = ComponentName("synthetic.aiai", "synthetic.OnDeviceRecognitionService")
    private val other = ComponentName("synthetic.other", "synthetic.ConfiguredRecognitionService")
    private fun drain() = shadowOf(Looper.getMainLooper()).idle()
    private fun installed(vararg languages: String) = RecognitionSupport.Builder()
        .setInstalledOnDeviceLanguages(languages.toList()).build()
    @Suppress("DEPRECATION")
    private fun register(component: ComponentName) {
        val resolution = ResolveInfo().apply { serviceInfo = ServiceInfo().apply {
            packageName = component.packageName; name = component.className
        } }
        shadowOf(RuntimeEnvironment.getApplication().packageManager)
            .addResolveInfoForIntent(Intent(RecognitionService.SERVICE_INTERFACE), resolution)
    }
    @Suppress("DEPRECATION")
    private fun context(onDevice: ComponentName = dedicated, configured: ComponentName = google): Context {
        val base = RuntimeEnvironment.getApplication()
        register(google); register(onDevice)
        if (configured != google && configured != onDevice) register(configured)
        Settings.Secure.putString(base.contentResolver, "voice_recognition_service", configured.flattenToString())
        val resources = object : Resources(base.assets, base.resources.displayMetrics, base.resources.configuration) {
            override fun getIdentifier(name: String?, defType: String?, defPackage: String?): Int =
                if (name == "config_defaultOnDeviceSpeechRecognitionService") 0x12345678
                else base.resources.getIdentifier(name, defType, defPackage)
            override fun getString(id: Int): String = if (id == 0x12345678) onDevice.flattenToString()
                else base.resources.getString(id)
        }
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(true)
        return object : ContextWrapper(base) { override fun getResources() = resources }
    }
    private class Events : LocalRecognizerEvents {
        val errors = mutableListOf<String>()
        override fun partial(text: String) {}
        override fun final(text: String) {}
        override fun error(code: String) { errors += code }
    }
    @Test fun googleInstalledLocalWinsWithoutAllocatingOtherCandidates() {
        val session = AndroidLocalRecognizerFactory(context()).create()
        val original = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
        val results = mutableListOf<Boolean>()
        session.checkLanguage("en-AU") { results += it }; drain()
        shadowOf(original).triggerSupportResult(installed("en-US", "en-AU")); drain()
        assertEquals(listOf(true), results); assertSame(original, ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        assertEquals(google.flattenToShortString(), session.route?.provider)
        assertEquals("en-AU", session.route?.selectedLanguage); assertTrue(session.route!!.localPreferred)
        session.destroy(); drain()
    }
    @Test fun permittedFallbackChecksGoogleThenDedicatedWhileStrictPolicyUsesOnlyDedicatedInstalledEnglish() {
        for (allowFallback in listOf(true, false)) {
            val session = AndroidLocalRecognizerFactory(context(), allowFallback).create()
            val first = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
            val results = mutableListOf<Boolean>()
            session.checkLanguage("en-AU") { results += it }; drain()
            val local = if (allowFallback) {
                assertEquals(google.flattenToShortString(), session.route?.provider)
                assertFalse(session.route!!.onDeviceOnly)
                shadowOf(first).triggerSupportResult(installed()); drain()
                assertTrue(results.isEmpty())
                val replacement = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
                assertNotSame(first, replacement); assertTrue(shadowOf(first).isDestroyed)
                shadowOf(first).triggerSupportError(SpeechRecognizer.ERROR_CANNOT_CHECK_SUPPORT)
                shadowOf(first).triggerSupportResult(installed("en-AU")); drain()
                assertTrue(results.isEmpty())
                replacement
            } else {
                assertEquals(dedicated.flattenToShortString(), session.route?.provider)
                assertTrue(session.route!!.onDeviceOnly)
                assertSame(first, ShadowSpeechRecognizer.getLatestSpeechRecognizer())
                assertTrue(results.isEmpty())
                first
            }
            shadowOf(local).triggerSupportResult(installed("en-US", "en-GB")); drain()
            assertEquals(listOf(true), results)
            assertEquals(dedicated.flattenToShortString(), session.route?.provider)
            assertEquals("en-AU", session.route?.requestedLanguage)
            assertEquals("en-US", session.route?.selectedLanguage); assertTrue(session.route!!.localPreferred)
            assertTrue(session.route!!.onDeviceOnly)
            val events = Events(); session.start("en-AU", events); drain()
            assertEquals("en-US", shadowOf(local).lastRecognizerIntent.getStringExtra(RecognizerIntent.EXTRA_LANGUAGE))
            assertTrue(shadowOf(local).lastRecognizerIntent.getBooleanExtra(RecognizerIntent.EXTRA_PREFER_OFFLINE, false))
            assertThrows(IllegalStateException::class.java) { session.checkLanguage("en-AU") {} }
            shadowOf(local).triggerOnError(SpeechRecognizer.ERROR_LANGUAGE_UNAVAILABLE); drain()
            assertEquals(listOf("recognition_error_13"), events.errors)
            assertSame(local, ShadowSpeechRecognizer.getLatestSpeechRecognizer())
            if (allowFallback) assertNull(shadowOf(first).lastRecognizerIntent)
            assertNull(shadowOf(first).latestModelDownloadIntent); assertNull(shadowOf(local).latestModelDownloadIntent)
            session.destroy(); drain()
        }
    }
    @Test fun allLocalModelsAbsentRecreatesGoogleOnlyForVisibleProviderControlledAdmission() {
        val session = AndroidLocalRecognizerFactory(context()).create()
        val first = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
        val results = mutableListOf<Boolean>()
        session.checkLanguage("en-AU") { results += it }; drain()
        shadowOf(first).triggerSupportResult(installed()); drain()
        assertTrue(results.isEmpty())
        val local = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
        shadowOf(local).triggerSupportResult(installed()); drain()
        val fallback = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
        assertNotSame(local, fallback); assertNotSame(first, fallback)
        assertTrue(shadowOf(first).isDestroyed); assertTrue(shadowOf(local).isDestroyed)
        assertEquals(listOf(true), results)
        assertEquals(google.flattenToShortString(), session.route?.provider)
        assertFalse(session.route!!.localPreferred); assertNull(session.route?.selectedLanguage)
        assertNull(session.route?.reportedLanguage); assertNotNull(session.route?.fallbackReason)
        val route = session.route
        shadowOf(local).triggerSupportResult(installed("en-AU"))
        shadowOf(first).triggerSupportResult(installed("en-AU")); drain()
        assertEquals(route, session.route); assertEquals(listOf(true), results)
        session.start("en-AU", Events()); drain()
        assertFalse(shadowOf(fallback).lastRecognizerIntent.getBooleanExtra(RecognizerIntent.EXTRA_PREFER_OFFLINE, true))
        assertNull(shadowOf(local).lastRecognizerIntent); assertNull(shadowOf(first).lastRecognizerIntent)
        assertNull(shadowOf(fallback).latestModelDownloadIntent)
        session.destroy(); drain()
    }
    @Test fun strictPolicyRejectsMissingDedicatedLanguageWithoutCreatingOrdinaryProvider() {
        val session = AndroidLocalRecognizerFactory(context(configured = other), false).create()
        val first = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
        val results = mutableListOf<Boolean>()
        session.checkLanguage("en-AU") { results += it }; drain()
        assertEquals(dedicated.flattenToShortString(), session.route?.provider)
        assertTrue(session.route!!.onDeviceOnly)
        shadowOf(first).triggerSupportResult(installed()); drain()
        assertEquals(listOf(false), results)
        assertSame(first, ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        assertTrue(session.route!!.localPreferred); assertNull(session.route?.selectedLanguage)
        shadowOf(first).triggerSupportResult(installed("en-AU")); drain()
        assertEquals(listOf(false), results); assertNull(session.route?.selectedLanguage)
        assertThrows(IllegalStateException::class.java) { session.start("en-AU", Events()) }
        assertNull(shadowOf(first).lastRecognizerIntent); assertNull(shadowOf(first).latestModelDownloadIntent)
        session.destroy(); drain()
    }
    @Test fun configuredThirdCandidateGetsInstalledLocalCheckBeforePreferredGoogleCloud() {
        val session = AndroidLocalRecognizerFactory(context(configured = other)).create()
        val first = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
        val results = mutableListOf<Boolean>()
        session.checkLanguage("en-AU") { results += it }; drain()
        shadowOf(first).triggerSupportResult(installed()); drain()
        assertTrue(results.isEmpty())
        val local = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
        shadowOf(local).triggerSupportResult(installed()); drain()
        assertTrue(results.isEmpty())
        val configured = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
        assertNotSame(local, configured); assertTrue(shadowOf(local).isDestroyed)
        shadowOf(configured).triggerSupportResult(installed("en-US", "en-GB")); drain()
        assertEquals(listOf(true), results); assertEquals(other.flattenToShortString(), session.route?.provider)
        assertEquals("en-US", session.route?.selectedLanguage); assertTrue(session.route!!.localPreferred)
        session.destroy(); drain()
    }
    @Test fun watchWithoutDedicatedLocalCandidateReusesGoogleForProviderControlledMode() {
        val context = context(); ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(false)
        val session = AndroidLocalRecognizerFactory(context).create()
        val first = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
        val results = mutableListOf<Boolean>()
        session.checkLanguage("en-AU") { results += it }; drain()
        shadowOf(first).triggerSupportResult(installed()); drain()
        assertEquals(listOf(true), results); assertSame(first, ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        assertFalse(shadowOf(first).isDestroyed); assertFalse(session.route!!.localPreferred)
        session.destroy(); drain()
    }
    @Test fun sameGoogleDedicatedComponentIsNotCheckedTwiceOrRebound() {
        val session = AndroidLocalRecognizerFactory(context(onDevice = google)).create()
        val first = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
        val results = mutableListOf<Boolean>()
        session.checkLanguage("en-AU") { results += it }; drain()
        shadowOf(first).triggerSupportResult(installed()); drain()
        assertEquals(listOf(true), results); assertSame(first, ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        assertFalse(shadowOf(first).isDestroyed); assertFalse(session.route!!.localPreferred)
        session.destroy(); drain()
    }
    @Test fun silentLocalCandidatesHaveOneBoundedWindowEachBeforeCloudAndCancellationFencesReplacement() {
        val session = AndroidLocalRecognizerFactory(context()).create()
        val first = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
        val results = mutableListOf<Boolean>()
        session.checkLanguage("en-AU") { results += it }; drain()
        shadowOf(first).triggerSupportError(SpeechRecognizer.ERROR_CANNOT_CHECK_SUPPORT); drain()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(3))
        assertTrue(results.isEmpty())
        val local = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
        assertNotSame(first, local); assertTrue(shadowOf(first).isDestroyed)
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofMillis(2_999))
        assertTrue(results.isEmpty())
        session.cancel()
        shadowOf(local).triggerSupportResult(installed("en-AU")); drain()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(10))
        assertTrue(results.isEmpty()); assertSame(local, ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        session.destroy(); drain()
    }
    @Test fun threeSilentLocalCandidatesExhaustOnceWithinNineSecondsBeforePreferredProviderFallback() {
        val session = AndroidLocalRecognizerFactory(context(configured = other)).create()
        val first = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
        val results = mutableListOf<Boolean>()
        session.checkLanguage("en-AU") { results += it }; drain()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(3))
        val local = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
        assertNotSame(first, local); assertTrue(results.isEmpty())
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(3))
        val configured = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
        assertNotSame(local, configured); assertTrue(results.isEmpty())
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofMillis(2_999))
        assertTrue(results.isEmpty())
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofMillis(1))
        val preferred = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
        assertNotSame(configured, preferred); assertEquals(listOf(true), results)
        assertEquals(google.flattenToShortString(), session.route?.provider)
        assertFalse(session.route!!.localPreferred); assertNull(session.route?.selectedLanguage)
        for (retired in listOf(first, local, configured)) {
            assertTrue(shadowOf(retired).isDestroyed)
            assertNull(shadowOf(retired).lastRecognizerIntent); assertNull(shadowOf(retired).latestModelDownloadIntent)
        }
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(20))
        assertEquals(listOf(true), results); assertSame(preferred, ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        session.destroy(); drain()
    }
    @Test @Config(sdk = [33], shadows = [FailingRecognizerRetirementShadow::class])
    fun failedGoogleRetirementHoldsLeaseAndCannotAllocateOrAdmitDedicatedReplacement() {
        val leases = RecognitionResourceLease()
        val owner = SharedDictationOwner(AndroidLocalRecognizerFactory(context()), { true }, leases)
        owner.setForeground(true); owner.start("en-AU"); drain()
        val first = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
        FailingRecognizerRetirementShadow.rejectDestroy = true
        try {
            shadowOf(first).triggerSupportResult(installed()); drain()
            assertSame(first, ShadowSpeechRecognizer.getLatestSpeechRecognizer())
            assertEquals("recognition_cleanup_failed", owner.snapshot().error)
            assertNotNull(leases.current()); assertNull(shadowOf(first).lastRecognizerIntent)
            shadowOf(first).triggerSupportResult(installed("en-AU")); drain()
            assertSame(first, ShadowSpeechRecognizer.getLatestSpeechRecognizer())
            assertNull(shadowOf(first).lastRecognizerIntent)
        } finally {
            FailingRecognizerRetirementShadow.rejectDestroy = false
            owner.stop(); drain()
        }
        assertTrue(shadowOf(first).isDestroyed); assertNull(leases.current())
    }
    @Test @Config(sdk = [33], shadows = [FailingRecognizerRetirementShadow::class])
    fun failedDedicatedRetirementCannotRecreateGoogleCloudAndKeepsRecoverableDraft() {
        val leases = RecognitionResourceLease()
        val owner = SharedDictationOwner(AndroidLocalRecognizerFactory(context()), { true }, leases)
        owner.setForeground(true); owner.start("en-AU", "Synthetic recoverable draft"); drain()
        val first = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
        shadowOf(first).triggerSupportResult(installed()); drain()
        val local = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
        assertNotSame(first, local); assertTrue(shadowOf(first).isDestroyed)
        FailingRecognizerRetirementShadow.rejectDestroy = true
        try {
            shadowOf(local).triggerSupportResult(installed()); drain()
            assertSame(local, ShadowSpeechRecognizer.getLatestSpeechRecognizer())
            assertEquals("recognition_cleanup_failed", owner.snapshot().error)
            assertEquals("Synthetic recoverable draft", owner.snapshot().draft)
            assertEquals(dedicated.flattenToShortString(), owner.snapshot().route?.provider)
            assertNotNull(leases.current()); assertNull(shadowOf(local).lastRecognizerIntent)
            shadowOf(local).triggerSupportResult(installed("en-AU")); drain()
            assertSame(local, ShadowSpeechRecognizer.getLatestSpeechRecognizer())
            assertNull(shadowOf(local).lastRecognizerIntent)
        } finally {
            FailingRecognizerRetirementShadow.rejectDestroy = false
            owner.stop(); drain()
        }
        assertTrue(shadowOf(local).isDestroyed); assertNull(leases.current())
    }
}

@Implements(SpeechRecognizer::class)
class FailingRecognizerRetirementShadow : ShadowSpeechRecognizer() {
    @Implementation
    override fun destroy() {
        if (rejectDestroy) throw IllegalStateException("Synthetic recognition retirement failure")
        super.destroy()
    }
    companion object { var rejectDestroy = false }
}
