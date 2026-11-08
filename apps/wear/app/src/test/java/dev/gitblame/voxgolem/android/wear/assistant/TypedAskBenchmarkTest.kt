package dev.gitblame.voxgolem.android.wear.assistant

import android.app.Instrumentation
import android.content.Context
import android.content.ContextWrapper
import android.os.Bundle
import android.os.Looper
import dev.gitblame.voxgolem.assistant.runtime.AssistantState
import dev.gitblame.voxgolem.assistant.runtime.AssistantRuntime
import dev.gitblame.voxgolem.assistant.runtime.ProviderAuthReason
import dev.gitblame.voxgolem.assistant.runtime.ProviderAuthStatus
import java.io.File
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Robolectric
import org.robolectric.Shadows
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class TypedAskBenchmarkTest {
    @Test fun benchmarkUiSendRequiresOneMatchingUnexpiredPermit() {
        val client = SyntheticAssistantClient()
        val prepared = client.snapshot().copy(draft = "Synthetic armed fixture", foreground = true, model = "custom-luna-none",
            credential = ProviderAuthStatus(ProviderAuthReason.READY, "synthetic-generation"))
        val permit = TypedBenchmarkSendPermit()
        assertFalse(permit.consume(client, prepared, true))
        permit.arm(client, prepared)
        assertTrue(permit.consume(client, prepared, true))
        assertFalse(permit.consume(client, prepared, true))
        for (changed in listOf(prepared.copy(draft = "Synthetic other fixture"), prepared.copy(conversation = "synthetic-other"),
            prepared.copy(selectedHead = "synthetic-head"), prepared.copy(credential = null),
            prepared.copy(credential = ProviderAuthStatus(ProviderAuthReason.INVALID, "synthetic-generation")), prepared.copy(model = "custom-sol-high"),
            prepared.copy(webEnabled = !prepared.webEnabled), prepared.copy(error = "synthetic_error"),
            prepared.copy(foreground = false), prepared.copy(busy = true), prepared.copy(contextReady = false))) {
            permit.arm(client, prepared)
            assertFalse(permit.consume(client, changed, true))
            assertFalse(permit.consume(client, prepared, true))
        }
        permit.arm(client, prepared)
        assertFalse(permit.consume(client, prepared, false))
        permit.arm(client, prepared)
        assertFalse(permit.consume(SyntheticAssistantClient(), prepared, true))
        permit.arm(client, prepared)
        permit.clear()
        assertFalse(permit.consume(client, prepared, true))
        assertTrue(TypedAskBenchmark.permitUiSend(client)) // Ordinary clients remain unchanged.
    }

    @Test fun delayedCredentialApprovalRefreshesUntilReadyWithinBound() {
        val missing = AssistantState(credential = ProviderAuthStatus(ProviderAuthReason.MISSING, "synthetic-generation"))
        val approved = missing.copy(credential = ProviderAuthStatus(ProviderAuthReason.READY, "synthetic-generation"))
        var now = 0L
        var refreshes = 0
        val result = TypedBenchmarkReadiness.await(1000, { now }, { now += it }) { remaining ->
            assertTrue(remaining > 0)
            if (++refreshes < 3) missing else approved
        }
        assertEquals(approved, result)
        assertEquals(3, refreshes)
        assertEquals(500L, now)
        assertTrue(TypedBenchmarkReadiness.ready(requireNotNull(result)))
    }

    @Test fun missingAndRevokedApprovalCannotPassBoundedGate() {
        for (reason in ProviderAuthReason.entries.filter { it != ProviderAuthReason.READY }) {
            var now = 0L
            var refreshes = 0
            val state = AssistantState(credential = ProviderAuthStatus(reason, "synthetic-generation"))
            val result = TypedBenchmarkReadiness.await(1000, { now }, { now += it }) {
                refreshes++; state
            }
            assertEquals(1000L, now)
            assertEquals(4, refreshes)
            assertFalse(TypedBenchmarkReadiness.ready(requireNotNull(result)))
            assertEquals(reason.name, TypedBenchmarkReadiness.diagnostics(result).getString("credential_reason"))
        }
        val before = AssistantState(credential = ProviderAuthStatus(ProviderAuthReason.READY, "synthetic-generation"))
        assertTrue(TypedBenchmarkReadiness.ready(before))
        val revoked = before.copy(credential = ProviderAuthStatus(ProviderAuthReason.INVALID, "synthetic-generation"))
        assertFalse(TypedBenchmarkReadiness.ready(revoked))
    }

    @Test fun failedOrUnsettledRefreshNeverUsesStaleReadySnapshot() {
        val ready = AssistantState(credential = ProviderAuthStatus(ProviderAuthReason.READY, "synthetic-generation"))
        for (state in listOf(ready.copy(busy = true), ready.copy(contextReady = false), ready.copy(error = "synthetic raw payload"))) {
            var now = 0L
            val result = TypedBenchmarkReadiness.await(500, { now }, { now += it }) { state }
            assertFalse(TypedBenchmarkReadiness.ready(requireNotNull(result)))
        }
        val result = TypedBenchmarkReadiness.await(500, { 0L }, { fail("No polling after refresh timeout") }) { null }
        assertNull(result)
        assertEquals("refresh_timeout", TypedBenchmarkReadiness.diagnostics(result).getString("failure_reason"))
        val facts = TypedBenchmarkReadiness.diagnostics(ready.copy(error = "synthetic raw payload"))
        assertEquals("unknown", facts.getString("error_code"))
        assertEquals("engine_error", facts.getString("failure_reason"))
        assertTrue(facts.getBoolean("error_present"))
        assertFalse(facts.toString().contains("synthetic"))
        assertEquals("auth_unavailable", TypedBenchmarkReadiness.diagnostics(ready.copy(error = "auth_unavailable")).getString("error_code"))
    }

    @Test fun fixtureDraftValidationUsesUtf8BudgetWithoutTruncation() {
        val boundary = "é".repeat(16384)
        assertEquals(boundary, TypedAskBenchmark.validateDraft(boundary))
        for (invalid in listOf("", " \n", boundary + "x")) {
            assertTrue(runCatching { TypedAskBenchmark.validateDraft(invalid) }.isFailure)
        }
    }

    @Test fun draftAccessorRequiresAppliedActivityProjectionAndDoesNotSend() {
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        try {
            val activity = controller.get()
            Shadows.shadowOf(Looper.getMainLooper()).idle()
            val before = activity.typedBenchmarkDraft()
            val pending = activity.synthetic.snapshot().copy(draft = "Synthetic fixture draft")
            activity.synthetic.state = pending
            assertEquals(before, activity.typedBenchmarkDraft())
            activity.synthetic.publish(pending)
            Shadows.shadowOf(Looper.getMainLooper()).idle()
            assertEquals("Synthetic fixture draft", activity.typedBenchmarkDraft())
            assertEquals(0, activity.synthetic.sends)
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun provisionalAccessorDoesNotReadAheadToClientSnapshot() {
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        try {
            val activity = controller.get()
            Shadows.shadowOf(Looper.getMainLooper()).idle()
            assertEquals("", activity.typedBenchmarkProvisional())
            val pending = activity.synthetic.snapshot().copy(provisional = "Synthetic not-yet-presented answer")
            activity.synthetic.state = pending
            assertEquals("Synthetic not-yet-presented answer", activity.synthetic.snapshot().provisional)
            assertEquals("", activity.typedBenchmarkProvisional())
            activity.synthetic.publish(pending)
            Shadows.shadowOf(Looper.getMainLooper()).idle()
            assertEquals("Synthetic not-yet-presented answer", activity.typedBenchmarkProvisional())
        } finally { controller.pause().stop().destroy() }
    }

    private fun arguments() = Bundle().apply {
        putString("run", "00000000-0000-4000-8000-000000000001")
        putString("liveProvider", "true")
    }

    @Test fun typedArgumentsHaveFiniteConservativeDefaults() {
        assertEquals(Triple("00000000-0000-4000-8000-000000000001", 7, 1230), TypedAskBenchmark.validateArguments(arguments()))
        for (limit in listOf("1", "7")) {
            assertEquals(limit.toInt(), TypedAskBenchmark.validateArguments(arguments().apply { putString("maxTurns", limit) }).second)
        }
    }

    @Test fun rejectUnboundedOrUnapprovedArguments() {
        for ((key, value) in listOf("maxTurns" to "0", "maxTurns" to "8", "timeout" to "1801", "timeout" to "0",
            "turnTimeoutSeconds" to "9", "turnTimeoutSeconds" to "181",
            "model" to "custom-sol-high", "liveProvider" to "false", "run" to "../synthetic-other")) {
            assertTrue("$key=$value", runCatching {
                TypedAskBenchmark.validateArguments(arguments().apply { putString(key, value) })
            }.isFailure)
        }
    }

    @Test fun approvedSevenTurnLifetimeSupportsIndependentPerTurnDeadline() {
        for (turnTimeout in listOf(10, 120, 180)) {
            val lifetime = 7 * (turnTimeout + 30) + 180
            val parsed = TypedAskBenchmark.validateArguments(arguments().apply {
                putString("maxTurns", "7")
                putString("timeout", lifetime.toString())
                putString("turnTimeoutSeconds", turnTimeout.toString())
            })
            assertEquals(7, parsed.second)
            assertEquals(lifetime, parsed.third)
        }
        assertEquals(1800, TypedAskBenchmark.validateArguments(arguments().apply { putString("timeout", "1800") }).third)
    }

    @Test fun unknownModelFailsBeforeFilesystemOrProviderAccess() {
        val untouched = object : ContextWrapper(null) {
            override fun getCacheDir(): File = error("unexpected_context_access")
            override fun getApplicationContext(): Context = error("unexpected_context_access")
        }
        assertTrue(runCatching {
            AssistantRuntime.typedBenchmarkSession(untouched, File("synthetic-unused"), "custom-sol-high", true) {
                error("unexpected_timing")
            }
        }.exceptionOrNull() is IllegalArgumentException)
    }

    @Test fun absentOwnerCannotReadSubmitOrRetireAnyState() {
        val stranger = Instrumentation()
        assertTrue(runCatching { TypedAskBenchmark.status(stranger) }.isFailure)
        assertTrue(runCatching { TypedAskBenchmark.validate(stranger) }.isFailure)
        assertTrue(runCatching { TypedAskBenchmark.arm(stranger, Bundle()) }.isFailure)
        assertTrue(runCatching { TypedAskBenchmark.prepareDraft(stranger, Bundle()) }.isFailure)
        assertTrue(runCatching { TypedAskBenchmark.setupCase(stranger, Bundle()) }.isFailure)
        assertTrue(runCatching { TypedAskBenchmark.close(stranger) }.isFailure)
    }

    @Test fun canonicalAncestryDecisionRules() {
        // first turn null parent -> valid
        assertTrue(TypedAskBenchmark.ancestryValid(null, null))
        // first turn non-null parent -> invalid
        assertFalse(TypedAskBenchmark.ancestryValid(null, "some-parent-key"))

        val prev = "prev-turn-key"
        // follow-up parent == previous -> valid
        assertTrue(TypedAskBenchmark.ancestryValid(prev, prev))
        // follow-up parent != previous -> invalid
        assertFalse(TypedAskBenchmark.ancestryValid(prev, "other-parent-key"))
        // follow-up null parent -> invalid
        assertFalse(TypedAskBenchmark.ancestryValid(prev, null))
    }

    @Test fun extractParentKeyFromResponseBody() {
        assertNull(TypedAskBenchmark.extractParentKey(null))
        assertTrue(runCatching { TypedAskBenchmark.extractParentKey(org.json.JSONObject()) }.isFailure)
        assertTrue(runCatching { TypedAskBenchmark.extractParentKey(org.json.JSONObject().put("parent", "synthetic")) }.isFailure)
        assertTrue(runCatching { TypedAskBenchmark.extractParentKey(org.json.JSONObject().put("parent", 7)) }.isFailure)

        val bodyNoParent = org.json.JSONObject().put("parent", org.json.JSONObject.NULL)
        assertNull(TypedAskBenchmark.extractParentKey(bodyNoParent))

        val parentId = org.json.JSONObject().put("replica", "11111111-1111-4111-8111-111111111111").put("sequence", 1)
        val bodyWithParent = org.json.JSONObject().put("parent", parentId)
        assertEquals("11111111-1111-4111-8111-111111111111:1", TypedAskBenchmark.extractParentKey(bodyWithParent))
    }
}
