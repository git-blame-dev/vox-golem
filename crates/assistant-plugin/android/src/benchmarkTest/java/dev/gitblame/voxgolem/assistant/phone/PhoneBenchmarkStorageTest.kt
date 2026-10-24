package dev.gitblame.voxgolem.assistant.phone

import android.content.Context
import android.content.ContextWrapper
import android.content.SharedPreferences
import dev.gitblame.voxgolem.benchmark.android.LocalFixtureSynthesis
import java.io.File
import java.util.UUID
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(manifest = Config.NONE, sdk = [35])
class PhoneBenchmarkStorageTest {
    @Test fun scopedContextPreservesIsolationAndRejectsUnknownPreferencesBeforeDelegating() {
        val app = RuntimeEnvironment.getApplication()
        val accessed = mutableListOf<String>()
        val base = object : ContextWrapper(app) {
            override fun getSharedPreferences(name: String, mode: Int): SharedPreferences {
                accessed += name
                return super.getSharedPreferences(name, mode)
            }
        }
        val directory = File(app.cacheDir, "synthetic-phone-ask-${UUID.randomUUID()}")
        val scoped = PhoneSyntheticVoiceBenchmark.ScopedContext(base, directory)
        assertSame(scoped, scoped.applicationContext)
        assertEquals(directory, scoped.cacheDir); assertEquals(directory, scoped.noBackupFilesDir)
        PhoneBenchmarkProfile.PREFERENCE_NAMES.forEach { scoped.applicationContext.getSharedPreferences(it, Context.MODE_PRIVATE) }
        assertEquals(PhoneBenchmarkProfile.PREFERENCE_NAMES.map { "${directory.name}-$it" }.toSet(), accessed.toSet())
        accessed.clear()
        for (name in listOf("assistant-policy", "assistant-runtime", "synthetic-unknown-sdk-preference")) {
            assertThrows(IllegalArgumentException::class.java) { scoped.applicationContext.getSharedPreferences(name, 0) }
        }
        assertTrue(accessed.isEmpty())
        scoped.cleanup()
    }

    @Test fun phoneFixtureRejectsPreexistingDestinationBeforeSdkCreation() {
        val app = RuntimeEnvironment.getApplication()
        val directory = File(app.cacheDir, "synthetic-phone-ask-${UUID.randomUUID()}").apply { mkdir() }
        try {
            val file = directory.resolve("fixture.wav").apply { writeText("synthetic") }
            val scoped = PhoneSyntheticVoiceBenchmark.ScopedContext(app, directory)
            assertThrows(IllegalStateException::class.java) {
                LocalFixtureSynthesis.create(android.app.Instrumentation(), scoped, "Synthetic fixture", directory)
            }
            assertEquals("synthetic", file.readText())
        } finally { directory.deleteRecursively() }
    }
}
