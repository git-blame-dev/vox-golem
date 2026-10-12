package dev.gitblame.voxgolem.android.wear

import android.Manifest
import android.app.ActivityManager
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.database.sqlite.SQLiteDatabase
import android.media.AudioManager
import android.os.Build
import android.os.SystemClock
import android.view.InputDevice
import android.view.KeyCharacterMap
import android.view.KeyEvent
import androidx.core.content.ContextCompat
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import dev.gitblame.voxgolem.android.wear.capture.CaptureRepository
import dev.gitblame.voxgolem.android.wear.capture.CaptureService
import dev.gitblame.voxgolem.android.wear.capture.CaptureSignals
import dev.gitblame.voxgolem.assistant.recognition.RecognitionResourceLease
import dev.gitblame.voxgolem.capture.CaptureState
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.security.MessageDigest
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference

@RunWith(AndroidJUnit4::class)
class CaptureExitEmulatorTest {
    private val instrumentation get() = InstrumentationRegistry.getInstrumentation()
    private val context get() = instrumentation.targetContext
    private val audioManager get() = context.getSystemService(AudioManager::class.java)

    @Before fun requireExplicitHostDisabledSyntheticInputAndExistingPermission() {
        assertTrue("Capture exit instrumentation is emulator-only",
            Build.HARDWARE == "ranchu" || Build.HARDWARE == "goldfish")
        assertEquals("Native capture fixtures must use separate app storage and Data Layer identity",
            "dev.gitblame.voxgolem.android.uxfixtures", context.packageName)
        assertTrue("Every existing capture in this sandbox must belong to this synthetic fixture",
            fixtureOwnedIds().containsAll(captureIds()))
        // The caller must verify the named Wear AVD and run `emu avd hostmicoff` before opting in.
        // The legacy silentInput argument confirms host-input isolation, not bit-zero guest PCM.
        assertEquals("Explicit confirmation of disabled host microphone input is required",
            "true", InstrumentationRegistry.getArguments().getString("silentInput"))
        assertTrue("This fixture requires a Wear emulator",
            context.packageManager.hasSystemFeature(PackageManager.FEATURE_WATCH))
        assertEquals("Grant RECORD_AUDIO before running; the fixture does not change permissions",
            PackageManager.PERMISSION_GRANTED,
            ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO))
        assertTrue("No unrelated native recording may be active", audioManager.activeRecordingConfigurations.isEmpty())
        assertNull("No unrelated microphone owner may be active", RecognitionResourceLease.process.current())
        assertTrue("No existing capture may be interrupted by this fixture", CaptureRepository(context).active() == null)
        await("An idle CaptureService from the preceding activity close must retire before the fixture") {
            !captureServiceRunning()
        }
    }

    @Test(timeout = 90_000) fun duplicateNativeBackSavesExactlyOneDurableWav() {
        verifyExit { _ ->
            nativeKey(KeyEvent.KEYCODE_BACK)
            nativeKey(KeyEvent.KEYCODE_BACK)
        }
    }

    @Test(timeout = 90_000) fun nativeHomeSavesExactlyOneDurableWav() {
        verifyExit { _ -> nativeKey(KeyEvent.KEYCODE_HOME) }
    }

    @Test(timeout = 90_000) fun activityFinishSavesExactlyOneDurableWav() {
        verifyExit { scenario -> scenario.close() }
    }

    private fun verifyExit(exit: (ActivityScenario<CaptureActivity>) -> Unit) {
        val repository = CaptureRepository(context)
        val baselineIds = captureIds()
        val ownedId = AtomicReference<String?>()
        val savedSignals = AtomicInteger()
        val receiver = object : BroadcastReceiver() {
            override fun onReceive(context: Context?, intent: Intent?) {
                val id = ownedId.get() ?: return
                if (intent?.action == CaptureSignals.SIGNAL_ACTION &&
                    intent.`package` == this@CaptureExitEmulatorTest.context.packageName &&
                    intent.getStringExtra("captureId") == id && intent.getStringExtra("state") == "saved") {
                    savedSignals.incrementAndGet()
                }
            }
        }
        ContextCompat.registerReceiver(context, receiver, IntentFilter(CaptureSignals.SIGNAL_ACTION),
            ContextCompat.RECEIVER_NOT_EXPORTED)
        var testFailure: Throwable? = null
        try {
            // ACTION_VIEW bypasses launcher calibration without changing motion preferences.
            ActivityScenario.launch<CaptureActivity>(Intent(context, CaptureActivity::class.java)
                .setAction(Intent.ACTION_VIEW).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)).use { scenario ->
                scenario.onActivity { it.requestStart() }
                await("A new native AudioRecord session must reach RECORDING with a nonempty PCM spool") {
                    val active = repository.active()
                    if (active == null || active.captureState != CaptureState.RECORDING || active.captureId in baselineIds) {
                        false
                    } else {
                        if (ownedId.compareAndSet(null, active.captureId)) rememberFixtureCapture(active.captureId)
                        val spool = active.localAudioPath?.let(::File)
                        spool?.isFile == true && spool.length() > 0 &&
                            audioManager.activeRecordingConfigurations.isNotEmpty() &&
                            RecognitionResourceLease.process.current()?.kind == RecognitionResourceLease.Kind.WATCH_NOTE_AUDIO
                    }
                }
                val id = requireNotNull(ownedId.get()) { "The fixture must own a new recording" }
                assertTrue("Only one new recording may be created", captureIds() - baselineIds == setOf(id))
                exit(scenario)
                await("Native exit must commit SAVED and deliver exactly one owned save signal") {
                    repository.get(id)?.captureState == CaptureState.SAVED && savedSignals.get() == 1
                }
                awaitRetirement(repository)
                val firstProof = durableWavProof(id, baselineIds)

                // Back/Home may leave an activity alive; closing it exercises an additional onStop safely.
                scenario.close()
                awaitRetirement(repository)
                val deadline = SystemClock.elapsedRealtime() + 1_500
                do {
                    assertEquals("Duplicate exit must not emit another owned save signal", 1, savedSignals.get())
                    assertTrue("Duplicate exit must not create another capture", captureIds() - baselineIds == setOf(id))
                    assertTrue("Duplicate exit must not restart recording", repository.active() == null)
                    assertTrue("The native recorder must remain stopped", audioManager.activeRecordingConfigurations.isEmpty())
                    assertNull("The microphone lease must remain released", RecognitionResourceLease.process.current())
                    assertFalse("CaptureService must remain retired", captureServiceRunning())
                    SystemClock.sleep(100)
                } while (SystemClock.elapsedRealtime() < deadline)
                assertTrue("Reopening the repository must preserve identical finalized WAV metadata and bytes",
                    firstProof == durableWavProof(id, baselineIds))
                instrumentation.waitForIdleSync()
                assertEquals("Exactly one owned saved broadcast must survive duplicate lifecycle exit", 1, savedSignals.get())
            }
        } catch (failure: Throwable) {
            testFailure = failure
            throw failure
        } finally {
            // ActivityScenario.use closes even on assertion failure; its onStop can enqueue another STOP.
            try {
                awaitRetirement(repository)
            } catch (cleanupFailure: Throwable) {
                val originalFailure = testFailure
                if (originalFailure != null) originalFailure.addSuppressed(cleanupFailure)
                else throw cleanupFailure
            } finally {
                context.unregisterReceiver(receiver)
            }
        }
    }

    private fun awaitRetirement(repository: CaptureRepository) {
        var idleSince: Long? = null
        await("Exit must retire the real recorder, microphone lease and CaptureService") {
            val idle = repository.active() == null && audioManager.activeRecordingConfigurations.isEmpty() &&
                RecognitionResourceLease.process.current() == null && !captureServiceRunning()
            val now = SystemClock.elapsedRealtime()
            if (!idle) idleSince = null
            else if (idleSince == null) idleSince = now
            idle && now - (idleSince ?: now) >= 500
        }
    }

    private data class WavProof(val byteLength: Long, val hash: String, val durableFrames: Long)

    private fun durableWavProof(id: String, baselineIds: Set<String>): WavProof {
        val reopened = CaptureRepository(context)
        assertTrue("Exactly one session delta must survive repository reopen", captureIds() - baselineIds == setOf(id))
        assertTrue("No active capture may survive repository reopen", reopened.active() == null)
        val saved = requireNotNull(reopened.get(id)) { "Owned saved metadata must survive repository reopen" }
        assertEquals("Native exit must durably save, not interrupt, the recording", CaptureState.SAVED, saved.captureState)
        assertTrue("Native exit must use the lifecycle manual-stop path", saved.stopReason == "manual_stop")
        val wav = reopened.audio(id)
        assertTrue("Final audio.wav must exist", wav.isFile && wav.length() > 44)
        assertTrue("Owned metadata must reference its finalized WAV", saved.localAudioPath == wav.absolutePath)
        assertTrue("Final byte length must match the actual file", saved.finalizedByteLength == wav.length())
        val files = requireNotNull(wav.parentFile?.listFiles()) { "Owned capture directory must be readable" }
        assertEquals("The owned session must have exactly one WAV", 1, files.count { it.isFile && it.extension == "wav" })
        assertFalse("Successful finalization must retire the owned PCM spool", files.any { it.name == "audio.pcm.part" })

        val header = ByteArray(44)
        val digest = MessageDigest.getInstance("SHA-256")
        var streamedByteLength = header.size.toLong()
        wav.inputStream().buffered().use { input ->
            var offset = 0
            while (offset < header.size) {
                val count = input.read(header, offset, header.size - offset)
                assertTrue("Finalized WAV header must be complete", count > 0)
                offset += count
            }
            digest.update(header)
            // Ranchu can generate nonzero sine input; disabled host input does not imply silence.
            // android.googlesource.com/device/generic/goldfish/+/android16-release/hals/audio/device_port_source.cpp
            val buffer = ByteArray(16 * 1024)
            while (true) {
                val count = input.read(buffer)
                if (count < 0) break
                streamedByteLength += count
                digest.update(buffer, 0, count)
            }
        }
        assertEquals("The complete finalized WAV must be readable", wav.length(), streamedByteLength)
        fun marker(offset: Int) = String(header, offset, 4, Charsets.US_ASCII)
        val fields = ByteBuffer.wrap(header).order(ByteOrder.LITTLE_ENDIAN)
        fun unsignedInt(offset: Int) = fields.getInt(offset).toLong() and 0xffffffffL
        assertEquals("WAV must have a RIFF header", "RIFF", marker(0))
        assertEquals("WAV must have a WAVE marker", "WAVE", marker(8))
        assertEquals("WAV must have a format chunk", "fmt ", marker(12))
        assertEquals("WAV must have a data chunk", "data", marker(36))
        assertEquals("RIFF size must be finalized", wav.length() - 8, unsignedInt(4))
        assertEquals("Data size must be finalized", wav.length() - 44, unsignedInt(40))
        assertEquals("Capture must store linear PCM", 1, fields.getShort(20).toInt())
        assertEquals("Channel count must match metadata", saved.format.channels, fields.getShort(22).toInt())
        assertEquals("Sample rate must match metadata", saved.format.sampleRate.toLong(), unsignedInt(24))
        assertEquals("Sample width must match metadata", saved.format.bitsPerSample, fields.getShort(34).toInt())
        val frameBytes = saved.format.channels * saved.format.bitsPerSample / 8
        assertEquals("WAV block alignment must match the PCM format", frameBytes, fields.getShort(32).toInt())
        assertEquals("WAV byte rate must match the PCM format", saved.format.sampleRate.toLong() * frameBytes, unsignedInt(28))
        assertEquals("PCM payload must contain whole frames", 0L, (wav.length() - 44) % frameBytes)
        assertEquals("Durable frame count must match the WAV payload", (wav.length() - 44) / frameBytes, saved.durableFrames)
        assertEquals("All observed frames must be finalized", saved.observedFrames, saved.durableFrames)
        val hash = digest.digest().joinToString("") { "%02x".format(it) }
        assertTrue("Persisted SHA-256 must match finalized bytes", saved.sha256 == hash)
        return WavProof(wav.length(), hash, saved.durableFrames)
    }

    private fun captureIds(): Set<String> {
        // Read only identities: historical transcript bodies and audio are never opened by the fixture.
        val databaseFile = context.getDatabasePath("captures.db")
        if (!databaseFile.exists()) return emptySet()
        SQLiteDatabase.openDatabase(databaseFile.absolutePath, null,
            SQLiteDatabase.OPEN_READONLY).use { database ->
            database.rawQuery("SELECT capture_id FROM captures", null).use { cursor ->
                return buildSet { while (cursor.moveToNext()) add(cursor.getString(0)) }
            }
        }
    }

    private fun fixtureOwnedIds(): Set<String> = context.getSharedPreferences("synthetic-capture-exit", Context.MODE_PRIVATE)
        .getStringSet("owned-captures", emptySet()).orEmpty().toSet()

    private fun rememberFixtureCapture(id: String) {
        assertTrue("Synthetic capture ownership must be saved before native exit",
            context.getSharedPreferences("synthetic-capture-exit", Context.MODE_PRIVATE).edit()
                .putStringSet("owned-captures", fixtureOwnedIds() + id).commit())
    }

    @Suppress("DEPRECATION")
    private fun captureServiceRunning(): Boolean = context.getSystemService(ActivityManager::class.java)
        .getRunningServices(Int.MAX_VALUE).any { it.service.className == CaptureService::class.java.name }

    private fun nativeKey(code: Int) {
        val downTime = SystemClock.uptimeMillis()
        for (action in listOf(KeyEvent.ACTION_DOWN, KeyEvent.ACTION_UP)) {
            val event = KeyEvent(downTime, SystemClock.uptimeMillis(), action, code, 0, 0,
                KeyCharacterMap.VIRTUAL_KEYBOARD, 0, KeyEvent.FLAG_FROM_SYSTEM, InputDevice.SOURCE_KEYBOARD)
            assertTrue("UiAutomation must inject the native exit key", instrumentation.uiAutomation.injectInputEvent(event, true))
        }
    }

    private fun await(message: String, condition: () -> Boolean) {
        val deadline = SystemClock.elapsedRealtime() + 20_000
        while (!condition()) {
            assertTrue(message, SystemClock.elapsedRealtime() < deadline)
            SystemClock.sleep(50)
        }
    }
}
