package dev.gitblame.voxgolem.assistant.runtime

import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.RuntimeEnvironment
import org.robolectric.shadows.ShadowLog
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.FilterOutputStream
import java.io.InputStream
import java.io.OutputStream
import java.io.PipedInputStream
import java.io.PipedOutputStream
import java.io.IOException
import java.util.Collections
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.Future
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicBoolean

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class PhoneRelaySessionTest {
    private val requestId = "synthetic-local-collision"
    private fun request(id: String = requestId, prompt: String = "Synthetic question") = JSONObject()
        .put("requestId", id).put("model", "gemini-nano").put("prompt", prompt).put("webEnabled", false).toString()
    private fun await(latch: CountDownLatch) = assertTrue("Timed out awaiting synthetic event", latch.await(5, TimeUnit.SECONDS))

    internal class Callback : ProviderDeltaCallback {
        val ready = CountDownLatch(1)
        val deltas = Collections.synchronizedList(mutableListOf<String>())
        val statuses = Collections.synchronizedList(mutableListOf<String>())
        val dispatched = AtomicInteger()
        val first = Collections.synchronizedList(mutableListOf<Long>())
        override fun onReady() { ready.countDown() }
        override fun onDelta(text: String) { deltas += text }
        override fun onStatus(status: String) { statuses += status }
        override fun onRequestDispatched() { dispatched.incrementAndGet() }
        override fun onFirstText(milliseconds: Long) { first += milliseconds }
    }

    internal class Native : ProviderNativeCalls {
        val calls = AtomicInteger()
        val entered = CountDownLatch(1)
        val cancellation = CountDownLatch(1)
        val retirementRegistered = CountDownLatch(1)
        val retirements = LinkedBlockingQueue<() -> Unit>()
        val allowReturn = CountDownLatch(1)
        @Volatile var retire: (() -> Unit)? = null
        @Volatile var id = ""
        @Volatile var json = ""
        var readinessGate: CountDownLatch? = null
        var run: (ProviderDeltaCallback) -> String = { callback ->
            callback.onRequestDispatched(); callback.onRequestDispatched()
            callback.onStatus("Searching"); callback.onFirstText(7)
            callback.onDelta("Synthetic answer")
            """{"status":"complete","text":"Synthetic answer"}"""
        }
        override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String {
            calls.incrementAndGet(); id = requestId; json = requestJson
            entered.countDown(); readinessGate?.await(5, TimeUnit.SECONDS); callback.onReady()
            return run(callback)
        }
        override fun cancel(requestId: String) { assertEquals(id, requestId); cancellation.countDown(); allowReturn.countDown() }
        override fun whenRetired(requestId: String, retired: () -> Unit) { retire = retired; retirements.add(retired); retirementRegistered.countDown() }
    }

    internal class Connection(
        override val nodeId: String, override val path: String,
        private val incoming: InputStream, private val outgoing: OutputStream,
    ) : PhoneRelayConnection {
        val closed = AtomicBoolean()
        override fun input() = incoming
        override fun output() = outgoing
        override fun close() { closed.set(true); runCatching { incoming.close() }; runCatching { outgoing.close() } }
    }

    internal inner class Fixture(val native: Native = Native(), cleanupMs: Long = 3_000) : AutoCloseable {
        val selected = CountDownLatch(1)
        var discoveryGate: CountDownLatch? = null
        var watchAllowed = true
        var phoneAllowed = true
        var watchNode = "synthetic-phone"
        var phoneSource = "synthetic-watch"
        var failPhoneOutput = false
        var phoneOutputGate: CountDownLatch? = null
        val phoneWriteEntered = CountDownLatch(1)
        val secondPhoneFrame = CountDownLatch(1)
        val phoneFrames = AtomicInteger()
        var routeApproval: (() -> Boolean)? = null
        var leaseFailure = false
        var leaseGate: CountDownLatch? = null
        val leaseEntered = CountDownLatch(1)
        var executionLost: (() -> Unit)? = null
        val leaseAcquired = AtomicInteger()
        val leaseReleased = AtomicInteger()
        val leaseClosed = CountDownLatch(1)
        val approved = AtomicBoolean(true)
        val opens = AtomicInteger()
        var phoneProvider: ProviderNativeCalls = native
        @Volatile var incoming: Connection? = null
        private val calls = Executors.newCachedThreadPool()
        private val connections = Collections.synchronizedList(mutableListOf<Connection>())
        private val phonePlatform = object : PhoneRelayPlatform {
            override val watch = false
            override fun route(expectedNode: String?) = if (phoneAllowed && expectedNode == "synthetic-watch")
                PhoneRelayRoute("synthetic-watch") { approved.get() && (routeApproval?.invoke() ?: true) } else null
            override fun open(route: PhoneRelayRoute, path: String): PhoneRelayConnection = error("Not a watch")
            override fun acquireExecution(timeoutMs: Long, cancel: () -> Unit): AutoCloseable {
                assertEquals(200_000L + cleanupMs, timeoutMs)
                executionLost = cancel
                leaseEntered.countDown()
                leaseGate?.await(5, TimeUnit.SECONDS)
                if (leaseFailure) { cancel(); error("SYNTHETIC_PRIVATE_WAKE_FAILURE") }
                leaseAcquired.incrementAndGet()
                return AutoCloseable { leaseReleased.incrementAndGet(); leaseClosed.countDown() }
            }
        }
        val phone = PhoneRelayRuntime(phonePlatform, cleanupMs = cleanupMs) { phoneProvider }
        private val watchPlatform = object : PhoneRelayPlatform {
            override val watch = true
            override fun route(expectedNode: String?): PhoneRelayRoute? {
                selected.countDown()
                discoveryGate?.await(5, TimeUnit.SECONDS)
                return if (watchAllowed) PhoneRelayRoute("synthetic-phone") { routeApproval?.invoke() ?: true } else null
            }
            override fun open(route: PhoneRelayRoute, path: String): PhoneRelayConnection {
                opens.incrementAndGet()
                val watchInput = PipedInputStream(4 * 1024 * 1024)
                val phoneOutput: OutputStream = PipedOutputStream(watchInput).let { output ->
                    object : FilterOutputStream(output) {
                        override fun write(value: Int) {
                            if (failPhoneOutput) throw IOException("SYNTHETIC_SECRET_ENDPOINT")
                            phoneWriteEntered.countDown()
                            phoneOutputGate?.await(5, TimeUnit.SECONDS)
                            output.write(value)
                        }
                        override fun write(bytes: ByteArray, off: Int, len: Int) {
                            if (failPhoneOutput) throw IOException("SYNTHETIC_SECRET_ENDPOINT")
                            if (len == 4 && phoneFrames.incrementAndGet() == 2) secondPhoneFrame.countDown()
                            phoneWriteEntered.countDown()
                            phoneOutputGate?.await(5, TimeUnit.SECONDS)
                            output.write(bytes, off, len)
                        }
                        override fun close() { phoneOutputGate?.countDown(); output.close() }
                    }
                }
                val phoneInput = PipedInputStream(4 * 1024 * 1024)
                val watchOutput = PipedOutputStream(phoneInput)
                val local = Connection(watchNode, path, watchInput, watchOutput)
                val remote = Connection(phoneSource, path, phoneInput, phoneOutput)
                incoming = remote
                connections += local; connections += remote
                phone.accept(remote)
                return local
            }
            override fun acquireExecution(timeoutMs: Long, cancel: () -> Unit): AutoCloseable = error("Watch never executes provider")
        }
        val watch = PhoneRelayRuntime(watchPlatform, cleanupMs = cleanupMs) { error("Must execute on phone") }
        fun start(callback: ProviderDeltaCallback = Callback(), id: String = requestId, json: String = request(id)): Future<String> =
            calls.submit<String> { watch.respond(id, json, callback) }
        fun retireAndAdmitNext(id: String = requestId) {
            await(native.retirementRegistered)
            val retired = CountDownLatch(1)
            watch.whenRetired(id) { retired.countDown() }
            assertEquals(1L, retired.count)
            requireNotNull(native.retirements.poll(5, TimeUnit.SECONDS)).invoke()
            await(retired)
            await(leaseClosed)
            val nextId = "synthetic-next-admission"
            native.run = { """{"status":"complete","text":"Synthetic next"}""" }
            assertEquals("complete", JSONObject(start(id = nextId).get(5, TimeUnit.SECONDS)).getString("status"))
            val nextRetired = CountDownLatch(1)
            watch.whenRetired(nextId) { nextRetired.countDown() }
            requireNotNull(native.retirements.poll(5, TimeUnit.SECONDS)).invoke()
            await(nextRetired)
            assertEquals(2, native.calls.get())
        }
        override fun close() {
            native.allowReturn.countDown()
            native.readinessGate?.countDown()
            discoveryGate?.countDown()
            phoneOutputGate?.countDown()
            leaseGate?.countDown()
            watch.close(); phone.close()
            connections.forEach { it.close() }
            calls.shutdownNow()
        }
    }

    @Test fun statusDoesNotConsumeExactDeltaByteBudgetAndNextAdmissionSucceeds() {
        Fixture().use { fixture ->
            val text = "x".repeat(AssistantRequestWorker.MAX_CALLBACK_BYTES)
            fixture.native.run = { callback ->
                callback.onStatus("Searching"); callback.onDelta(text)
                JSONObject().put("status", "complete").put("text", text).toString()
            }
            val callback = Callback()
            val result = JSONObject(fixture.start(callback).get(5, TimeUnit.SECONDS))
            assertEquals("complete", result.getString("status"))
            assertEquals(text, result.getString("text"))
            assertEquals(listOf("Searching"), callback.statuses)
            assertEquals(listOf(text), callback.deltas)
            fixture.retireAndAdmitNext()
        }
    }

    @Test fun oversizedEncodedOutcomeIsOneBoundedErrorAndWaitsForActualRetirement() {
        assertOversizedOutcome(("\u0001" + "\\".repeat(3)).repeat(65_520), rawOversized = false)
    }

    @Test fun oversizedRawOutcomeIsOneBoundedErrorAndWaitsForActualRetirement() {
        assertOversizedOutcome("\u0001".repeat(262_080), rawOversized = true)
    }

    private fun assertOversizedOutcome(text: String, rawOversized: Boolean) {
        Fixture().use { fixture ->
            val footer = "\n\nSources:\nhttps://example.invalid/synthetic\n"
            assertTrue((text + footer).toByteArray().size <= AssistantRequestWorker.MAX_CALLBACK_BYTES)
            val raw = JSONObject().put("status", "complete").put("text", text + footer).put("speechText", text).toString()
            assertEquals(rawOversized, raw.toByteArray().size > PhoneRelayWire.MAX_FRAME_BYTES)
            assertTrue(JSONObject().put("outcomeJson", raw).toString().toByteArray().size > PhoneRelayWire.MAX_FRAME_BYTES)
            fixture.native.run = { callback -> callback.onDelta(text); callback.onDelta(footer); raw }
            val result = JSONObject(fixture.start().get(5, TimeUnit.SECONDS))
            assertEquals("error", result.getString("status"))
            assertEquals("too_large", result.getString("code"))
            assertFalse(result.has("text"))
            assertEquals("relay_busy", JSONObject(fixture.watch.respond("synthetic-blocked", request("synthetic-blocked"), Callback())).getString("code"))
            fixture.retireAndAdmitNext()
        }
    }

    @Test fun queuedFramesAreRecheckedAfterBlockedInflightWriteIsRevoked() {
        Fixture(cleanupMs = 60_000).use { fixture ->
            fixture.phoneOutputGate = CountDownLatch(1)
            val queued = CountDownLatch(1)
            fixture.native.run = { callback ->
                callback.onDelta("Synthetic first")
                await(fixture.phoneWriteEntered)
                callback.onDelta("Synthetic queued second")
                queued.countDown()
                fixture.native.allowReturn.await(5, TimeUnit.SECONDS)
                """{"status":"cancelled"}"""
            }
            val response = fixture.start()
            await(queued)
            fixture.approved.set(false)
            fixture.phoneOutputGate!!.countDown()
            assertFalse("Revoked queued frame began writing", fixture.secondPhoneFrame.await(200, TimeUnit.MILLISECONDS))
            assertEquals("error", JSONObject(response.get(5, TimeUnit.SECONDS)).getString("status"))
            await(fixture.native.cancellation)
            assertEquals(1, fixture.phoneFrames.get())
            assertEquals(1, fixture.native.calls.get())
            assertEquals("relay_busy", JSONObject(fixture.watch.respond("synthetic-blocked", request("synthetic-blocked"), Callback())).getString("code"))
        }
    }

    @Test fun productionDuplexPathPreservesLargeUnicodeEscapedOutcomeAndPhysicalRetirement() {
        Fixture().use { fixture ->
            val text = "Synthetic 🌍 \"answer\"\n".repeat(7_000)
            val footer = "\n\nSources:\nhttps://example.invalid/synthetic\n"
            fixture.native.run = { callback ->
                callback.onRequestDispatched(); callback.onRequestDispatched()
                callback.onStatus("Searching"); callback.onFirstText(7)
                callback.onDelta(text); callback.onDelta(footer)
                JSONObject().put("status", "complete").put("text", text + footer).put("speechText", text).toString()
            }
            val callback = Callback()
            val result = JSONObject(fixture.start(callback, json = request(prompt = "q".repeat(110_000))).get(5, TimeUnit.SECONDS))
            assertEquals(text + footer, result.getString("text"))
            assertEquals(text, result.getString("speechText"))
            assertEquals(listOf(text, footer), callback.deltas)
            assertEquals(listOf("Searching"), callback.statuses)
            assertEquals(listOf(7L), callback.first)
            assertEquals(1, callback.dispatched.get())
            assertNotEquals(requestId, fixture.native.id)
            assertTrue(fixture.native.id.startsWith("phone-relay-local-"))
            assertEquals(fixture.native.id, JSONObject(fixture.native.json).getString("requestId"))
            val retired = CountDownLatch(1)
            PhoneRelayProvider(fixture.watch).whenRetired(requestId) { retired.countDown() }
            await(fixture.native.retirementRegistered)
            assertEquals(1, fixture.leaseAcquired.get())
            assertEquals(0, fixture.leaseReleased.get())
            assertEquals(1L, retired.count)
            assertEquals("relay_busy", JSONObject(fixture.watch.respond("synthetic-next", request("synthetic-next"), Callback())).getString("code"))
            fixture.native.retire!!()
            await(retired)
            await(fixture.leaseClosed)
            assertEquals(1, fixture.leaseReleased.get())
        }
    }

    @Test fun localReadyCanStopBeforeDiscoveryAndDoesNotStartLateGeneration() {
        Fixture().use { fixture ->
            fixture.discoveryGate = CountDownLatch(1)
            val callback = Callback()
            val response = fixture.start(callback)
            await(callback.ready); await(fixture.selected)
            fixture.watch.cancel(requestId)
            assertEquals("cancelled", JSONObject(response.get(5, TimeUnit.SECONDS)).getString("status"))
            val retired = CountDownLatch(1)
            fixture.watch.whenRetired(requestId) { retired.countDown() }
            fixture.discoveryGate!!.countDown()
            await(retired)
            assertEquals(0, fixture.opens.get())
            assertEquals(0, fixture.native.calls.get())
        }
    }

    @Test fun wrongRouteAndUnauthorizedFrameworkSourceNeverExecuteProvider() {
        for (kind in 0..2) Fixture().use { fixture ->
            when (kind) {
                0 -> fixture.watchAllowed = false
                1 -> fixture.watchNode = "synthetic-unselected-phone"
                2 -> fixture.phoneSource = "synthetic-unselected-watch"
            }
            assertEquals("error", JSONObject(fixture.start().get(5, TimeUnit.SECONDS)).getString("status"))
            assertEquals(0, fixture.native.calls.get())
        }
    }

    @Test fun cancellationRetainsOneQuarantineUntilActualRemoteRetirementAndUnknownIdsCannotCancel() {
        Fixture(cleanupMs = 60_000).use { fixture ->
            fixture.native.run = { fixture.native.allowReturn.await(5, TimeUnit.SECONDS); """{"status":"cancelled"}""" }
            val response = fixture.start()
            await(fixture.native.entered)
            repeat(500) { fixture.watch.cancel("synthetic-unknown-$it") }
            assertEquals(1L, fixture.native.cancellation.count)
            // Duplicate local IDs cannot replace the owned Session or its cancellation target.
            assertEquals("relay_busy", JSONObject(fixture.watch.respond(requestId, request(), Callback())).getString("code"))
            fixture.watch.cancel(requestId)
            assertEquals("cancelled", JSONObject(response.get(5, TimeUnit.SECONDS)).getString("status"))
            await(fixture.native.cancellation); await(fixture.native.retirementRegistered)
            val retired = CountDownLatch(1)
            fixture.watch.whenRetired(requestId) { retired.countDown() }
            assertEquals(1L, retired.count)
            assertEquals(1, fixture.native.calls.get())
            fixture.native.retire!!()
            await(retired)
        }
    }

    @Test fun lostRetirementAndTerminalSendFailureNeverSynthesizeRetiredOrLeakSecrets() {
        ShadowLog.clear()
        for (sendFailure in listOf(false, true)) Fixture(cleanupMs = 100).use { fixture ->
            fixture.failPhoneOutput = sendFailure
            if (sendFailure) fixture.native.run = { """{"status":"complete","text":"Synthetic terminal only"}""" }
            val result = fixture.start().get(5, TimeUnit.SECONDS)
            assertFalse(result.contains("SYNTHETIC_SECRET_ENDPOINT"))
            await(fixture.native.retirementRegistered)
            val retired = CountDownLatch(1)
            fixture.watch.whenRetired(requestId) { retired.countDown() }
            // An outcome is not a retirement confirmation, including after the finite cleanup deadline.
            assertFalse(retired.await(250, TimeUnit.MILLISECONDS))
            assertEquals("relay_busy", JSONObject(fixture.watch.respond("synthetic-next", request("synthetic-next"), Callback())).getString("code"))
            fixture.native.retire!!()
            assertFalse(retired.await(100, TimeUnit.MILLISECONDS))
            await(fixture.leaseClosed)
            assertEquals(1, fixture.leaseReleased.get())
        }
        assertTrue(ShadowLog.getLogsForTag("PhoneRelay").all { !it.msg.contains("SYNTHETIC_SECRET_ENDPOINT") && it.throwable == null })
    }

    @Test fun streamingSendFailureCancelsOnlyOwnedWorkAndQuarantinesWatch() {
        Fixture().use { fixture ->
            fixture.failPhoneOutput = true
            fixture.native.run = { callback ->
                callback.onDelta("Synthetic delta")
                fixture.native.allowReturn.await(5, TimeUnit.SECONDS)
                """{"status":"cancelled"}"""
            }
            val result = fixture.start().get(5, TimeUnit.SECONDS)
            assertEquals("error", JSONObject(result).getString("status"))
            await(fixture.native.cancellation)
            assertNotEquals(requestId, fixture.native.id)
            assertEquals("relay_busy", JSONObject(fixture.watch.respond("synthetic-next", request("synthetic-next"), Callback())).getString("code"))
        }
    }

    @Test fun blockedWriterFloodFailsAtFixedOutboxBoundsCancelsWorkAndRetainsWakeUntilRetired() {
        Fixture().use { fixture ->
            fixture.phoneOutputGate = CountDownLatch(1)
            fixture.native.run = { callback ->
                repeat(500) { callback.onDelta("synthetic-delta") }
                fixture.native.allowReturn.await(5, TimeUnit.SECONDS)
                """{"status":"cancelled"}"""
            }
            assertEquals("error", JSONObject(fixture.start().get(5, TimeUnit.SECONDS)).getString("status"))
            await(fixture.native.cancellation); await(fixture.native.retirementRegistered)
            assertEquals(1, fixture.leaseAcquired.get())
            assertEquals(0, fixture.leaseReleased.get())
            fixture.native.retire!!()
            await(fixture.leaseClosed)
            assertEquals(1, fixture.leaseReleased.get())
        }
    }

    @Test fun foregroundAdmissionDenialConfirmsNonExecutionAndAllowsCleanRetry() {
        Fixture().use { fixture ->
            fixture.leaseFailure = true
            val outcome = fixture.start().get(5, TimeUnit.SECONDS)
            assertEquals("error", JSONObject(outcome).getString("status"))
            assertEquals("unavailable", JSONObject(outcome).getString("code"))
            assertFalse(outcome.contains("SYNTHETIC_PRIVATE_WAKE_FAILURE"))
            val retired = CountDownLatch(1)
            fixture.watch.whenRetired(requestId) { retired.countDown() }
            await(retired)
            assertEquals(0, fixture.native.calls.get())
            assertEquals(0, fixture.leaseAcquired.get())
            assertEquals(1L, fixture.native.retirementRegistered.count)
            fixture.leaseFailure = false
            val nextId = "synthetic-after-denial"
            assertEquals("complete", JSONObject(fixture.start(id = nextId).get(5, TimeUnit.SECONDS)).getString("status"))
            await(fixture.native.retirementRegistered)
            val nextRetired = CountDownLatch(1)
            fixture.watch.whenRetired(nextId) { nextRetired.countDown() }
            fixture.native.retire!!()
            await(nextRetired)
        }
    }

    @Test fun denialProofSendFailureStillQuarantinesUnknownRemoteRetirement() {
        Fixture().use { fixture ->
            fixture.leaseFailure = true
            fixture.failPhoneOutput = true
            assertEquals("error", JSONObject(fixture.start().get(5, TimeUnit.SECONDS)).getString("status"))
            val retired = CountDownLatch(1)
            fixture.watch.whenRetired(requestId) { retired.countDown() }
            assertFalse(retired.await(100, TimeUnit.MILLISECONDS))
            assertEquals(0, fixture.native.calls.get())
            assertEquals(1L, fixture.native.retirementRegistered.count)
            assertEquals("relay_busy", JSONObject(fixture.watch.respond("synthetic-next", request("synthetic-next"), Callback())).getString("code"))
        }
    }

    @Test fun stopDuringForegroundPromotionDoesNotStartLateGeneration() {
        Fixture().use { fixture ->
            fixture.leaseGate = CountDownLatch(1)
            val response = fixture.start()
            await(fixture.leaseEntered)
            fixture.watch.cancel(requestId)
            assertEquals("cancelled", JSONObject(response.get(5, TimeUnit.SECONDS)).getString("status"))
            assertFalse(fixture.native.entered.await(100, TimeUnit.MILLISECONDS))
            val retired = CountDownLatch(1)
            fixture.watch.whenRetired(requestId) { retired.countDown() }
            fixture.leaseGate!!.countDown()
            await(retired)
            assertEquals(0, fixture.native.calls.get())
            assertEquals(1L, fixture.native.retirementRegistered.count)
        }
    }

    @Test fun serviceLossAfterRetirementCannotCloseChannelBeforeTerminalFlushAndAck() {
        Fixture().use { fixture ->
            fixture.phoneOutputGate = CountDownLatch(1)
            val response = fixture.start()
            await(fixture.native.retirementRegistered)
            fixture.native.retire!!()
            requireNotNull(fixture.executionLost).invoke()
            fixture.phoneOutputGate!!.countDown()
            assertEquals("complete", JSONObject(response.get(5, TimeUnit.SECONDS)).getString("status"))
            val retired = CountDownLatch(1)
            fixture.watch.whenRetired(requestId) { retired.countDown() }
            await(retired)
            await(fixture.leaseClosed)
            assertEquals(1, fixture.leaseReleased.get())
        }
    }

    @Test fun duplicateFrameworkDeliveryAndSeparateChannelsCannotReplaceAnOwnedRequest() {
        Fixture().use { fixture ->
            fixture.native.run = { fixture.native.allowReturn.await(5, TimeUnit.SECONDS); """{"status":"cancelled"}""" }
            val response = fixture.start()
            await(fixture.native.entered)
            val incoming = requireNotNull(fixture.incoming)
            repeat(100) { fixture.phone.accept(incoming) }
            assertFalse(incoming.closed.get())
            val duplicate = Connection(incoming.nodeId, incoming.path, ByteArrayInputStream(byteArrayOf()), ByteArrayOutputStream())
            fixture.phone.accept(duplicate)
            assertTrue(duplicate.closed.get())
            assertEquals(1, fixture.native.calls.get())
            fixture.watch.cancel(requestId)
            assertEquals("cancelled", JSONObject(response.get(5, TimeUnit.SECONDS)).getString("status"))
            await(fixture.native.cancellation); await(fixture.native.retirementRegistered)
            fixture.native.retire!!()
        }
    }

    @Test fun stopBeforePhoneCancellationRegistrationIsLatchedAndDeliveredAtReady() {
        Fixture().use { fixture ->
            fixture.native.readinessGate = CountDownLatch(1)
            fixture.native.run = { fixture.native.allowReturn.await(5, TimeUnit.SECONDS); """{"status":"cancelled"}""" }
            val response = fixture.start()
            await(fixture.native.entered)
            fixture.watch.cancel(requestId)
            assertEquals("cancelled", JSONObject(response.get(5, TimeUnit.SECONDS)).getString("status"))
            fixture.native.readinessGate!!.countDown()
            await(fixture.native.cancellation); await(fixture.native.retirementRegistered)
            fixture.native.retire!!()
        }
    }

    @Test fun realNanoProviderRelayPreservesSearchFooterAndSpeechContract() {
        Fixture().use { fixture ->
            val footer = "\n\nSources:\nhttps://example.invalid/relay-source\n"
            fixture.phoneProvider = GeminiNanoProvider(RuntimeEnvironment.getApplication(), searchNative = object : ProviderNativeCalls {
                override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback) = "{}"
                override fun cancel(requestId: String) {}
                override fun prepareSearch(requestJson: String, callback: ProviderDeltaCallback): String {
                    callback.onStatus("Searching")
                    return JSONObject().put("evidenceContext", "Synthetic evidence").put("sourceFooter", footer).toString()
                }
            }, engineFactory = {
                object : GeminiNanoEngine {
                    override suspend fun prepareInferenceEngine() {}
                    override fun generateContentStream(prompt: String): Flow<String> = flow { emit("Relayed"); emit(" answer") }
                    override fun close() {}
                }
            })
            val callback = Callback()
            val json = JSONObject(request()).put("webEnabled", true).put("searchMode", "verify").toString()
            val outcome = JSONObject(fixture.start(callback, json = json).get(5, TimeUnit.SECONDS))
            assertEquals("Relayed answer$footer", outcome.getString("text"))
            assertEquals("Relayed answer", outcome.getString("speechText"))
            assertEquals(listOf("Relayed", " answer", footer), callback.deltas)
            assertEquals(listOf("Searching"), callback.statuses)
            val retired = CountDownLatch(1)
            fixture.watch.whenRetired(requestId) { retired.countDown() }
            await(retired)
        }
    }

    @Test fun executionServiceLossCancelsOwnedProviderAndDoesNotConfirmWatchRetirement() {
        Fixture().use { fixture ->
            fixture.native.run = { fixture.native.allowReturn.await(5, TimeUnit.SECONDS); """{"status":"cancelled"}""" }
            val response = fixture.start()
            await(fixture.native.entered)
            requireNotNull(fixture.executionLost).invoke()
            assertEquals("error", JSONObject(response.get(5, TimeUnit.SECONDS)).getString("status"))
            await(fixture.native.cancellation); await(fixture.native.retirementRegistered)
            val retired = CountDownLatch(1)
            fixture.watch.whenRetired(requestId) { retired.countDown() }
            assertEquals(1L, retired.count)
            fixture.native.retire!!()
            assertFalse(retired.await(100, TimeUnit.MILLISECONDS))
            assertEquals("relay_busy", JSONObject(fixture.watch.respond("synthetic-next", request("synthetic-next"), Callback())).getString("code"))
        }
    }
}
