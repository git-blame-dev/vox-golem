package dev.gitblame.voxgolem.assistant.runtime

import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowPowerManager

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class PhoneRelayWakeLeaseTest {
    @Test fun platformLeaseAcquiresAndReleasesIdempotently() {
        val lease = PhoneRelayWakeLease(RuntimeEnvironment.getApplication(), 215_000)
        val wake = requireNotNull(ShadowPowerManager.getLatestWakeLock())
        assertTrue(wake.isHeld)
        lease.close()
        assertFalse(wake.isHeld)
        lease.close()
        assertFalse(wake.isHeld)
    }
}
