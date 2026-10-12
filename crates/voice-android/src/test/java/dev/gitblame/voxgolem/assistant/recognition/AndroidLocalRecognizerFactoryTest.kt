package dev.gitblame.voxgolem.assistant.recognition

import android.os.Bundle
import android.os.Looper
import android.content.Intent
import android.content.ComponentName
import android.content.pm.ResolveInfo
import android.content.pm.ServiceInfo
import android.content.pm.ApplicationInfo
import android.speech.RecognitionService
import android.speech.RecognitionSupport
import android.speech.RecognizerIntent
import android.speech.SpeechRecognizer
import dev.gitblame.voxgolem.assistant.AssistantTiming
import dev.gitblame.voxgolem.assistant.AssistantTiming.Kind
import dev.gitblame.voxgolem.assistant.AssistantTiming.Operation
import dev.gitblame.voxgolem.assistant.AssistantTiming.Stage
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.annotation.LooperMode
import org.robolectric.shadows.ShadowSpeechRecognizer
import org.robolectric.shadows.ShadowLog
import org.robolectric.util.ReflectionHelpers
import java.time.Duration

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
@LooperMode(LooperMode.Mode.PAUSED)
class AndroidLocalRecognizerFactoryTest {
    @Test fun setupErrorOnRecognitionListenerSettlesSupportWithCauseWithoutWaitingForDeadline() {
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(true)
        val session = AndroidLocalRecognizerFactory(RuntimeEnvironment.getApplication(), false).create()
        val results = mutableListOf<Boolean>()
        session.checkLanguage("en-AU") { results += it }; drain()
        val sdk = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        sdk.triggerOnError(SpeechRecognizer.ERROR_CLIENT); drain()
        assertEquals(listOf(false), results)
        assertEquals("support_error_5", session.route?.fallbackReason)
        assertNull(sdk.lastRecognizerIntent); assertNull(sdk.latestModelDownloadIntent)
        sdk.triggerSupportResult(RecognitionSupport.Builder().setInstalledOnDeviceLanguages(listOf("en-AU")).build()); drain()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(4))
        assertEquals(listOf(false), results)
        session.destroy(); drain()
    }

    @Test fun recognitionListenerError14KeepsSupportGraceAndCancellationFence() {
        for (cancel in listOf(false, true)) {
            ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(true)
            val session = AndroidLocalRecognizerFactory(RuntimeEnvironment.getApplication(), false).create()
            val results = mutableListOf<Boolean>()
            session.checkLanguage("en-AU") { results += it }; drain()
            val sdk = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
            sdk.triggerOnError(SpeechRecognizer.ERROR_CANNOT_CHECK_SUPPORT); drain()
            shadowOf(Looper.getMainLooper()).idleFor(Duration.ofMillis(100))
            assertTrue(results.isEmpty())
            if (cancel) session.cancel()
            sdk.triggerSupportResult(RecognitionSupport.Builder().setInstalledOnDeviceLanguages(listOf("en-AU")).build()); drain()
            sdk.triggerOnError(SpeechRecognizer.ERROR_CLIENT); drain()
            shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(4))
            assertEquals(if (cancel) emptyList<Boolean>() else listOf(true), results)
            assertNull(sdk.lastRecognizerIntent); assertNull(sdk.latestModelDownloadIntent)
            session.destroy(); drain()
        }
    }

    @Test fun retiredCandidateSetupErrorCannotFailReplacementSupportCheck() {
        registerService("synthetic.other.provider", "synthetic.OtherRecognitionService")
        android.provider.Settings.Secure.putString(RuntimeEnvironment.getApplication().contentResolver,
            "voice_recognition_service", "synthetic.other.provider/synthetic.OtherRecognitionService")
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(true)
        val session = AndroidLocalRecognizerFactory(RuntimeEnvironment.getApplication()).create()
        val results = mutableListOf<Boolean>()
        session.checkLanguage("en-AU") { results += it }; drain()
        val dedicatedSdk = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
        val dedicated = shadowOf(dedicatedSdk)
        dedicated.triggerSupportResult(RecognitionSupport.Builder().build()); drain()
        val configuredSdk = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
        val configured = shadowOf(configuredSdk)
        assertNotSame(dedicatedSdk, configuredSdk); assertTrue(dedicated.isDestroyed)
        dedicated.triggerOnError(SpeechRecognizer.ERROR_CLIENT); drain()
        assertTrue(results.isEmpty())
        configured.triggerSupportResult(RecognitionSupport.Builder().setInstalledOnDeviceLanguages(listOf("en-AU")).build()); drain()
        assertEquals(listOf(true), results)
        assertSame(configuredSdk, ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        assertNull(dedicated.lastRecognizerIntent); assertNull(configured.lastRecognizerIntent)
        session.destroy(); drain()
    }

    @Test fun explicitAustralianLanguageUsesInstalledAuMatchAndCapturedDedicatedOnlyPolicy() {
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(true)
        var configuration = dev.gitblame.voxgolem.assistant.speech.SpeechAdmission(
            dev.gitblame.voxgolem.assistant.speech.SpeechSelection(language = "en-AU"), false)
        val owner = SharedDictationOwner(AndroidLocalRecognizerFactory(RuntimeEnvironment.getApplication()), { true },
            RecognitionResourceLease(), configuration = { configuration })
        owner.setForeground(true); owner.start("en-US"); drain()
        configuration = configuration.copy(selection = configuration.selection.copy(language = "en-US"), networkFallback = true)
        val sdk = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        sdk.triggerSupportResult(RecognitionSupport.Builder().setInstalledOnDeviceLanguages(listOf("en-US", "en-AU")).build()); drain()
        assertEquals("en-AU", sdk.lastRecognizerIntent.getStringExtra(RecognizerIntent.EXTRA_LANGUAGE))
        assertEquals("en-AU", owner.snapshot().route!!.requestedLanguage)
        assertEquals("en-AU", owner.snapshot().route!!.selectedLanguage)
        assertTrue(owner.snapshot().route!!.onDeviceOnly)
        assertEquals(listOf("en-US", "en-AU"), owner.snapshot().route!!.installedLanguages)
        owner.close(); drain()
    }
    @Test fun explicitLanguageMissingFromCurrentSdkSupportNeverStartsEvenWithFallbackPermitted() {
        registerGoogle(); ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(false)
        val owner = SharedDictationOwner(AndroidLocalRecognizerFactory(RuntimeEnvironment.getApplication()), { true },
            RecognitionResourceLease(), configuration = {
                dev.gitblame.voxgolem.assistant.speech.SpeechAdmission(dev.gitblame.voxgolem.assistant.speech.SpeechSelection(language = "en-AU"), true)
            })
        owner.setForeground(true); owner.start("en-US"); drain()
        val sdk = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        sdk.triggerSupportResult(RecognitionSupport.Builder().setOnlineLanguages(listOf("de-DE")).build()); drain()
        assertNull(sdk.lastRecognizerIntent)
        assertEquals("local_language_unavailable", owner.snapshot().error)
        assertTrue(sdk.isDestroyed)
        assertNull(sdk.latestModelDownloadIntent)
        owner.close(); drain()
    }
    private fun drain() = shadowOf(Looper.getMainLooper()).idle()
    private fun owner(allowFallback: Boolean = true, onDeviceAvailable: Boolean = true): SharedDictationOwner {
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(onDeviceAvailable)
        return SharedDictationOwner(AndroidLocalRecognizerFactory(RuntimeEnvironment.getApplication(), allowFallback), { true }, RecognitionResourceLease())
    }
    private fun timedOwner(events: MutableList<AssistantTiming.Event>, allowFallback: Boolean = true,
        clock: () -> Long = { 0L }): SharedDictationOwner {
        val context = RuntimeEnvironment.getApplication()
        context.applicationInfo.flags = context.applicationInfo.flags or ApplicationInfo.FLAG_DEBUGGABLE
        return SharedDictationOwner(AndroidLocalRecognizerFactory(context, allowFallback), { true }, RecognitionResourceLease(),
            beginTiming = { AssistantTiming.begin(context, Operation.RECOGNITION, clock, AssistantTiming.Sink { events += it }) })
    }

    @Test fun recognitionTimingSeparatesActualReadyAndFinalCallbacksFromMainDeliveryAndCoalescesNonblankPartials() {
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(true)
        var now = 1_000L
        val events = mutableListOf<AssistantTiming.Event>()
        val owner = timedOwner(events, clock = { now })
        owner.setForeground(true); owner.start("en-AU"); drain()
        val sdk = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        val rootRun = events.first().run
        sdk.triggerSupportResult(RecognitionSupport.Builder().setInstalledOnDeviceLanguages(listOf("en-AU")).build()); drain()
        now += 20; sdk.triggerOnReadyForSpeech(Bundle())
        assertEquals(20L, events.single { it.run == rootRun && it.kind == Kind.MARK && it.stage == Stage.READY_CALLBACK }.elapsedNanos)
        assertFalse(events.any { it.stage == Stage.READY_MAIN_DELIVERED })
        now += 30; drain()
        assertEquals(50L, events.single { it.run == rootRun && it.kind == Kind.MARK && it.stage == Stage.READY_MAIN_DELIVERED }.elapsedNanos)
        sdk.triggerOnPartialResults(result("   ")); drain()
        assertFalse(events.any { it.stage == Stage.FIRST_PARTIAL })
        sdk.triggerOnPartialResults(result("Synthetic private partial")); sdk.triggerOnPartialResults(result("Synthetic later partial")); drain()
        assertEquals(1, events.count { it.kind == Kind.MARK && it.stage == Stage.FIRST_PARTIAL })
        sdk.triggerOnEndOfSpeech(); drain()
        assertTrue(events.any { it.run == rootRun && it.stage == Stage.END_OF_SPEECH })
        assertEquals(DictationPhase.LISTENING, owner.snapshot().phase)
        assertFalse(sdk.isDestroyed)
        owner.endUtterance(); drain()
        assertTrue(events.any { it.run == rootRun && it.stage == Stage.FINISH_REQUESTED })
        now += 40; sdk.triggerOnResults(result("Synthetic private authoritative final"))
        assertEquals(90L, events.single { it.run == rootRun && it.kind == Kind.MARK && it.stage == Stage.FINAL_CALLBACK }.elapsedNanos)
        assertFalse(events.any { it.stage == Stage.FINAL_MAIN_DELIVERED })
        now += 10; drain()
        assertEquals(100L, events.single { it.run == rootRun && it.kind == Kind.MARK && it.stage == Stage.FINAL_MAIN_DELIVERED }.elapsedNanos)
        assertEquals(1, events.count { it.run == rootRun && it.kind == Kind.END })
        assertEquals(Stage.SETTLED, events.single { it.run == rootRun && it.kind == Kind.END }.stage)
        assertTrue(sdk.isDestroyed); assertEquals(DictationPhase.FINAL, owner.snapshot().phase)
        val afterFinal = events.toList()
        sdk.triggerOnReadyForSpeech(Bundle()); sdk.triggerOnEndOfSpeech(); sdk.triggerOnResults(result("Synthetic obsolete final")); drain()
        assertEquals(afterFinal, events)
        assertFalse(events.toString().contains("Synthetic private")); assertFalse(events.toString().contains("en-AU"))
        owner.close(); drain()
    }

    @Test fun supportTimingPreservesError14GraceAndSettlesInstalledReplyExactlyOnce() {
        registerGoogle(); ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(false)
        val events = mutableListOf<AssistantTiming.Event>()
        val owner = timedOwner(events)
        owner.setForeground(true); owner.start("en-AU"); drain()
        val sdk = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        val supportRun = events.single { it.operation == Operation.SUPPORT_CHECK && it.kind == Kind.BEGIN }.run
        sdk.triggerSupportError(SpeechRecognizer.ERROR_CANNOT_CHECK_SUPPORT); drain()
        sdk.triggerSupportResult(RecognitionSupport.Builder().setOnlineLanguages(listOf("en-AU")).build()); drain()
        assertFalse(events.any { it.run == supportRun && it.kind == Kind.END })
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofMillis(100))
        sdk.triggerSupportResult(RecognitionSupport.Builder().setInstalledOnDeviceLanguages(listOf("en-AU")).build())
        sdk.triggerSupportResult(RecognitionSupport.Builder().setInstalledOnDeviceLanguages(listOf("en-US")).build()); drain()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(4))
        assertEquals(Stage.SUPPORTED, events.single { it.run == supportRun && it.kind == Kind.END }.stage)
        assertEquals(1, events.count { it.run == supportRun && it.kind == Kind.MARK && it.stage == Stage.SUPPORT_CALLBACK })
        assertFalse(events.any { it.run == supportRun && it.stage == Stage.TIMED_OUT })
        assertEquals("en-AU", owner.snapshot().route?.selectedLanguage)
        assertTrue(owner.snapshot().route!!.localPreferred); assertNull(sdk.latestModelDownloadIntent)
        owner.close(); drain()
    }

    @Test fun supportCandidateSpansStayWithTheirOwnSdkAndLateReplyCannotAttachToReplacement() {
        registerService("synthetic.other.provider", "synthetic.OtherRecognitionService")
        android.provider.Settings.Secure.putString(RuntimeEnvironment.getApplication().contentResolver,
            "voice_recognition_service", "synthetic.other.provider/synthetic.OtherRecognitionService")
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(true)
        val events = mutableListOf<AssistantTiming.Event>()
        val owner = timedOwner(events)
        owner.setForeground(true); owner.start("en-AU"); drain()
        val dedicated = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        dedicated.triggerSupportResult(RecognitionSupport.Builder().build()); drain()
        val configured = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        val children = events.filter { it.operation == Operation.SUPPORT_CHECK && it.kind == Kind.BEGIN }
        assertEquals(2, children.size); assertNotEquals(children[0].run, children[1].run)
        assertTrue(children.all { it.parentRun == events.first().run })
        assertTrue(children.all { child -> events.count { it.run == child.run && it.kind == Kind.MARK && it.stage == Stage.SDK_CREATED } == 1 })
        assertEquals(Stage.UNSUPPORTED, events.single { it.run == children[0].run && it.kind == Kind.END }.stage)
        val beforeStale = events.toList()
        dedicated.triggerSupportResult(RecognitionSupport.Builder().setInstalledOnDeviceLanguages(listOf("en-AU")).build()); drain()
        assertEquals(beforeStale, events)
        configured.triggerSupportResult(RecognitionSupport.Builder().setInstalledOnDeviceLanguages(listOf("en-AU")).build()); drain()
        assertEquals(Stage.SUPPORTED, events.single { it.run == children[1].run && it.kind == Kind.END }.stage)
        assertTrue(dedicated.isDestroyed); assertTrue(owner.snapshot().route!!.localPreferred)
        owner.close(); drain()
        val afterClose = events.toList()
        configured.triggerOnReadyForSpeech(Bundle()); configured.triggerOnResults(result("Synthetic obsolete final")); drain()
        assertEquals(afterClose, events)
    }

    @Test fun supportTimeoutAndCancellationCloseTheirExactCandidateWithoutExtendingExistingDeadline() {
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(true)
        for (cancel in listOf(false, true)) {
            val events = mutableListOf<AssistantTiming.Event>()
            val owner = timedOwner(events, allowFallback = false)
            owner.setForeground(true); owner.start("en-AU"); drain()
            val sdk = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
            val supportRun = events.single { it.operation == Operation.SUPPORT_CHECK && it.kind == Kind.BEGIN }.run
            sdk.triggerSupportError(SpeechRecognizer.ERROR_CANNOT_CHECK_SUPPORT); drain()
            shadowOf(Looper.getMainLooper()).idleFor(Duration.ofMillis(2_900))
            sdk.triggerSupportError(SpeechRecognizer.ERROR_CANNOT_CHECK_SUPPORT); drain()
            assertFalse(events.any { it.run == supportRun && it.kind == Kind.END })
            if (cancel) { owner.stop(); drain() }
            shadowOf(Looper.getMainLooper()).idleFor(Duration.ofMillis(100))
            assertEquals(if (cancel) Stage.CANCELLED else Stage.UNSUPPORTED,
                events.single { it.run == supportRun && it.kind == Kind.END }.stage)
            assertEquals(!cancel, events.any { it.run == supportRun && it.stage == Stage.TIMED_OUT })
            assertNull(sdk.lastRecognizerIntent); assertNull(sdk.latestModelDownloadIntent)
            val beforeStale = events.toList()
            sdk.triggerSupportResult(RecognitionSupport.Builder().setInstalledOnDeviceLanguages(listOf("en-AU")).build()); drain()
            assertEquals(beforeStale, events)
            owner.close(); drain()
        }
    }

    @Test fun contextConstructorUsesOptInTimingAndEmitsNoTranscriptLocaleRouteOrRawError() {
        val context = RuntimeEnvironment.getApplication()
        context.applicationInfo.flags = context.applicationInfo.flags or ApplicationInfo.FLAG_DEBUGGABLE
        shadowOf(context).grantPermissions(android.Manifest.permission.RECORD_AUDIO)
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(true); ShadowLog.clear()
        val owner = SharedDictationOwner(context)
        owner.setForeground(true); owner.start("en-AU", "Synthetic private retained draft"); drain()
        val sdk = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        sdk.triggerSupportResult(RecognitionSupport.Builder().setInstalledOnDeviceLanguages(listOf("en-AU")).build()); drain()
        sdk.triggerOnReadyForSpeech(Bundle()); sdk.triggerOnPartialResults(result("Synthetic private partial")); drain()
        sdk.triggerOnError(SpeechRecognizer.ERROR_INSUFFICIENT_PERMISSIONS); drain()
        val messages = ShadowLog.getLogsForTag(AssistantTiming.TAG).map { it.msg }
        assertTrue(messages.isNotEmpty())
        assertTrue(messages.all { it.matches(Regex("operation=(RECOGNITION|SUPPORT_CHECK) run=[0-9]+( parent=[0-9]+)? stage=[A-Z_]+ elapsed_ms=[0-9]+ observed_ms=[0-9]+")) })
        for (privateValue in listOf("Synthetic private", "en-AU", "android.on-device", "recognition_error_9")) {
            assertFalse(messages.any { it.contains(privateValue) })
        }
        assertEquals("recognition_error_9", owner.snapshot().error)
        owner.close(); drain()
    }
    @Test fun sdkReadyForSpeechIsDistinctFromStartRequestAndSuppressedAfterStop() {
        val owner = owner(); owner.setForeground(true); owner.start("en-AU"); drain()
        val sdk = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        sdk.triggerSupportResult(RecognitionSupport.Builder().setInstalledOnDeviceLanguages(listOf("en-AU")).build()); drain()
        assertFalse(owner.snapshot().readyForSpeech)
        sdk.triggerOnReadyForSpeech(Bundle()); drain()
        assertTrue(owner.snapshot().readyForSpeech)
        owner.stop(); drain()
        sdk.triggerOnReadyForSpeech(Bundle()); drain()
        assertFalse(owner.snapshot().readyForSpeech)
        assertEquals(DictationPhase.IDLE, owner.snapshot().phase)
        owner.close(); drain()
    }
    @Test fun installedLanguageSupportGatesRealSdkStartAndFinalDestroysRecognizer() {
        val owner = owner(); owner.setForeground(true); owner.start("en-US"); drain()
        val sdk = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        assertNull(sdk.lastRecognizerIntent)
        sdk.triggerSupportResult(RecognitionSupport.Builder().setInstalledOnDeviceLanguages(listOf("en-US")).build()); drain()
        assertEquals("en-US", sdk.lastRecognizerIntent.getStringExtra(RecognizerIntent.EXTRA_LANGUAGE))
        assertTrue(owner.snapshot().route!!.onDeviceOnly)
        assertTrue(sdk.lastRecognizerIntent.getBooleanExtra(RecognizerIntent.EXTRA_PARTIAL_RESULTS, false))
        sdk.triggerOnPartialResults(Bundle().apply { putStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION, arrayListOf("Synthetic partial")) }); drain()
        sdk.triggerOnResults(Bundle().apply { putStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION, arrayListOf("Synthetic final")) }); drain()
        assertEquals("Synthetic final", owner.snapshot().draft); assertTrue(sdk.isDestroyed)
        assertNull(sdk.latestModelDownloadIntent)
    }
    @Test fun pendingAndOnlineLanguagesDoNotCountAsInstalledAndNeverDownload() {
        val owner = owner(false); owner.setForeground(true); owner.start("en-US"); drain()
        val sdk = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        sdk.triggerSupportResult(RecognitionSupport.Builder().setPendingOnDeviceLanguages(listOf("en-US"))
            .setOnlineLanguages(listOf("en-US")).build()); drain()
        assertEquals("local_language_unavailable", owner.snapshot().error)
        assertNull(sdk.lastRecognizerIntent); assertNull(sdk.latestModelDownloadIntent); assertTrue(sdk.isDestroyed)
    }
    @Test @Config(sdk = [31, 32]) fun api31And32WithoutOnDeviceServiceNeverFallBackToCloud() {
        val owner = owner(false, false); owner.setForeground(true); owner.start("en-US"); drain()
        assertEquals("on_device_recognition_unavailable", owner.snapshot().error)
        assertNull(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
    }
    @Test @Config(sdk = [30]) fun pre31NeverAllocatesACloudRecognizer() {
        val factory = AndroidLocalRecognizerFactory(RuntimeEnvironment.getApplication(), allowProviderFallback = false)
        assertFalse(factory.available())
        assertThrows(UnsupportedOperationException::class.java) { factory.create() }
        assertNull(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
    }
    private fun registerGoogle() {
        registerService("com.google.android.tts", "synthetic.GoogleRecognitionService")
    }
    @Test @Config(sdk = [30, 35]) fun suppliedConstructionNeverUsesOrdinaryProviderUnderStrictPolicy() {
        registerGoogle(); ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(false)
        val factory = AndroidLocalRecognizerFactory(RuntimeEnvironment.getApplication(), allowProviderFallback = true)
        assertThrows(UnsupportedOperationException::class.java) { factory.createSuppliedRecognizer(false) }
        assertNull(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
    }
    @Test @Config(sdk = [33]) fun suppliedConstructionUsesTheSharedGooglePriorityButDoesNotStartOrApprovePcm() {
        registerService("synthetic.other.provider", "synthetic.OtherRecognitionService")
        registerGoogle(); ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(true)
        val factory = AndroidLocalRecognizerFactory(RuntimeEnvironment.getApplication(), allowProviderFallback = false)
        val sdk = factory.createSuppliedRecognizer(true)
        try {
            assertEquals(ComponentName("com.google.android.tts", "synthetic.GoogleRecognitionService"),
                ReflectionHelpers.getField<ComponentName>(sdk, "mServiceComponent"))
            assertNull(shadowOf(sdk).lastRecognizerIntent)
            assertNull(shadowOf(sdk).latestModelDownloadIntent)
            assertFalse(factory.recordingAvailable("en-AU", true))
        } finally { sdk.destroy() }
    }
    @Test fun suppliedConstructionAdmitsAvailableDedicatedApiWithoutStartingListening() {
        registerGoogle(); ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(true)
        val sdk = AndroidLocalRecognizerFactory(RuntimeEnvironment.getApplication()).createSuppliedRecognizer(false)
        try {
            assertNotNull(sdk)
            assertNull(shadowOf(sdk).lastRecognizerIntent)
            assertNull(shadowOf(sdk).latestModelDownloadIntent)
        } finally { sdk.destroy() }
    }
    @Test fun suppliedConstructionRejectsWorkerCallsBeforeSdkAllocation() {
        registerGoogle(); ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(false)
        val factory = AndroidLocalRecognizerFactory(RuntimeEnvironment.getApplication())
        var failure: Throwable? = null
        Thread { try { factory.createSuppliedRecognizer(true) } catch (error: Throwable) { failure = error } }
            .apply { start(); join() }
        assertTrue(failure is IllegalStateException)
        assertNull(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
    }
    @Test fun disablingFallbackRejectsOrdinaryProviderEvenWithInstalledLanguageMetadata() {
        registerGoogle()
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(false)
        val factory = AndroidLocalRecognizerFactory(RuntimeEnvironment.getApplication(), allowProviderFallback = false)
        assertFalse(factory.available())
        assertThrows(UnsupportedOperationException::class.java) { factory.create() }
        assertNull(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
    }
    @Test fun fallbackPolicyIsCapturedBySessionAndFutureAdmissionUsesDedicatedCapability() {
        registerGoogle(); ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(false)
        var allow = true
        val factory = AndroidLocalRecognizerFactory(RuntimeEnvironment.getApplication(), fallbackPolicy = { allow })
        val session = factory.create()
        allow = false
        assertFalse(factory.available())
        session.checkLanguage("en-AU") {}
        val sdk = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        sdk.triggerSupportResult(RecognitionSupport.Builder().setOnlineLanguages(listOf("en-AU")).build()); drain()
        assertFalse(session.route!!.onDeviceOnly)
        assertFalse(session.route!!.localPreferred)
        session.destroy()
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(true)
        val dedicated = factory.create()
        dedicated.checkLanguage("en-AU") {}
        val local = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        local.triggerSupportResult(RecognitionSupport.Builder().setInstalledOnDeviceLanguages(listOf("en-AU")).build()); drain()
        assertTrue(dedicated.route!!.onDeviceOnly)
        dedicated.destroy()
    }
    @Suppress("DEPRECATION")
    private fun registerService(provider: String, service: String) {
        val resolution = ResolveInfo().apply {
            serviceInfo = ServiceInfo().apply { packageName = provider; name = service }
        }
        shadowOf(RuntimeEnvironment.getApplication().packageManager)
            .addResolveInfoForIntent(Intent(RecognitionService.SERVICE_INTERFACE), resolution)
    }
    @Test fun googleWithoutInstalledModelUsesPermittedProviderFallbackWithoutDownload() {
        registerGoogle()
        val owner = owner(onDeviceAvailable = false); owner.setForeground(true); owner.start("en-AU"); drain()
        val sdk = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        sdk.triggerSupportResult(RecognitionSupport.Builder().setPendingOnDeviceLanguages(listOf("en-AU"))
            .setOnlineLanguages(listOf("en-AU")).build()); drain()
        assertEquals(DictationPhase.LISTENING, owner.snapshot().phase)
        assertEquals("en-AU", sdk.lastRecognizerIntent.getStringExtra(RecognizerIntent.EXTRA_LANGUAGE))
        assertFalse(sdk.lastRecognizerIntent.getBooleanExtra(RecognizerIntent.EXTRA_PREFER_OFFLINE, true))
        assertEquals("com.google.android.tts/synthetic.GoogleRecognitionService", owner.snapshot().route?.provider)
        assertEquals("en-AU", owner.snapshot().route?.requestedLanguage)
        assertNull(owner.snapshot().route?.selectedLanguage)
        assertFalse(owner.snapshot().route!!.localPreferred)
        assertFalse(owner.snapshot().route!!.onDeviceOnly)
        assertEquals("installed_language_unavailable", owner.snapshot().route?.fallbackReason)
        assertNull(sdk.latestModelDownloadIntent)
        owner.stop(); drain()
    }
    @Test fun supportCheckWithoutAnyCallbackHasBoundedDeadline() {
        val factory = AndroidLocalRecognizerFactory(RuntimeEnvironment.getApplication())
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(true)
        val session = factory.create()
        val results = mutableListOf<Boolean>()
        session.checkLanguage("en-AU") { results += it }; drain()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(3))
        assertEquals(listOf(false), results)
        session.destroy(); drain()
    }
    @Test fun sdkTerminalCallbacksAreDeduplicatedAndCancelSuppressesStalePartials() {
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(true)
        val session = AndroidLocalRecognizerFactory(RuntimeEnvironment.getApplication()).create()
        val sdk = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        session.checkLanguage("en-AU") {}; drain()
        sdk.triggerSupportResult(RecognitionSupport.Builder().setInstalledOnDeviceLanguages(listOf("en-AU")).build()); drain()
        val finals = mutableListOf<String>(); val errors = mutableListOf<String>(); val partials = mutableListOf<String>()
        session.start("en-AU", object : LocalRecognizerEvents {
            override fun partial(text: String) { partials += text }
            override fun final(text: String) { finals += text }
            override fun error(code: String) { errors += code }
        }); drain()
        sdk.triggerOnPartialResults(Bundle().apply {
            putStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION, arrayListOf("Synthetic marked final"))
            putBoolean("final_result", true)
        }); drain()
        sdk.triggerOnResults(Bundle()); drain()
        sdk.triggerOnResults(Bundle()); sdk.triggerOnError(SpeechRecognizer.ERROR_NO_MATCH); drain()
        session.cancel()
        sdk.triggerOnPartialResults(Bundle().apply { putStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION, arrayListOf("Synthetic stale")) }); drain()
        assertEquals(listOf("Synthetic marked final"), finals)
        assertTrue(errors.isEmpty()); assertEquals(listOf("Synthetic marked final"), partials)
        session.destroy(); drain()
    }
    @Test @Config(sdk = [33]) fun googleComponentWinsOverEarlierRegisteredProviderAndDedicatedFactory() {
        registerService("synthetic.other.provider", "synthetic.OtherRecognitionService")
        registerGoogle()
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(true)
        val factory = AndroidLocalRecognizerFactory(RuntimeEnvironment.getApplication())
        assertTrue(factory.available())
        val session = factory.create()
        val sdk = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
        assertEquals(ComponentName("com.google.android.tts", "synthetic.GoogleRecognitionService"),
            ReflectionHelpers.getField<ComponentName>(sdk, "mServiceComponent"))
        session.destroy(); drain()
    }
    @Test fun unavailableDedicatedAndProviderServiceNeverAllocates() {
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(false)
        val factory = AndroidLocalRecognizerFactory(RuntimeEnvironment.getApplication())
        assertFalse(factory.available())
        assertThrows(UnsupportedOperationException::class.java) { factory.create() }
        assertNull(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
    }
    @Test fun configuredProviderIsAvailableWhenDedicatedAndGoogleAreMissing() {
        registerService("synthetic.other.provider", "synthetic.OtherRecognitionService")
        android.provider.Settings.Secure.putString(RuntimeEnvironment.getApplication().contentResolver,
            "voice_recognition_service", "synthetic.other.provider/synthetic.OtherRecognitionService")
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(false)
        val factory = AndroidLocalRecognizerFactory(RuntimeEnvironment.getApplication())
        assertTrue(factory.available())
        val session = factory.create()
        session.checkLanguage("en-AU") {}; drain()
        val sdk = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        sdk.triggerSupportResult(RecognitionSupport.Builder().setInstalledOnDeviceLanguages(listOf("en-US", "en-GB")).build()); drain()
        assertEquals("synthetic.other.provider/synthetic.OtherRecognitionService", session.route?.provider)
        assertEquals("en-US", session.route?.selectedLanguage)
        assertTrue(session.route!!.localPreferred)
        session.destroy(); drain()
    }
    @Test @Config(sdk = [30, 31, 32]) fun legacyGoogleMetadataUnknownAllowsHonestProviderControlledMode() {
        registerGoogle()
        val factory = AndroidLocalRecognizerFactory(RuntimeEnvironment.getApplication())
        assertTrue(factory.available())
        val session = factory.create()
        val results = mutableListOf<Boolean>()
        session.checkLanguage("en-AU") { results += it }; drain()
        assertEquals(listOf(true), results)
        assertFalse(session.route!!.localPreferred)
        assertNull(session.route?.selectedLanguage)
        assertEquals("support_metadata_unavailable", session.route?.fallbackReason)
        session.start("en-AU", RecordingEvents()); drain()
        val sdk = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        assertFalse(sdk.lastRecognizerIntent.getBooleanExtra(RecognizerIntent.EXTRA_PREFER_OFFLINE, true))
        session.destroy(); drain()
    }
    @Test fun exactAuWinsOtherwiseProviderOrderSelectsUsRatherThanForcingUk() {
        fun selected(installed: List<String>): String? {
            registerGoogle()
            val owner = owner(); owner.setForeground(true); owner.start("en-AU"); drain()
            val sdk = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
            sdk.triggerSupportResult(RecognitionSupport.Builder().setInstalledOnDeviceLanguages(installed).build()); drain()
            val language = sdk.lastRecognizerIntent.getStringExtra(RecognizerIntent.EXTRA_LANGUAGE)
            assertEquals("en-AU", owner.snapshot().route?.requestedLanguage)
            assertEquals(language, owner.snapshot().route?.selectedLanguage)
            assertNull(owner.snapshot().route?.reportedLanguage)
            assertTrue(owner.snapshot().route!!.localPreferred)
            assertFalse(owner.snapshot().route!!.onDeviceOnly)
            owner.stop(); drain()
            return language
        }
        assertEquals("en-AU", selected(listOf("en-US", "en-GB", "en-AU")))
        assertEquals("en-US", selected(listOf("fr-FR", "en-US", "en-GB")))
    }
    @Test fun error14ThenEmptyAndRepeatedValidRepliesWithin100msSettlesInstalledExactlyOnce() {
        registerGoogle()
        val session = AndroidLocalRecognizerFactory(RuntimeEnvironment.getApplication()).create()
        val results = mutableListOf<Boolean>()
        session.checkLanguage("en-AU") { results += it }; drain()
        val sdk = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        sdk.triggerSupportError(SpeechRecognizer.ERROR_CANNOT_CHECK_SUPPORT); drain()
        sdk.triggerSupportResult(RecognitionSupport.Builder().setOnlineLanguages(listOf("en-AU")).build()); drain()
        assertTrue(results.isEmpty())
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofMillis(100))
        sdk.triggerSupportResult(RecognitionSupport.Builder().setInstalledOnDeviceLanguages(listOf("en-AU")).build())
        sdk.triggerSupportResult(RecognitionSupport.Builder().setInstalledOnDeviceLanguages(listOf("en-US")).build()); drain()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(4))
        assertEquals(listOf(true), results); assertEquals("en-AU", session.route?.selectedLanguage)
        assertTrue(session.route!!.localPreferred); assertNull(session.route?.fallbackReason)
        session.destroy(); drain()
    }
    @Test fun repeatedError14DoesNotExtendDeadlineAndLateSupportCannotChangeFallback() {
        registerGoogle()
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(false)
        val session = AndroidLocalRecognizerFactory(RuntimeEnvironment.getApplication()).create()
        val results = mutableListOf<Boolean>()
        session.checkLanguage("en-AU") { results += it }; drain()
        val sdk = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        sdk.triggerSupportError(SpeechRecognizer.ERROR_CANNOT_CHECK_SUPPORT); drain()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofMillis(2_900))
        sdk.triggerSupportError(SpeechRecognizer.ERROR_CANNOT_CHECK_SUPPORT); drain()
        assertTrue(results.isEmpty())
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofMillis(100))
        assertEquals(listOf(true), results)
        assertFalse(session.route!!.localPreferred); assertNull(session.route?.selectedLanguage)
        sdk.triggerSupportResult(RecognitionSupport.Builder().setInstalledOnDeviceLanguages(listOf("en-AU")).build()); drain()
        assertEquals(listOf(true), results); assertFalse(session.route!!.localPreferred)
        session.destroy(); drain()
    }
    @Test fun strictPolicyError14TimesOutUnavailableAndCancellationSuppressesSupportAndDeadline() {
        registerGoogle()
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(true)
        fun checking(cancel: Boolean) {
            val session = AndroidLocalRecognizerFactory(RuntimeEnvironment.getApplication(), false).create()
            val results = mutableListOf<Boolean>()
            session.checkLanguage("en-AU") { results += it }; drain()
            assertTrue(session.route!!.onDeviceOnly)
            val sdk = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
            sdk.triggerSupportError(SpeechRecognizer.ERROR_CANNOT_CHECK_SUPPORT); drain()
            if (cancel) {
                session.cancel()
                sdk.triggerSupportResult(RecognitionSupport.Builder().setInstalledOnDeviceLanguages(listOf("en-AU")).build()); drain()
            }
            shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(4))
            assertEquals(if (cancel) emptyList<Boolean>() else listOf(false), results)
            assertNull(session.route?.selectedLanguage)
            assertNull(sdk.lastRecognizerIntent); assertNull(sdk.latestModelDownloadIntent)
            session.destroy(); drain()
        }
        checking(false); checking(true)
    }
    @Test fun permissionSupportErrorIsTerminalWithoutNetworkRetry() {
        registerGoogle()
        val session = AndroidLocalRecognizerFactory(RuntimeEnvironment.getApplication()).create()
        val results = mutableListOf<Boolean>()
        session.checkLanguage("en-AU") { results += it }; drain()
        val sdk = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        sdk.triggerSupportError(SpeechRecognizer.ERROR_INSUFFICIENT_PERMISSIONS); drain()
        sdk.triggerSupportResult(RecognitionSupport.Builder().setInstalledOnDeviceLanguages(listOf("en-AU")).build()); drain()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(4))
        assertEquals(listOf(false), results); assertNull(sdk.lastRecognizerIntent)
        assertNull(sdk.latestModelDownloadIntent)
        session.destroy(); drain()
    }
    @Test fun missingDedicatedModelRetiresItBeforeProviderControlledAdmission() {
        registerService("synthetic.other.provider", "synthetic.OtherRecognitionService")
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(true)
        val session = AndroidLocalRecognizerFactory(RuntimeEnvironment.getApplication()).create()
        val dedicated = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        val results = mutableListOf<Boolean>()
        session.checkLanguage("en-AU") { results += it }; drain()
        dedicated.triggerSupportResult(RecognitionSupport.Builder().setPendingOnDeviceLanguages(listOf("en-AU")).build()); drain()
        val controlled = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        assertNotSame(dedicated, controlled); assertTrue(dedicated.isDestroyed)
        assertTrue(results.isEmpty()); assertNull(controlled.lastRecognizerIntent)
        dedicated.triggerSupportResult(RecognitionSupport.Builder().setInstalledOnDeviceLanguages(listOf("en-AU")).build()); drain()
        assertTrue(results.isEmpty())
        controlled.triggerSupportResult(RecognitionSupport.Builder().setOnlineLanguages(listOf("en-AU")).build()); drain()
        assertEquals(listOf(true), results); assertFalse(session.route!!.localPreferred)
        assertNull(session.route?.selectedLanguage); assertNull(dedicated.lastRecognizerIntent)
        assertNull(dedicated.latestModelDownloadIntent)
        session.start("en-AU", RecordingEvents()); drain()
        assertEquals("en-AU", controlled.lastRecognizerIntent.getStringExtra(RecognizerIntent.EXTRA_LANGUAGE))
        assertNull(controlled.latestModelDownloadIntent)
        session.destroy(); drain()
    }
    @Test fun missingDedicatedModelStillPrefersInstalledLocalOnConfiguredProviderBeforeNetworkAdmission() {
        registerService("synthetic.other.provider", "synthetic.OtherRecognitionService")
        android.provider.Settings.Secure.putString(RuntimeEnvironment.getApplication().contentResolver,
            "voice_recognition_service", "synthetic.other.provider/synthetic.OtherRecognitionService")
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(true)
        val session = AndroidLocalRecognizerFactory(RuntimeEnvironment.getApplication()).create()
        val dedicated = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        val results = mutableListOf<Boolean>()
        session.checkLanguage("en-AU") { results += it }; drain()
        dedicated.triggerSupportResult(RecognitionSupport.Builder().build()); drain()
        val configured = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        assertTrue(dedicated.isDestroyed); assertTrue(results.isEmpty())
        configured.triggerSupportResult(RecognitionSupport.Builder().setInstalledOnDeviceLanguages(listOf("en-US", "en-AU")).build()); drain()
        assertEquals(listOf(true), results)
        assertTrue(session.route!!.localPreferred); assertEquals("en-AU", session.route?.selectedLanguage)
        assertFalse(session.route!!.onDeviceOnly)
        assertEquals("synthetic.other.provider/synthetic.OtherRecognitionService", session.route?.provider)
        assertEquals("dedicated_installed_language_unavailable", session.route?.fallbackReason)
        session.start("en-AU", RecordingEvents()); drain()
        assertTrue(configured.lastRecognizerIntent.getBooleanExtra(RecognizerIntent.EXTRA_PREFER_OFFLINE, false))
        assertNull(configured.latestModelDownloadIntent); assertNull(dedicated.lastRecognizerIntent)
        session.destroy(); drain()
    }
    @Test fun nonemptyFinalOverridesMarkedPartialAndBlankCurrentLocaleRemainsUnknown() {
        val owner = owner(); owner.setForeground(true); owner.start("en-AU"); drain()
        val sdk = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        sdk.triggerSupportResult(RecognitionSupport.Builder().setInstalledOnDeviceLanguages(listOf("en-US")).build()); drain()
        sdk.triggerOnPartialResults(result("Synthetic marked partial", final = true, locale = "")); drain()
        assertNull(owner.snapshot().route?.reportedLanguage)
        sdk.triggerOnResults(result("Synthetic authoritative final", locale = "")); drain()
        assertEquals("Synthetic authoritative final", owner.snapshot().draft)
        assertEquals(DictationPhase.FINAL, owner.snapshot().phase)
        assertEquals("en-AU", owner.snapshot().route?.requestedLanguage)
        assertEquals("en-US", owner.snapshot().route?.selectedLanguage)
        assertNull(owner.snapshot().route?.reportedLanguage)
    }
    @Test fun reportedLocaleIsDistinctAndRouteSurvivesCancelAndError() {
        val owner = owner(); owner.setForeground(true); owner.start("en-AU"); drain()
        var sdk = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        sdk.triggerSupportResult(RecognitionSupport.Builder().setInstalledOnDeviceLanguages(listOf("en-US")).build()); drain()
        sdk.triggerOnPartialResults(result("Synthetic partial", locale = "en-gb")); drain()
        assertEquals("en-GB", owner.snapshot().route?.reportedLanguage)
        owner.stop(); drain()
        assertEquals("en-GB", owner.snapshot().route?.reportedLanguage)
        assertEquals("en-AU", owner.snapshot().route?.requestedLanguage)
        owner.start("en-AU"); drain()
        sdk = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        sdk.triggerSupportResult(RecognitionSupport.Builder().setInstalledOnDeviceLanguages(listOf("en-US")).build()); drain()
        sdk.triggerOnError(SpeechRecognizer.ERROR_NO_MATCH); drain()
        assertEquals(DictationPhase.ERROR, owner.snapshot().phase)
        assertEquals("en-US", owner.snapshot().route?.selectedLanguage)
        assertNull(owner.snapshot().route?.reportedLanguage)
    }
    @Test fun activeCancelAndDestroyInvalidateAllStaleRecognitionCallbacks() {
        registerGoogle()
        for (destroy in listOf(false, true)) {
            val session = AndroidLocalRecognizerFactory(RuntimeEnvironment.getApplication()).create()
            session.checkLanguage("en-AU") {}; drain()
            val sdk = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
            sdk.triggerSupportResult(RecognitionSupport.Builder().setInstalledOnDeviceLanguages(listOf("en-AU")).build()); drain()
            val callbacks = mutableListOf<String>()
            session.start("en-AU", object : LocalRecognizerEvents {
                override fun partial(text: String) { callbacks += text }
                override fun final(text: String) { callbacks += text }
                override fun error(code: String) { callbacks += code }
            }); drain()
            if (destroy) session.destroy() else session.cancel()
            val route = session.route
            sdk.triggerOnPartialResults(result("Synthetic stale partial", locale = "en-US"))
            sdk.triggerOnResults(result("Synthetic stale final", locale = "en-US"))
            sdk.triggerOnError(SpeechRecognizer.ERROR_NO_MATCH); drain()
            assertTrue(callbacks.isEmpty()); assertEquals(route, session.route)
            if (!destroy) session.destroy()
            drain()
        }
    }
    @Test fun routeIsVisibleDuringSupportCheckAndAfterCancellationBeforeSupport() {
        registerGoogle()
        val owner = owner(); owner.setForeground(true); owner.start("en-AU"); drain()
        val route = owner.snapshot().route!!
        assertEquals(DictationPhase.CHECKING, owner.snapshot().phase)
        assertEquals("en-AU", route.requestedLanguage)
        assertEquals("com.google.android.tts/synthetic.GoogleRecognitionService", route.provider)
        owner.stop(); drain()
        val sdk = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        sdk.triggerSupportError(SpeechRecognizer.ERROR_CANNOT_CHECK_SUPPORT)
        sdk.triggerSupportResult(RecognitionSupport.Builder().setInstalledOnDeviceLanguages(listOf("en-AU")).build()); drain()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(4))
        assertEquals(route, owner.snapshot().route)
        assertEquals(DictationPhase.IDLE, owner.snapshot().phase)
        assertNull(sdk.lastRecognizerIntent)
    }
    @Test fun ordinaryPartialIsNotPromotedToFinalAndRuntimeLanguageErrorNeverRecaptures() {
        registerGoogle()
        val owner = owner(); owner.setForeground(true); owner.start("en-AU"); drain()
        var sdk = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        sdk.triggerSupportResult(RecognitionSupport.Builder().setInstalledOnDeviceLanguages(listOf("en-AU")).build()); drain()
        sdk.triggerOnPartialResults(result("Synthetic ordinary partial")); drain()
        sdk.triggerOnResults(Bundle()); drain()
        assertEquals("no_speech", owner.snapshot().error)
        assertEquals("Synthetic ordinary partial", owner.snapshot().draft)
        owner.start("en-AU"); drain()
        val admitted = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
        sdk = shadowOf(admitted)
        sdk.triggerSupportResult(RecognitionSupport.Builder().setInstalledOnDeviceLanguages(listOf("en-AU")).build()); drain()
        sdk.triggerOnError(SpeechRecognizer.ERROR_LANGUAGE_UNAVAILABLE); drain()
        assertEquals("recognition_error_13", owner.snapshot().error)
        assertSame(admitted, ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        assertTrue(owner.snapshot().route!!.localPreferred)
        assertTrue(sdk.isDestroyed); assertNull(sdk.latestModelDownloadIntent)
    }
    private fun result(text: String, final: Boolean = false, locale: String? = null) = Bundle().apply {
        putStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION, arrayListOf(text))
        putBoolean("final_result", final)
        if (locale != null) putString("current_locale", locale)
    }
    private class RecordingEvents : LocalRecognizerEvents {
        override fun partial(text: String) {}
        override fun final(text: String) {}
        override fun error(code: String) {}
    }
}
