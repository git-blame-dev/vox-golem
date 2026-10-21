package dev.gitblame.voxgolem.assistant.runtime

import dev.gitblame.voxgolem.assistant.AssistantStoreRunner
import dev.gitblame.voxgolem.assistant.AssistantTiming
import java.io.File
import java.util.UUID
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(AssistantStoreRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class TypedBenchmarkSessionTest {
    @Test fun webPolicyIsCaseOwnedAndRetirementRemovesOnlyIsolatedStorageWithoutRequests() {
        val context = RuntimeEnvironment.getApplication()
        val preference = context.getSharedPreferences("assistant-runtime", 0)
        val previous = preference.getString("model", null)
        val executor = Executors.newSingleThreadExecutor()
        try {
            for (web in listOf(false, true)) {
                val directory = File(context.cacheDir, "synthetic-ask-${UUID.randomUUID()}")
                val session = AssistantRuntime.typedBenchmarkSession(context, directory, "custom-luna-none", web) {
                    AssistantTiming.begin(context, it)
                }
                try {
                    val ready = CountDownLatch(1)
                    val subscription = session.engine.subscribe { if (it.contextReady) ready.countDown() }
                    try { assertTrue(ready.await(3, TimeUnit.SECONDS)) } finally { subscription.close() }
                    assertEquals(web, session.engine.snapshot().webEnabled)
                    assertEquals("custom-luna-none", session.engine.snapshot().model)
                    assertFalse(session.engine.snapshot().busy)
                    assertEquals(previous, preference.getString("model", null))
                    val acknowledged = CountDownLatch(1)
                    var settled: AssistantState? = null
                    session.engine.setDraft("Synthetic fixture draft") { settled = it; acknowledged.countDown() }
                    assertTrue(acknowledged.await(3, TimeUnit.SECONDS))
                    assertEquals("Synthetic fixture draft", settled?.draft)
                    assertNull(settled?.error)
                    assertEquals(true, settled?.contextReady)
                    assertEquals(false, settled?.busy)
                    assertNull(settled?.clientRequestToken)
                    assertNull(settled?.conversation)
                } finally { executor.submit { session.close() }.get(3, TimeUnit.SECONDS) }
                assertFalse(directory.exists())
            }
        } finally { executor.shutdownNow() }
    }
}
