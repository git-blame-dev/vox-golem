package dev.gitblame.voxgolem.assistant.phone

import dev.gitblame.voxgolem.assistant.AssistantStore
import dev.gitblame.voxgolem.assistant.DomainGateway
import dev.gitblame.voxgolem.assistant.runtime.*
import dev.gitblame.voxgolem.assistant.speech.SpeechOptions
import dev.gitblame.voxgolem.assistant.speech.SpeechSelection
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.Executor

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class EnginePhonePreferencesTest {
    private class Queue : Executor {
        private val tasks = ConcurrentLinkedQueue<Runnable>()
        override fun execute(command: Runnable) { tasks.add(command) }
        fun drain() { while (true) (tasks.poll() ?: return).run() }
    }
    private class Fixture(enabled: Boolean, selection: SpeechSelection = SpeechSelection()) : AutoCloseable {
        val context = RuntimeEnvironment.getApplication()
        private val io = Queue()
        // Preferences do not use conversation decisions. Only the initial empty-view projection is simulated.
        private val gateway = DomainGateway { raw ->
            check(JSONObject(raw).getString("kind") == "projection")
            """{"ok":true,"value":{"view":{"revision":"0","records":[],"conversations":[],"pending":[]},"continuation":null,"read":null}}"""
        }
        private val store = AssistantStore(context, gateway)
        var rejectModelWrite = false
        val prewarmed = mutableListOf<String>()
        private val models = object : AssistantModelStorage {
            private var value = "custom-sol-high"
            override fun read() = value
            override fun write(value: String) {
                if (rejectModelWrite) { rejectModelWrite = false; error("Synthetic model storage failure") }
                this.value = value
            }
        }
        private val policies = NativeAssistantPolicyStorage(context).apply {
            write(AssistantPolicy(speechLanguage = selection.language, speechTtsProvider = selection.ttsProvider, speechTtsVoice = selection.ttsVoice))
        }
        private val worker = AssistantRequestWorker(io, object : ProviderNativeCalls {
            override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String =
                error("Preference updates must not submit a provider request")
            override fun cancel(requestId: String) {}
        })
        private val authPath: () -> String = { "/synthetic/private/provider.json" }
        // The shared actor's constructor is Kotlin-internal to its module; use its JVM constructor without changing that boundary.
        val engine = AssistantEngine::class.java.declaredConstructors.single {
            it.parameterTypes.size == 8 && it.parameterTypes.first() == AssistantStore::class.java
        }.newInstance(store, io, worker, authPath, models, null, policies, gateway) as AssistantEngine
        val backend: EnginePhoneBackend
        init {
            io.drain()
            PhonePreferences(context).write(PhonePreferences.defaults().put("tts_enabled", enabled)
                .put("speech_language", selection.language).put("speech_tts_provider", selection.ttsProvider).put("speech_tts_voice", selection.ttsVoice))
            backend = EnginePhoneBackend(context, engine) { model ->
                assertEquals(model, engine.snapshot().model)
                assertEquals(model, models.read())
                assertEquals(model, persisted().getString("instant"))
                assertEquals(model, context.getSharedPreferences("assistant-phone-preferences", android.content.Context.MODE_PRIVATE)
                    .getString("instant", null))
                assertEquals(engine.snapshot().webEnabled, persistedPolicy().webEnabled)
                assertFalse(engine.snapshot().busy)
                prewarmed += model
            }
            backend.speechStatus = JSONObject().put("ttsAvailable", false)
        }
        fun update(input: JSONObject): Result<Any?> {
            var terminal: Result<Any?>? = null
            backend.command("set_preferences", JSONObject().put("preferences", input)) { terminal = it }
            io.drain()
            return terminal ?: error("Preference update did not settle")
        }
        fun persisted() = PhonePreferences(context).read(engine.snapshot().model)
        fun persistedPolicy() = NativeAssistantPolicyStorage(context).read()
        fun duplicatePolicyKeys() = context.getSharedPreferences("assistant-phone-preferences", android.content.Context.MODE_PRIVATE)
            .all.keys.filter(dev.gitblame.voxgolem.voice.InstallationPreferences::ownsKey)
        fun publishOptions(options: SpeechOptions) { backend.publishSpeechOptions(options); io.drain() }
        fun queueUpdate(input: JSONObject, reply: (Result<Any?>) -> Unit) {
            backend.command("set_preferences", JSONObject().put("preferences", input), reply)
        }
        fun drain() = io.drain()
        override fun close() { engine.stop(); io.drain(); worker.close(); store.close() }
    }

    @After fun resetAdmission() { AssistantRuntime.configureCaptureResourceAdmission(false) }

    @Test fun acceptedModelChangesPrewarmOnlyAfterPersistenceAndInvalidateForNonAgySelection() {
        AssistantRuntime.configureCaptureResourceAdmission(true)
        Fixture(false).use { f ->
            f.backend.speechStatus = JSONObject().put("ttsAvailable", true)
            f.update(f.backend.nativePreferences().put("instant", "agy-flash-high").put("web_enabled", false)).getOrThrow()
            assertEquals(listOf("agy-flash-high"), f.prewarmed)
            for (input in listOf(
                f.backend.nativePreferences().put("ui_theme", "light"),
                f.backend.nativePreferences().put("tts_enabled", true),
                f.backend.nativePreferences().put("web_enabled", true),
            )) {
                f.update(input).getOrThrow()
                assertEquals(listOf("agy-flash-high"), f.prewarmed)
            }
            f.update(f.backend.nativePreferences().put("instant", "custom-sol-none")).getOrThrow()
            assertEquals(listOf("agy-flash-high", "custom-sol-none"), f.prewarmed)
        }
    }

    @Test fun rejectedUnsupportedUnavailableAndRolledBackSelectionsNeverPrewarm() {
        Fixture(false).use { f ->
            val initial = f.backend.nativePreferences()
            for (input in listOf(
                JSONObject(initial.toString()).put("instant", "synthetic-unsupported"),
                JSONObject(initial.toString()).put("instant", "agy-flash-high").put("tts_enabled", true),
                JSONObject(initial.toString()).put("instant", "agy-flash-high").put("speech_tts_provider", "synthetic.missing.tts"),
            )) {
                assertTrue(f.update(input).isFailure)
                assertTrue(f.prewarmed.isEmpty())
                assertEquals("custom-sol-high", f.engine.snapshot().model)
                assertEquals("custom-sol-high", f.persisted().getString("instant"))
            }
            f.rejectModelWrite = true
            assertTrue(f.update(JSONObject(initial.toString()).put("instant", "agy-flash-high")).isFailure)
            assertTrue(f.prewarmed.isEmpty())
            assertEquals("custom-sol-high", f.engine.snapshot().model)
            assertEquals("custom-sol-high", f.persisted().getString("instant"))
        }
    }

    @Test fun pendingPreferenceChoiceRejectsBusySelectionWithoutPrewarmingIt() {
        Fixture(false).use { f ->
            var first: Result<Any?>? = null
            var busy: Result<Any?>? = null
            f.queueUpdate(f.backend.nativePreferences().put("web_enabled", false)) { first = it }
            f.queueUpdate(f.backend.nativePreferences().put("instant", "agy-flash-high")) { busy = it }
            assertEquals("busy", (busy?.exceptionOrNull() as AssistantPhoneFailure).code)
            assertTrue(f.prewarmed.isEmpty())
            f.drain()
            first!!.getOrThrow()
            assertTrue(f.prewarmed.isEmpty())
            assertEquals("custom-sol-high", f.persisted().getString("instant"))
        }
    }

    @Test fun legacyPhonePolicyKeysRemainUntouchedAndNeverOverrideTheActorPolicy() {
        Fixture(false, SpeechSelection(language = "en-AU")).use { f ->
            val legacy = f.context.getSharedPreferences("assistant-phone-preferences", android.content.Context.MODE_PRIVATE)
            legacy.edit().putString("speech_language", "fr-FR").putString("speech_tts_provider", "synthetic.legacy.tts")
                .putString("speech_tts_voice", "synthetic-legacy-voice").putBoolean("speech_network_fallback", false)
                .putBoolean("web_enabled", true).commit()
            val before = legacy.all.filterKeys(dev.gitblame.voxgolem.voice.InstallationPreferences::ownsKey)
            val input = f.backend.nativePreferences()
            assertEquals("en-AU", input.getString("speech_language"))
            assertEquals("", input.getString("speech_tts_provider"))
            assertTrue(input.getBoolean("speech_network_fallback"))
            val settled = f.update(input.put("web_enabled", false)).getOrThrow() as JSONObject
            assertFalse(settled.getBoolean("web_enabled"))
            assertEquals("en-AU", settled.getString("speech_language"))
            assertEquals(before, legacy.all.filterKeys(dev.gitblame.voxgolem.voice.InstallationPreferences::ownsKey))
            assertFalse(f.persistedPolicy().webEnabled)
            assertEquals("en-AU", f.persistedPolicy().speechLanguage)
        }
    }

    @Test fun previouslyEnabledUnavailableTtsDoesNotBlockIndependentPrivacyChangesOrLoseItsSelection() {
        AssistantRuntime.configureCaptureResourceAdmission(false)
        val selection = SpeechSelection(ttsProvider = "synthetic.retired.tts", ttsVoice = "synthetic-retired-voice")
        Fixture(true, selection).use { f ->
            val input = f.backend.nativePreferences().put("web_enabled", false).put("speech_network_fallback", false)
            val settled = f.update(input).getOrThrow() as JSONObject
            assertTrue(settled.getBoolean("tts_enabled"))
            assertFalse(settled.getBoolean("web_enabled")); assertFalse(settled.getBoolean("speech_network_fallback"))
            assertEquals(selection, f.persistedPolicy().selection())
            assertEquals(selection.ttsProvider, f.backend.nativePreferences().getString("speech_tts_provider"))
            assertEquals(selection.ttsVoice, f.backend.nativePreferences().getString("speech_tts_voice"))
            assertTrue(f.duplicatePolicyKeys().isEmpty())
        }
    }

    @Test fun previouslyEnabledUnavailableTtsCanRecoverToAutomaticProviderAndVoiceAndPersistThatRecovery() {
        AssistantRuntime.configureCaptureResourceAdmission(true)
        Fixture(true, SpeechSelection(ttsProvider = "synthetic.retired.tts", ttsVoice = "synthetic-retired-voice")).use { f ->
            val input = f.backend.nativePreferences().put("speech_tts_provider", "").put("speech_tts_voice", "")
            val settled = f.update(input).getOrThrow() as JSONObject
            assertTrue(settled.getBoolean("tts_enabled"))
            assertEquals("", settled.getString("speech_tts_provider")); assertEquals("", settled.getString("speech_tts_voice"))
            assertEquals(SpeechSelection(), f.persistedPolicy().selection())
            assertTrue(f.duplicatePolicyKeys().isEmpty())
        }
    }

    @Test fun explicitNewEnableStillRequiresBothNativeResourceAdmissionAndAvailableTts() {
        for ((admitted, available) in listOf(false to true, true to false, false to false, true to true)) {
            AssistantRuntime.configureCaptureResourceAdmission(admitted)
            Fixture(false).use { f ->
                f.backend.speechStatus = JSONObject().put("ttsAvailable", available)
                val result = f.update(f.backend.nativePreferences().put("tts_enabled", true))
                if (admitted && available) {
                    assertTrue((result.getOrThrow() as JSONObject).getBoolean("tts_enabled"))
                    assertTrue(f.persisted().getBoolean("tts_enabled"))
                } else {
                    assertEquals("unavailable", (result.exceptionOrNull() as AssistantPhoneFailure).code)
                    assertFalse(f.persisted().getBoolean("tts_enabled"))
                }
            }
        }
    }

    @Test fun recoveryBypassDoesNotPermitUnreportedProviderOrVoiceOrPersistRejectedSelections() {
        AssistantRuntime.configureCaptureResourceAdmission(true)
        Fixture(true).use { f ->
            f.publishOptions(SpeechOptions(ttsProviders = listOf("synthetic.installed.tts")))
            for (input in listOf(f.backend.nativePreferences().put("speech_tts_provider", "synthetic.missing.tts"),
                f.backend.nativePreferences().put("speech_tts_provider", "synthetic.installed.tts").put("speech_tts_voice", "synthetic-missing-voice"))) {
                val result = f.update(input)
                assertEquals("unavailable", (result.exceptionOrNull() as AssistantPhoneFailure).code)
                assertEquals(SpeechSelection(), f.persistedPolicy().selection())
                assertTrue(f.duplicatePolicyKeys().isEmpty())
                assertTrue(f.persisted().getBoolean("tts_enabled")); assertFalse(f.backend.busy())
            }
        }
    }
}
