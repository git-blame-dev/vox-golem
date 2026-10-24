package dev.gitblame.voxgolem.assistant.phone

import dev.gitblame.voxgolem.benchmark.android.runBenchmarkCleanup
import org.junit.Assert.*
import org.junit.Test

class PhoneBenchmarkCleanupTest {
    @Test fun failedSdkRetirementStillAttemptsShutdownAndFileCleanupWithoutReleasingAdmission() {
        val effects = mutableListOf<String>()
        assertThrows(IllegalStateException::class.java) {
            runBenchmarkCleanup(listOf(
                { effects += "stop"; error("Synthetic SDK teardown failure") },
                { effects += "shutdown" },
                { effects += "delete" },
            ))
            effects += "release"
        }
        assertEquals(listOf("stop", "shutdown", "delete"), effects)
    }

    @Test fun successfulRetirementReleasesAdmissionAfterEveryOwnedStage() {
        val effects = mutableListOf<String>()
        runBenchmarkCleanup(listOf({ effects += "stop" }, { effects += "shutdown" }, { effects += "delete" }))
        effects += "release"
        assertEquals(listOf("stop", "shutdown", "delete", "release"), effects)
    }
}
