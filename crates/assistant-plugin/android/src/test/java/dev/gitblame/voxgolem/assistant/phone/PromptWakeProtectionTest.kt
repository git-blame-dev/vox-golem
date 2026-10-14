package dev.gitblame.voxgolem.assistant.phone

import android.os.Handler
import android.os.Looper
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.annotation.LooperMode
import java.time.Duration

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
@LooperMode(LooperMode.Mode.PAUSED)
class PromptWakeProtectionTest {
    @Test fun activityDestructionKeepsRequestProtectionUntilPhysicalRetirement() {
        var acquires = 0; var releases = 0
        val protection = PromptWakeProtection(Handler(Looper.getMainLooper()), { acquires++ }, { releases++ })
        protection.begin("synthetic-destroyed-window-request")
        protection.close()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(181))
        assertEquals(7, acquires); assertEquals(0, releases)
        protection.retired("synthetic-destroyed-window-request")
        assertEquals(1, releases)
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(61))
        assertEquals(7, acquires)
        assertThrows(IllegalStateException::class.java) { protection.begin("synthetic-after-destroy") }
    }
    @Test fun boundedLeasesProtectA180SecondRequestUntilExactWorkerRetirement() {
        val leases = mutableListOf<Long>()
        var releases = 0
        val protection = PromptWakeProtection(Handler(Looper.getMainLooper()), { leases += it }, { releases++ })
        protection.begin("synthetic-request-a")
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(181))
        assertEquals(7, leases.size)
        assertTrue(leases.all { it == 60_000L })
        assertEquals(0, releases)
        protection.retired("synthetic-other-request")
        assertEquals(0, releases)
        protection.retired("synthetic-request-a")
        assertEquals(1, releases)
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofMinutes(1))
        assertEquals(7, leases.size)
        protection.begin("synthetic-request-b")
        protection.retired("synthetic-request-a")
        assertEquals(1, releases)
        protection.retired("synthetic-request-b")
        assertEquals(2, releases)
    }
}
