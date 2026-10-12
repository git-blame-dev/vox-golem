package dev.gitblame.voxgolem.assistant.replication

import android.app.job.JobParameters
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.util.ReflectionHelpers
import java.util.concurrent.FutureTask

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [24, 35], manifest = Config.NONE)
class ReplicationLifecycleTest {
    @Test fun jobStopCancelsOnlyItsWorkAndIgnoresLateCompletion() {
        val service = Robolectric.buildService(AssistantReplicationJobService::class.java).create().get()
        val parameters = ReflectionHelpers.newInstance(JobParameters::class.java)
        val future = FutureTask<Unit> {}
        var complete: (() -> Unit)? = null
        var finished = 0
        service.beginWork = { callback -> complete = callback; future }
        service.finishWork = { finished++ }
        assertTrue(service.onStartJob(parameters))
        assertTrue(service.onStopJob(parameters))
        assertTrue(future.isCancelled)
        complete!!.invoke()
        assertEquals(0, finished)
        service.onDestroy()
    }

    @Test fun normalJobCompletesExactlyOnceAndRejectsUnavailableWork() {
        val service = Robolectric.buildService(AssistantReplicationJobService::class.java).create().get()
        val parameters = ReflectionHelpers.newInstance(JobParameters::class.java)
        var complete: (() -> Unit)? = null
        var finished = 0
        service.beginWork = { callback -> complete = callback; FutureTask<Unit> {} }
        service.finishWork = { finished++ }
        assertTrue(service.onStartJob(parameters))
        complete!!.invoke(); complete!!.invoke()
        assertEquals(1, finished)
        service.beginWork = { null }
        assertFalse(service.onStartJob(parameters))
        service.onDestroy()
    }

}
