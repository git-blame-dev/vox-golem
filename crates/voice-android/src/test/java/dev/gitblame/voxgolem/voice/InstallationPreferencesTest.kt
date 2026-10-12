package dev.gitblame.voxgolem.voice

import android.content.Context
import android.content.SharedPreferences
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.lang.reflect.Proxy
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException
import java.util.concurrent.atomic.AtomicInteger

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class InstallationPreferencesTest {
    @Test fun existingFileKeysDefaultsAndUnknownFieldsSurviveAcrossOwners() {
        val context = RuntimeEnvironment.getApplication()
        val raw = context.getSharedPreferences("assistant-policy", Context.MODE_PRIVATE)
        raw.edit().putString("synthetic_future_setting", "preserve").commit()
        val first = InstallationPreferences(context)
        assertEquals(InstallationSettings(), first.read())
        val selected = InstallationSettings(false, false, "en-AU", "synthetic.tts", "synthetic-au-local")
        first.write(selected)
        val second = InstallationPreferences(context)
        assertEquals(selected, second.read())
        assertEquals(selected.speechAdmission(), second.read().speechAdmission())
        assertFalse(raw.getBoolean("web_enabled", true))
        assertFalse(raw.getBoolean("speech_network_fallback", true))
        assertEquals("en-AU", raw.getString("speech_language", null))
        assertEquals("synthetic.tts", raw.getString("speech_tts_provider", null))
        assertEquals("synthetic-au-local", raw.getString("speech_tts_voice", null))
        assertEquals("preserve", raw.getString("synthetic_future_setting", null))
    }

    @Test fun invalidShapeDoesNotChangeAnyPersistedField() {
        val context = RuntimeEnvironment.getApplication()
        val storage = InstallationPreferences(context)
        val before = InstallationSettings(false, true, "en-AU", "synthetic.tts", "synthetic-local")
        storage.write(before)
        for (invalid in listOf(before.copy(speechLanguage = "en--AU"), before.copy(speechTtsVoice = "界".repeat(171)))) {
            assertThrows(IllegalArgumentException::class.java) { storage.write(invalid) }
            assertEquals(before, InstallationPreferences(context).read())
        }
    }

    @Test fun anotherOwnerCannotReadFailedCommitMemoryBeforeFullRecordRollback() {
        val raw = RuntimeEnvironment.getApplication().getSharedPreferences("assistant-policy", Context.MODE_PRIVATE)
        val before = InstallationSettings(false, false, "en-AU", "synthetic.old.tts", "synthetic-old-voice")
        InstallationPreferences(raw).write(before)
        val committedMemory = CountDownLatch(1)
        val releaseCommit = CountDownLatch(1)
        val readStarted = CountDownLatch(1)
        val commits = AtomicInteger()
        val intercepted = interceptCommits(raw) { editor ->
            val saved = editor.commit()
            if (commits.incrementAndGet() == 1) {
                committedMemory.countDown()
                check(releaseCommit.await(5, TimeUnit.SECONDS))
                false // Models a disk failure after SharedPreferences has updated its memory cache.
            } else saved
        }
        val writer = InstallationPreferences(intercepted)
        val reader = InstallationPreferences(intercepted)
        val workers = Executors.newFixedThreadPool(2)
        try {
            val mutation = workers.submit {
                assertThrows(IllegalStateException::class.java) { writer.write(InstallationSettings()) }
            }
            assertTrue(committedMemory.await(5, TimeUnit.SECONDS))
            val observed = workers.submit<InstallationSettings> { readStarted.countDown(); reader.read() }
            assertTrue(readStarted.await(5, TimeUnit.SECONDS))
            assertThrows(TimeoutException::class.java) { observed.get(100, TimeUnit.MILLISECONDS) }
            releaseCommit.countDown()
            mutation.get(5, TimeUnit.SECONDS)
            assertEquals(before, observed.get(5, TimeUnit.SECONDS))
            assertEquals(before, InstallationPreferences(raw).read())
            assertEquals(2, commits.get())
        } finally {
            releaseCommit.countDown()
            workers.shutdownNow()
            assertTrue(workers.awaitTermination(5, TimeUnit.SECONDS))
        }
    }

    private fun interceptCommits(storage: SharedPreferences, commit: (SharedPreferences.Editor) -> Boolean): SharedPreferences =
        Proxy.newProxyInstance(SharedPreferences::class.java.classLoader, arrayOf(SharedPreferences::class.java)) { _, method, args ->
            val result = method.invoke(storage, *args.orEmpty())
            if (method.name != "edit") result else {
                val editor = result as SharedPreferences.Editor
                lateinit var wrapped: SharedPreferences.Editor
                wrapped = Proxy.newProxyInstance(SharedPreferences.Editor::class.java.classLoader,
                    arrayOf(SharedPreferences.Editor::class.java)) { _, edit, fields ->
                    if (edit.name == "commit") commit(editor)
                    else edit.invoke(editor, *fields.orEmpty()).let { if (it === editor) wrapped else it }
                } as SharedPreferences.Editor
                wrapped
            }
        } as SharedPreferences
}
