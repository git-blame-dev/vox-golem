package dev.gitblame.voxgolem.assistant.runtime

import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.InputStream
import java.io.OutputStream
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.Executor
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.Rule
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class AgyProcessTest {
    @get:Rule val folder = TemporaryFolder()
    private val delta = """{"event":"step_update","step_update":{"step_type":"agent_response","text_delta":"Synthetic answer"}}"""
    private val success = """{"event":"result","result":{"status":"SUCCESS"}}"""
    private class Engine(output: String, val blockWrite: Boolean = false, val blockRead: Boolean = false,
        var ignoreDestroy: Boolean = false) : Process() {
        val entered = CountDownLatch(1)
        val unblock = CountDownLatch(1)
        val input = ByteArrayOutputStream()
        @Volatile var exited = false
        private val source = ByteArrayInputStream(output.toByteArray())
        private fun block() { entered.countDown(); check(unblock.await(5, TimeUnit.SECONDS)) }
        override fun getOutputStream() = object : OutputStream() {
            override fun write(value: Int) { input.write(value) }
            override fun write(bytes: ByteArray, offset: Int, length: Int) {
                if (blockWrite) block()
                input.write(bytes, offset, length)
            }
        }
        override fun getInputStream() = object : InputStream() {
            override fun read(): Int { if (blockRead) block(); return source.read() }
            override fun read(bytes: ByteArray, offset: Int, length: Int): Int {
                if (blockRead) block(); return source.read(bytes, offset, length)
            }
        }
        override fun getErrorStream() = ByteArrayInputStream(byteArrayOf())
        override fun exitValue(): Int { if (!exited) throw IllegalThreadStateException(); return 0 }
        override fun waitFor(): Int { check(unblock.await(5, TimeUnit.SECONDS)); return 0 }
        override fun waitFor(timeout: Long, unit: TimeUnit): Boolean = exited
        override fun destroy() { if (!ignoreDestroy) { exited = true; unblock.countDown() } }
        override fun destroyForcibly(): Process { destroy(); return this }
    }
    private class Callback : ProviderDeltaCallback {
        val text = StringBuilder()
        var ready = 0
        override fun onReady() { ready++ }
        override fun onDelta(text: String) { this.text.append(text) }
    }
    private fun request(previous: String = "Synthetic previous") = JSONObject().put("model", "agy-flash-high")
        .put("webEnabled", true).put("instructions", "Synthetic full application instructions: keep roles and quote history")
        .put("externalSessionId", "synthetic-shared-session").put("messages", JSONArray()
            .put(JSONObject().put("role", "user").put("content", previous))
            .put(JSONObject().put("role", "assistant").put("content", "Synthetic earlier refusal").put("contentType", "refusal"))
            .put(JSONObject().put("role", "user").put("content", "Synthetic next question")))
    private fun provider(timeout: Long = 5000, factory: () -> Engine) = AgyProviderNative(agyTestContext(folder.newFolder()),
        { File("/synthetic/agycli") }, { _, _, _ -> factory() }, timeout)

    @Test fun everyBranchUsesFreshProcessWithExactRolePreservingCanonicalInputAndFullInstructions() {
        val engines = mutableListOf<Engine>()
        provider { Engine("$delta\n$success\n").also { engines += it } }.use { native ->
            for (branch in listOf("Synthetic branch A", "Synthetic branch B with quoted \"history\"")) {
                val req = request(branch)
                val result = JSONObject(native.respond("synthetic-$branch", "", req.toString(), Callback()))
                assertEquals("complete", result.getString("status"))
                val wire = JSONObject(engines.last().input.toString("UTF-8"))
                assertEquals("user", wire.getString("event"))
                val content = wire.getJSONObject("message").getString("content")
                val canonical = JSONObject(content.substringAfter('\n'))
                assertEquals(req.getString("instructions"), canonical.getString("instructions"))
                assertEquals(req.getJSONArray("messages").toString(), canonical.getJSONArray("messages").toString())
                assertTrue(engines.last().exited)
            }
            assertEquals(2, engines.size)
            assertNotSame(engines[0], engines[1])
        }
    }
    @Test fun deltasNeverAuthorizeEofMalformedOrFailedTerminalAndNextRequestCannotInheritFailure() {
        val outputs = listOf("$delta\n", "$delta\n{\"event\":\"result\"}\n", "$delta\n{\"event\":\"result\",\"result\":{\"status\":\"FAILED\"}}\n")
        for (output in outputs) {
            val engines = mutableListOf<Engine>()
            provider { Engine(if (engines.isEmpty()) output else "$delta\n$success\n").also { engines += it } }.use { native ->
                val callback = Callback()
                val failed = JSONObject(native.respond("synthetic-failed", "", request().toString(), callback))
                assertEquals("error", failed.getString("status"))
                assertEquals("Synthetic answer", callback.text.toString())
                assertTrue(engines.single().exited)
                assertEquals("complete", JSONObject(native.respond("synthetic-next", "", request("Synthetic clean branch").toString(), Callback())).getString("status"))
                assertEquals(2, engines.size)
            }
        }
    }
    @Test fun webDisabledIsUnsupportedAfterCancellationRegistrationButBeforeProcessLaunch() {
        var launches = 0
        provider { launches++; Engine("") }.use { native ->
            val callback = Callback()
            val result = JSONObject(native.respond("synthetic-web-off", "", request().put("webEnabled", false).toString(), callback))
            assertEquals("unsupported_search_mode", result.getString("code"))
            assertEquals(0, launches); assertEquals(1, callback.ready)
        }
    }
    @Test @Config(sdk = [24, 25]) fun androidWithoutBoundedProcessRetirementRejectsAgyBeforeLaunch() {
        var launches = 0
        provider { launches++; Engine("") }.use { native ->
            assertEquals("unsupported_model", JSONObject(native.respond("synthetic-pre26", "", request().toString(), Callback())).getString("code"))
            assertEquals(0, launches)
        }
    }
    @Test fun cancellationRetiresBlockedWriteAndReadWithoutWaitingForTheIoMonitor() {
        for (write in listOf(true, false)) {
            val engine = Engine("", blockWrite = write, blockRead = !write)
            val executor = Executors.newSingleThreadExecutor()
            provider { engine }.use { native ->
                try {
                    val result = executor.submit<String> { native.respond("synthetic-blocked", "", request().toString(), Callback()) }
                    assertTrue(engine.entered.await(1, TimeUnit.SECONDS))
                    native.cancel("synthetic-blocked")
                    assertEquals("cancelled", JSONObject(result.get(1, TimeUnit.SECONDS)).getString("status"))
                    assertTrue(engine.exited)
                } finally { engine.unblock.countDown(); executor.shutdownNow(); assertTrue(executor.awaitTermination(2, TimeUnit.SECONDS)) }
            }
        }
    }
    @Test fun workerRouterCancellationDuringStartupIsPromptButRetirementWaitsForItsLateProcess() {
        val entered = CountDownLatch(1); val release = CountDownLatch(1)
        val retired = CountDownLatch(1)
        val retiredAfterExit = AtomicBoolean()
        val factoryCalls = AtomicInteger()
        val outcomes = ConcurrentLinkedQueue<String>()
        val deltas = AtomicInteger()
        val engine = Engine("")
        val context = agyTestContext(folder.newFolder())
        val providerExecutor = Executors.newSingleThreadExecutor()
        val cancelExecutor = Executors.newSingleThreadExecutor()
        AgyProviderNative(context, { File("/synthetic/agycli") }, { _, _, _ ->
            if (factoryCalls.getAndIncrement() == 0) {
                check(File(context.filesDir, "ws").isDirectory) // Actual environment preparation was not bypassed.
                entered.countDown(); check(release.await(10, TimeUnit.SECONDS)); engine
            } else Engine("$delta\n$success\n")
        }, 180_000).use { native ->
            val unexpectedDefault = object : ProviderNativeCalls {
                override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback) = error("Wrong provider route")
                override fun cancel(requestId: String) = error("Wrong cancellation route")
            }
            val worker = AssistantRequestWorker(providerExecutor, AgyProviderRouter(context, unexpectedDefault, native))
            val listener = object : AssistantRequestListener {
                override fun onDelta(requestId: String, text: String) { deltas.incrementAndGet() }
                override fun onOutcome(requestId: String, outcomeJson: String) { outcomes += requestId }
                override fun onRetired(requestId: String) {
                    if (requestId == "synthetic-startup") { retiredAfterExit.set(engine.exited); retired.countDown() }
                }
            }
            try {
                assertTrue(worker.submit("synthetic-startup", "", request().toString(), listener))
                assertTrue(entered.await(2, TimeUnit.SECONDS))
                cancelExecutor.submit { worker.cancel() }.get(1, TimeUnit.SECONDS)
                val cancellation = native.javaClass.getDeclaredField("cancelled").apply { isAccessible = true }.get(native) as AtomicBoolean
                assertTrue("Worker cancellation must reach Agy while process creation is still gated", cancellation.get())
                // The worker runnable unwinds promptly, not at its normal 180-second deadline.
                providerExecutor.submit { }.get(2, TimeUnit.SECONDS)
                assertEquals(1L, retired.count)
                assertFalse(engine.exited)
                assertFalse(worker.submit("synthetic-overlap", "", request().toString(), listener))
                release.countDown()
                assertTrue(retired.await(2, TimeUnit.SECONDS))
                assertTrue(retiredAfterExit.get())
                assertEquals(0, engine.input.size())
                assertTrue(outcomes.isEmpty()); assertEquals(0, deltas.get())
                assertTrue(worker.submit("synthetic-replacement", "", request().toString(), listener))
                providerExecutor.submit { }.get(2, TimeUnit.SECONDS)
                assertEquals(listOf("synthetic-replacement"), outcomes.toList())
            } finally {
                release.countDown(); worker.close()
                providerExecutor.shutdownNow(); cancelExecutor.shutdownNow()
                assertTrue(providerExecutor.awaitTermination(2, TimeUnit.SECONDS))
                assertTrue(cancelExecutor.awaitTermination(2, TimeUnit.SECONDS))
            }
        }
    }
    @Test fun cancellationLatchedBetweenWorkerAdmissionAndAgyRegistrationCannotBeLost() {
        val context = agyTestContext(folder.newFolder())
        var launches = 0; var retirements = 0; var outcomes = 0
        AgyProviderNative(context, { File("/synthetic/agycli") }, { _, _, _ -> launches++; Engine("") }).use { native ->
            lateinit var worker: AssistantRequestWorker
            val beforeClaim = object : ProviderNativeCalls {
                override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String {
                    worker.cancel() // Native registration has not happened, but the worker has admitted its runnable.
                    return native.respond(requestId, authPath, requestJson, callback)
                }
                override fun cancel(requestId: String) = native.cancel(requestId)
                override fun whenRetired(requestId: String, retired: () -> Unit) = native.whenRetired(requestId, retired)
            }
            worker = AssistantRequestWorker(Executor { it.run() }, AgyProviderRouter(context, beforeClaim, beforeClaim))
            try {
                assertTrue(worker.submit("synthetic-early-cancel", "", request().toString(), object : AssistantRequestListener {
                    override fun onDelta(requestId: String, text: String) { fail("Cancelled work cannot emit text") }
                    override fun onOutcome(requestId: String, outcomeJson: String) { outcomes++ }
                    override fun onRetired(requestId: String) { retirements++ }
                }))
                assertEquals(0, launches); assertEquals(0, outcomes); assertEquals(1, retirements)
                assertFalse(File(context.filesDir, "ws").exists())
            } finally { worker.close() }
        }
    }
    @Test fun registrationCancellationBeforeAcquisitionDoesNotDestroyAnUnusedPrewarm() {
        val context = agyTestContext(folder.newFolder())
        val warm = Engine("")
        var launches = 0; var retirements = 0
        AgyProviderNative(context, { File("/synthetic/agycli") }, { _, _, _ -> launches++; warm }).use { native ->
            native.prewarm("agy-flash-high", true)
            val io = native.javaClass.getDeclaredField("io").apply { isAccessible = true }.get(native) as java.util.concurrent.ThreadPoolExecutor
            io.submit { }.get(2, TimeUnit.SECONDS)
            assertEquals(1, launches); assertFalse(warm.exited)
            lateinit var worker: AssistantRequestWorker
            val beforeClaim = object : ProviderNativeCalls {
                override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String {
                    worker.cancel(); return native.respond(requestId, authPath, requestJson, callback)
                }
                override fun cancel(requestId: String) = native.cancel(requestId)
                override fun whenRetired(requestId: String, retired: () -> Unit) = native.whenRetired(requestId, retired)
            }
            worker = AssistantRequestWorker(Executor { it.run() }, AgyProviderRouter(context, beforeClaim, beforeClaim))
            try {
                assertTrue(worker.submit("synthetic-cancelled-before-acquisition", "", request().toString(), object : AssistantRequestListener {
                    override fun onDelta(requestId: String, text: String) { fail("No request input was admitted") }
                    override fun onOutcome(requestId: String, outcomeJson: String) { fail("Cancelled outcome must be suppressed") }
                    override fun onRetired(requestId: String) { retirements++ }
                }))
                assertEquals(1, retirements); assertEquals(1, launches)
                assertFalse(warm.exited); assertEquals(0, warm.input.size())
            } finally { worker.close() }
        }
        assertTrue(warm.exited)
    }
    @Test fun requestDeadlineRetiresSilentProcessAndLineBudgetsRejectUnterminatedOutput() {
        val engine = Engine("", blockRead = true)
        val executor = Executors.newSingleThreadExecutor()
        provider(500) { engine }.use { native ->
            try {
                val result = executor.submit<String> { native.respond("synthetic-deadline", "", request().toString(), Callback()) }
                assertTrue("Deadline regression must reach the blocked-read phase", engine.entered.await(2, TimeUnit.SECONDS))
                assertEquals("timeout", JSONObject(result.get(3, TimeUnit.SECONDS)).getString("code"))
                assertTrue(engine.exited)
            } finally { engine.unblock.countDown(); executor.shutdownNow(); assertTrue(executor.awaitTermination(2, TimeUnit.SECONDS)) }
        }
        assertThrows(IllegalStateException::class.java) { AgyLines(ByteArrayInputStream("x".repeat(9).toByteArray()), 8).next() }
        assertThrows(IllegalStateException::class.java) { AgyLines(ByteArrayInputStream("x\nx\n".toByteArray()), 8, 3).next() }
        assertThrows(java.nio.charset.CharacterCodingException::class.java) { AgyLines(ByteArrayInputStream(byteArrayOf(-1, 10))).next() }
    }
    @Test fun stalledLaunchHasABoundedRequestOutcomeAndItsLateProcessRemainsOwnedForRetirement() {
        val entered = CountDownLatch(1); val release = CountDownLatch(1)
        val engine = Engine("")
        val executor = Executors.newSingleThreadExecutor()
        provider(500) { entered.countDown(); check(release.await(5, TimeUnit.SECONDS)); engine }.use { native ->
            try {
                val result = executor.submit<String> { native.respond("synthetic-stalled-launch", "", request().toString(), Callback()) }
                assertTrue(entered.await(1, TimeUnit.SECONDS))
                assertEquals("retirement_failed", JSONObject(result.get(3, TimeUnit.SECONDS)).getString("code"))
                assertEquals("busy", JSONObject(native.respond("synthetic-overlap", "", request().toString(), Callback())).getString("code"))
                val confirmed = CountDownLatch(1)
                native.whenRetired("synthetic-stalled-launch") { confirmed.countDown() }
                release.countDown()
                assertTrue(confirmed.await(2, TimeUnit.SECONDS))
            } finally {
                release.countDown(); executor.shutdownNow(); assertTrue(executor.awaitTermination(2, TimeUnit.SECONDS))
            }
        }
        assertTrue(engine.exited)
    }
    @Test fun unconfirmedRetirementBlocksReplacementUntilPhysicalExit() {
        val engine = Engine("$delta\n$success\n", ignoreDestroy = true)
        provider { engine }.use { native ->
            assertEquals("retirement_failed", JSONObject(native.respond("synthetic-complete", "", request().toString(), Callback())).getString("code"))
            assertFalse(engine.exited)
            assertEquals("busy", JSONObject(native.respond("synthetic-overlap", "", request().toString(), Callback())).getString("code"))
            val retired = CountDownLatch(1)
            native.whenRetired("synthetic-complete") { retired.countDown() }
            assertFalse(retired.await(100, TimeUnit.MILLISECONDS))
            engine.ignoreDestroy = false; engine.destroy()
            assertTrue(retired.await(1, TimeUnit.SECONDS))
        }
    }

    @Test fun agyUsesPreparedSearchRouteAndAppendsSourceFooterOnLiveQuery() {
        val footer = "\n\nSources:\nhttps://example.com/weather\n"
        var prepareSearchCalls = 0
        val fakeSearchNative = object : ProviderNativeCalls {
            override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String = error("Not used")
            override fun cancel(requestId: String) {}
            override fun prepareSearch(requestJson: String, callback: ProviderDeltaCallback): String {
                prepareSearchCalls++
                callback.onStatus("Searching")
                return JSONObject()
                    .put("evidenceContext", "Tokyo weather is 18C and clear.")
                    .put("sourceFooter", footer)
                    .put("needsEscalation", false)
                    .toString()
            }
        }
        val engines = mutableListOf<Engine>()
        val testContext = agyTestContext(folder.newFolder())
        AgyProviderNative(
            context = testContext,
            executableSource = { File("/synthetic/agycli") },
            processFactory = { _, _, _ -> Engine("$delta\n$success\n").also { engines += it } },
            requestTimeoutMs = 5000,
            searchNative = fakeSearchNative
        ).use { native ->
            val callback = Callback()
            val req = JSONObject()
                .put("model", "agy-flash-high")
                .put("webEnabled", true)
                .put("searchMode", "quick")
                .put("prompt", "What is the weather in Tokyo today?")
            val resStr = native.respond("req-search-test", "", req.toString(), callback)
            val res = JSONObject(resStr)

            assertEquals(1, prepareSearchCalls)
            assertEquals("complete", res.getString("status"))
            assertEquals("Synthetic answer$footer", res.getString("text"))
            assertEquals("Synthetic answer", res.getString("speechText"))
            assertEquals("Synthetic answer$footer", callback.text.toString())
            assertTrue(res.has("retireMs"))
            assertTrue(res.has("searchStatus"))
            assertFalse(res.getBoolean("fallbackUsed"))

            val wire = JSONObject(engines.single().input.toString("UTF-8"))
            assertEquals("user", wire.getString("event"))
            val content = wire.getJSONObject("message").getString("content")
            val canonical = JSONObject(content.substringAfter('\n'))
            assertTrue(canonical.has("webEvidence"))
            assertEquals("Tokyo weather is 18C and clear.", canonical.getString("webEvidence"))
            assertTrue(content.substringBefore('\n').contains("Use the provided webEvidence field directly to answer"))
        }
    }

    @Test fun agyPrivateEnvironmentPrepareWritesPureVoiceAssistantAgentAndEmptySkills() {
        val testContext = agyTestContext(folder.newFolder())
        assertTrue(AgyPrivateEnvironment.prepare(testContext))

        val workspace = File(testContext.filesDir, "ws")
        val agentFile = File(workspace, ".agents/agents/vox-assistant.md")
        assertTrue("vox-assistant.md must exist", agentFile.isFile)
        val agentContent = agentFile.readText(Charsets.UTF_8)
        assertTrue(agentContent.contains("name: vox-assistant"))
        assertTrue(agentContent.contains("excludeDefaultComponents: true"))
        assertTrue(agentContent.contains("inheritCustomizations: false"))
        assertTrue(agentContent.contains("inheritMcp: false"))
        assertTrue(agentContent.contains("enable_write_tools: false"))
        assertTrue(agentContent.contains("enable_mcp_tools: false"))
        assertTrue(agentContent.contains("enable_subagent_tools: false"))
        assertTrue(agentContent.contains("disabledTools:"))
        assertTrue(agentContent.contains("- read_browser_page"))
        assertFalse(agentContent.contains("- search_web"))
        assertTrue(agentContent.contains("You are VoxGolem, a fast, direct voice and chat assistant."))

        val wsSkillsFile = File(workspace, ".agents/skills.json")
        assertTrue("ws skills.json must exist", wsSkillsFile.isFile)
        assertEquals("{\"entries\":[]}", wsSkillsFile.readText(Charsets.UTF_8))

        val wsRulesFile = File(workspace, ".agents/rules.json")
        assertTrue("ws rules.json must exist", wsRulesFile.isFile)
        assertEquals("{\"entries\":[]}", wsRulesFile.readText(Charsets.UTF_8))

        val configSkillsFile = File(testContext.filesDir, "home/.gemini/config/skills.json")
        assertTrue("home skills.json must exist", configSkillsFile.isFile)
        assertEquals("{\"entries\":[]}", configSkillsFile.readText(Charsets.UTF_8))

        val configPluginsFile = File(testContext.filesDir, "home/.gemini/config/plugins.json")
        assertTrue("home plugins.json must exist", configPluginsFile.isFile)
        assertEquals("{\"entries\":[]}", configPluginsFile.readText(Charsets.UTF_8))
    }

    @Test fun cancellationDuringRoundZeroSearchPreservesPrewarmedProcessForNextRequest() {
        val testContext = agyTestContext(folder.newFolder())
        val warmEngine = Engine("$delta\n$success\n")
        var launches = 0
        var prepareSearchCalls = 0
        lateinit var nativeRef: AgyProviderNative

        val fakeSearchNative = object : ProviderNativeCalls {
            override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String = error("Not used")
            override fun cancel(requestId: String) {}
            override fun prepareSearch(requestJson: String, callback: ProviderDeltaCallback): String {
                prepareSearchCalls++
                if (prepareSearchCalls == 1) {
                    nativeRef.cancel("req-cancel-in-search")
                    return """{"evidenceContext":null,"sourceFooter":"","sources":[]}"""
                }
                return JSONObject()
                    .put("evidenceContext", "Tokyo weather is 18C and clear.")
                    .put("sourceFooter", "\n\nSources:\nhttps://example.com/weather\n")
                    .put("needsEscalation", false)
                    .toString()
            }
        }

        AgyProviderNative(
            context = testContext,
            executableSource = { File("/synthetic/agycli") },
            processFactory = { _, _, _ ->
                launches++
                if (launches == 1) warmEngine else Engine("$delta\n$success\n")
            },
            requestTimeoutMs = 5000,
            searchNative = fakeSearchNative
        ).use { native ->
            nativeRef = native
            native.prewarm("agy-flash-high", true)
            val io = native.javaClass.getDeclaredField("io").apply { isAccessible = true }.get(native) as java.util.concurrent.ThreadPoolExecutor
            io.submit { }.get(2, TimeUnit.SECONDS)
            assertEquals(1, launches)
            assertFalse(warmEngine.exited)

            val req = JSONObject()
                .put("model", "agy-flash-high")
                .put("webEnabled", true)
                .put("searchMode", "quick")
                .put("prompt", "What is the weather in Tokyo today?")

            // 1. Cancelled during prepareSearch -> warmEngine is NOT retired or written to
            val res1 = JSONObject(native.respond("req-cancel-in-search", "", req.toString(), Callback()))
            assertEquals("cancelled", res1.getString("status"))
            assertEquals(1, prepareSearchCalls)
            assertEquals(1, launches)
            assertFalse("Prewarmed process must remain alive when cancelled during Round-0 search", warmEngine.exited)
            assertEquals(0, warmEngine.input.size())

            // 2. Subsequent request reuses the preserved warmEngine without launching a new process
            val res2 = JSONObject(native.respond("req-reuse-warm", "", req.toString(), Callback()))
            assertEquals("complete", res2.getString("status"))
            assertTrue("Subsequent request must report warm=true", res2.getBoolean("warm"))
            assertEquals(2, prepareSearchCalls)
            assertEquals(1, launches)
            assertTrue(warmEngine.input.size() > 0)
            assertTrue(warmEngine.exited)
        }
    }

    @Test fun agyPrewarmTriggersSearchNativePrewarmAndColdRespondStartsProcessBeforeSearch() {
        val testContext = agyTestContext(folder.newFolder())
        val events = mutableListOf<String>()
        val fakeSearchNative = object : ProviderNativeCalls {
            override fun prewarm(model: String, authPath: String, webEnabled: Boolean) {
                if (webEnabled) events += "search_prewarm"
            }
            override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String = error("Not used")
            override fun cancel(requestId: String) {}
            override fun prepareSearch(requestJson: String, callback: ProviderDeltaCallback): String {
                events += "prepare_search"
                return """{"evidenceContext":null,"sourceFooter":"","sources":[]}"""
            }
        }
        AgyProviderNative(
            context = testContext,
            executableSource = { File("/synthetic/agycli") },
            processFactory = { _, _, _ ->
                events += "process_launch"
                Engine("$delta\n$success\n")
            },
            requestTimeoutMs = 5000,
            searchNative = fakeSearchNative,
        ).use { native ->
            val req = JSONObject()
                .put("model", "agy-flash-high")
                .put("webEnabled", true)
                .put("searchMode", "quick")
                .put("prompt", "What is the weather in Tokyo today?")
            val res = JSONObject(native.respond("req-cold-order", "", req.toString(), Callback()))
            assertEquals("complete", res.getString("status"))
            assertEquals(listOf("process_launch", "prepare_search"), events)

            events.clear()
            native.prewarm("agy-flash-high", true)
            val io = native.javaClass.getDeclaredField("io").apply { isAccessible = true }.get(native) as java.util.concurrent.ThreadPoolExecutor
            io.submit { }.get(2, TimeUnit.SECONDS)
            assertEquals(listOf("search_prewarm", "process_launch"), events)
        }
    }
}
