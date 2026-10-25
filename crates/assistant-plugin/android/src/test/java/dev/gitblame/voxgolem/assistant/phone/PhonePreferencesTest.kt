package dev.gitblame.voxgolem.assistant.phone

import org.junit.Assert.*
import org.junit.Test

@org.junit.runner.RunWith(PhoneAssistantRunner::class)
@org.robolectric.annotation.Config(sdk = [35], manifest = org.robolectric.annotation.Config.NONE)
class PhonePreferencesTest {
    @Test fun synchronousReadsAreCacheOnlyAndInitializationIsExplicitAndIdempotent() {
        val real = org.robolectric.RuntimeEnvironment.getApplication().getSharedPreferences("synthetic-bootstrap", 0)
        real.edit().clear().putString("ui_theme", "slate").commit()
        var reads = 0
        val storage = object : android.content.SharedPreferences by real {
            override fun getString(key: String?, default: String?): String? { reads++; return real.getString(key, default) }
            override fun getBoolean(key: String?, default: Boolean): Boolean { reads++; return real.getBoolean(key, default) }
        }
        val preferences = PhonePreferences(storage)
        assertThrows(AssistantPhoneFailure::class.java) { preferences.read("custom-sol-high") }
        assertEquals(0, reads)
        preferences.initialize()
        val initializedReads = reads
        assertTrue(initializedReads > 0)
        preferences.initialize()
        repeat(3) { assertEquals("slate", preferences.read("custom-sol-high").getString("ui_theme")) }
        assertEquals(initializedReads, reads)
    }
    @Test fun pendingCommitAndRollbackNeverBlockReadersOrPublishFailedValues() {
        for (succeed in listOf(true, false)) {
            val real = org.robolectric.RuntimeEnvironment.getApplication().getSharedPreferences("synthetic-presentation", 0)
            real.edit().clear().commit()
            val entered = java.util.concurrent.CountDownLatch(1)
            val release = java.util.concurrent.CountDownLatch(1)
            val rollbackEntered = java.util.concurrent.CountDownLatch(1)
            val rollbackRelease = java.util.concurrent.CountDownLatch(1)
            val commits = java.util.concurrent.atomic.AtomicInteger()
            val storage = object : android.content.SharedPreferences by real {
                override fun edit(): android.content.SharedPreferences.Editor {
                    val editor = real.edit()
                    return object : android.content.SharedPreferences.Editor by editor {
                        override fun commit(): Boolean {
                            val rollback = commits.incrementAndGet() > 1
                            (if (rollback) rollbackEntered else entered).countDown()
                            check((if (rollback) rollbackRelease else release).await(5, java.util.concurrent.TimeUnit.SECONDS))
                            editor.commit()
                            return succeed
                        }
                    }
                }
            }
            val preferences = PhonePreferences(storage)
            preferences.initialize()
            assertEquals("dark", preferences.read("custom-sol-high").getString("ui_theme"))
            val executor = java.util.concurrent.Executors.newFixedThreadPool(2)
            try {
                val write = executor.submit<Unit> { preferences.write(PhonePreferences.defaults().put("ui_theme", "slate")) }
                assertTrue(entered.await(5, java.util.concurrent.TimeUnit.SECONDS))
                val read = executor.submit<String> { preferences.read("custom-sol-high").getString("ui_theme") }
                try { assertEquals("dark", read.get(1, java.util.concurrent.TimeUnit.SECONDS)) }
                finally { release.countDown() }
                if (succeed) write.get(5, java.util.concurrent.TimeUnit.SECONDS)
                else {
                    assertTrue(rollbackEntered.await(5, java.util.concurrent.TimeUnit.SECONDS))
                    val rollbackRead = executor.submit<String> { preferences.read("custom-sol-high").getString("ui_theme") }
                    try { assertEquals("dark", rollbackRead.get(1, java.util.concurrent.TimeUnit.SECONDS)) }
                    finally { rollbackRelease.countDown() }
                    assertTrue(assertThrows(java.util.concurrent.ExecutionException::class.java) {
                        write.get(5, java.util.concurrent.TimeUnit.SECONDS)
                    }.cause is AssistantPhoneFailure)
                }
                assertEquals(if (succeed) "slate" else "dark", preferences.read("custom-sol-high").getString("ui_theme"))
            } finally { release.countDown(); rollbackRelease.countDown(); executor.shutdownNow() }
        }
    }
    @Test fun acceptsAllThemesAndKeepsDarkDefaultWithoutAcceptingAliases() {
        assertEquals("dark", PhonePreferences.defaults().getString("ui_theme"))
        for (theme in listOf("dark", "slate", "light")) {
            PhonePreferences.validate(PhonePreferences.defaults().put("ui_theme", theme))
        }
        for (theme in listOf("sepia", "dim", "sleek-dark", true, 0)) {
            val error = assertThrows(AssistantPhoneFailure::class.java) {
                PhonePreferences.validate(PhonePreferences.defaults().put("ui_theme", theme))
            }
            assertEquals("invalid_input", error.code)
        }
    }
    @Test fun retainsAllLegacyKeysAndEnumSpellings() {
        val value = PhonePreferences.defaults().put("deep", "custom-luna-low").put("review", "custom-luna-low").put("ui_text_size", "extra_large")
        PhonePreferences.validate(value)
        assertEquals(17, value.length())
        assertFalse(value.getBoolean("fast_mode"))
        assertTrue(value.getBoolean("web_enabled"))
        assertTrue(value.getBoolean("speech_network_fallback"))
        for (key in listOf("speech_language", "speech_tts_provider", "speech_tts_voice")) assertEquals("", value.getString(key))
    }
    @Test fun unavailableFlagsAreNotSilentlyDowngraded() {
        for (key in listOf("deep_enabled", "review_enabled", "prefetch", "completion")) {
            try { PhonePreferences.validate(PhonePreferences.defaults().put(key, true)); fail("must reject unsupported capability") }
            catch (error: AssistantPhoneFailure) { assertEquals("unavailable", error.code) }
        }
    }
    @Test fun acceptsNativeTtsPreferenceWithAvailabilityValidatedByBackend() {
        PhonePreferences.validate(PhonePreferences.defaults().put("tts_enabled", true))
    }
    @Test fun acceptsDisabledWebAndStrictSpeechPolicyWithoutBooleanCoercion() {
        PhonePreferences.validate(PhonePreferences.defaults().put("web_enabled", false).put("speech_network_fallback", false))
        for (key in listOf("web_enabled", "speech_network_fallback")) {
            try { PhonePreferences.validate(PhonePreferences.defaults().put(key, "false")); fail("must require boolean") }
            catch (error: AssistantPhoneFailure) { assertEquals("invalid_input", error.code) }
        }
    }
    @Test fun currentPayloadRequiresAllSeventeenFieldsRatherThanFillingLegacyOmissions() {
        for (key in PhonePreferences.defaults().keys()) {
            val incomplete = PhonePreferences.defaults().apply { remove(key) }
            assertThrows("Missing $key", AssistantPhoneFailure::class.java) { PhonePreferences.validate(incomplete) }
        }
    }
    @Test fun unknownKeysAndBooleanCoercionAreRejected() {
        for (value in listOf(PhonePreferences.defaults().put("extra", false), PhonePreferences.defaults().put("completion", "false"))) {
            try { PhonePreferences.validate(value); fail("must reject malformed preferences") }
            catch (error: AssistantPhoneFailure) { assertEquals("invalid_input", error.code) }
        }
    }
    @Test fun speechSelectionShapeRequiresStringsAndAValidLocaleWithoutInventingSdkSupport() {
        PhonePreferences.validate(PhonePreferences.defaults().put("speech_language", "en-AU")
            .put("speech_tts_provider", "synthetic.tts").put("speech_tts_voice", "synthetic-au-local"))
        for (value in listOf(PhonePreferences.defaults().put("speech_language", "en--AU"),
            PhonePreferences.defaults().put("speech_tts_voice", false), PhonePreferences.defaults().put("speech_tts_provider", "x".repeat(513)))) {
            assertThrows(AssistantPhoneFailure::class.java) { PhonePreferences.validate(value) }
        }
    }
    @Test fun multibyteSpeechIdsRespectTheCoreUtf8ByteLimitBeforePreferencePersistence() {
        val exact = "界".repeat(170) + "ab"
        PhonePreferences.validate(PhonePreferences.defaults().put("speech_tts_provider", exact).put("speech_tts_voice", exact))
        for (key in listOf("speech_tts_provider", "speech_tts_voice")) {
            assertThrows(AssistantPhoneFailure::class.java) { PhonePreferences.validate(PhonePreferences.defaults().put(key, "界".repeat(171))) }
        }
    }
}
