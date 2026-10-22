package dev.gitblame.voxgolem.assistant.replication

import android.content.Context
import android.content.ContextWrapper
import android.content.pm.PackageManager
import android.database.sqlite.SQLiteDatabase
import dev.gitblame.voxgolem.assistant.AssistantStore
import dev.gitblame.voxgolem.assistant.AssistantStoreRunner
import dev.gitblame.voxgolem.assistant.runtime.ChatGptAuth
import dev.gitblame.voxgolem.assistant.runtime.PhoneRelaySessionTest
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import java.io.File
import java.util.concurrent.Executors
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.ArrayDeque
import java.util.UUID
import java.util.concurrent.Executor

@RunWith(AssistantStoreRunner::class)
@Config(sdk = [24, 35], manifest = Config.NONE)
class AssistantPeerSetupTest {
    @get:Rule val folder = TemporaryFolder()
    private val a = "00000000-0000-4000-8000-000000000001"
    private val b = "00000000-0000-4000-8000-000000000002"
    private val c = "00000000-0000-4000-8000-000000000003"

    @Test fun oppositeFormFactorsLinkWithoutProviderFilesOrCredentialIdentity() = worker {
        val pair = PairFixture()
        pair.link()
        assertEquals(PeerPairingReason.ENROLLED, pair.left.state().reason)
        assertEquals(PeerPairingReason.ENROLLED, pair.right.state().reason)
        assertFalse(pair.left.state().credentialsAllowed)
        assertFalse(pair.right.state().credentialsAllowed)
        assertEquals(pair.left.state().associationId, pair.right.state().associationId)
        assertEquals(b, pair.left.state().peerInstallationId)
        EnrolledPeerRegistry(pair.leftContext).use { assertEquals(setOf(b), it.replicas()) }
        EnrolledPeerRegistry(pair.rightContext).use { assertEquals(setOf(a), it.replicas()) }
        assertTrue(pair.mailbox.events.all { it.uri.path == ReplicationWire.IDENTITY_PATH })
    }

    @Test fun reconnectAndReopenPreserveAssociationButNeverUseSavedNodeRoutes() = worker {
        val pair = PairFixture()
        pair.link()
        val before = pair.left.state()
        pair.mailbox.nodes.remove("synthetic-right-node")
        pair.publish(GmsReplicationTransport(pair.mailbox.sdk("synthetic-new-right-node"), budget()), b, "watch")
        val reopened = pair.setup(pair.leftContext, a, pair.leftSdk, false)
        reopened.retry(a, pair.leftIo)
        assertEquals(before, reopened.state())
        assertNull(reopened.replicaForNode("synthetic-right-node"))
        assertEquals(b, reopened.replicaForNode("synthetic-new-right-node"))
        pair.mailbox.unreachableNodes.add("synthetic-new-right-node")
        assertNull(reopened.replicaForNode("synthetic-new-right-node"))
    }

    @Test fun unchangedSetupAndDiscoveryFailuresAreNotCredentialChanges() = worker {
        val pair = PairFixture()
        var changes = 0
        shadowOf(pair.leftContext.packageManager).setSystemFeature(PackageManager.FEATURE_WATCH, false)
        val setup = AssistantPeerSetup(pair.leftContext, { a }, { pair.leftIo }, { changes++ })
        assertEquals(PeerPairingReason.ENROLLED, setup.retry(a, pair.leftIo).reason)
        assertEquals(1, changes)
        repeat(3) { setup.retry(a, pair.leftIo) }
        setup.recordFailure(PeerPairingReason.DISCOVERY_FAILED)
        pair.leftSdk.capabilityError = IllegalStateException("Synthetic discovery failure")
        setup.retry(a, pair.leftIo)
        pair.leftSdk.capabilityError = null
        setup.retry(a, pair.leftIo)
        assertEquals(1, changes)
        pair.publish(pair.rightIo, c, "watch")
        setup.retry(a, pair.leftIo)
        assertEquals(2, changes)
    }

    @Test fun operationRouteReusesOneFreshSelectionButCannotOutliveItsBudgetOrApproval() = worker {
        val pair = PairFixture(); pair.link()
        val routes = pair.left.operationAssociation(pair.leftIo)
        assertEquals(b, routes.replicaForNode("synthetic-right-node"))
        val lookups = pair.leftSdk.lookups.size
        assertEquals(b, routes.replicaForNode("synthetic-right-node"))
        assertEquals(lookups, pair.leftSdk.lookups.size)
        EnrolledPeerRegistry(pair.leftContext).use { it.revoke(b) }
        assertNull(routes.replicaForNode("synthetic-right-node"))
        val expired = pair.left.operationAssociation(pair.leftIo.withWait(BoundedGmsWait(System.nanoTime() - 1)))
        assertThrows(java.util.concurrent.TimeoutException::class.java) { expired.replicaForNode("synthetic-right-node") }
        assertEquals(lookups, pair.leftSdk.lookups.size)
    }

    @Test fun historyEpochResetsOnlySessionsAndPreservesCurrentTrustedBindingsAndProviderFiles() = worker {
        for (oldVersion in 1..3) {
            val context = isolated("synthetic-migration-$oldVersion")
            val auth = ChatGptAuth.managedFile(context).apply { writeText("Synthetic managed credential bytes") }
            val authBefore = auth.readBytes()
            AssistantStore(context).use { history ->
                history.createConversation("Synthetic retained migration history")
                val historyBefore = history.snapshot().toString()
                val publicationsBefore = history.publications(b, 128).toString()
                val sequenceBefore = history.allocateId().getString("sequence").toLong()
                val replica = history.replica()
                assertEquals(replica, NativeReplicationStore(history).replica)
                assertEquals(sequenceBefore + 1, history.allocateId().getString("sequence").toLong())
                SQLiteDatabase.openOrCreateDatabase(File(context.noBackupFilesDir, "assistant-replication.db"), null).use { db ->
                    db.execSQL("CREATE TABLE peers (replica TEXT PRIMARY KEY, session TEXT)")
                    db.execSQL("CREATE TABLE cleanup_peers (replica TEXT PRIMARY KEY)")
                    db.execSQL("CREATE TABLE setup (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
                    if (oldVersion < 3) db.execSQL("CREATE TABLE native_bindings (replica TEXT PRIMARY KEY, peer_installation TEXT NOT NULL, association TEXT NOT NULL, hub TEXT NOT NULL, credentials_allowed INTEGER NOT NULL)")
                    else db.execSQL("CREATE TABLE native_bindings (replica TEXT PRIMARY KEY, association TEXT NOT NULL)")
                    db.execSQL("CREATE TABLE retry (singleton INTEGER PRIMARY KEY, attempts INTEGER NOT NULL, due INTEGER NOT NULL)")
                    db.execSQL("INSERT INTO retry VALUES (1, 2, 1800000030000)")
                    db.execSQL("INSERT INTO peers VALUES (?, ?)", arrayOf(b, "{\"synthetic_session\":true}"))
                    db.execSQL("INSERT INTO cleanup_peers VALUES (?)", arrayOf(c))
                    db.execSQL("INSERT INTO setup VALUES ('status', '{\"reason\":\"INVITATION_PENDING\"}')")
                    if (oldVersion < 3) db.execSQL("INSERT INTO native_bindings VALUES (?, ?, ?, ?, 1)", arrayOf(b, c, a, a))
                    else db.execSQL("INSERT INTO native_bindings VALUES (?, ?)", arrayOf(b, a))
                    db.version = oldVersion
                }
                EnrolledPeerRegistry(context).use { registry ->
                    if (oldVersion < 3) assertNull(registry.setupValue("status"))
                    else assertEquals("INVITATION_PENDING", registry.setupValue("status")!!.getString("reason"))
                    assertEquals(setOf(b), registry.replicas())
                    if (oldVersion < 3) assertNull(registry.bindingFor(b, b))
                    else assertEquals(a, registry.bindingFor(b, b)!!.association)
                }
                DurableReplicationState(context).use { state ->
                    assertNull(state.load(b))
                    assertEquals(setOf(c), state.cleanupReplicas())
                    assertEquals(0L, state.retryDelay(1_800_000_000_000L))
                }
                assertEquals(historyBefore, history.snapshot().toString())
                assertEquals(publicationsBefore, history.publications(b, 128).toString())
            }
            assertArrayEquals(authBefore, auth.readBytes())
        }
    }

    @Test fun missingOldV2UnknownVersionAndWrongRoleCannotAuthorizeRouting() = worker {
        val pair = PairFixture()
        pair.mailbox.slots.clear()
        assertEquals(PeerPairingReason.WAITING_FOR_PEER, pair.left.retry(a, pair.leftIo).reason)
        for ((version, role) in listOf(1 to null, 2 to "watch", 4 to "watch", 3 to "unknown")) {
            pair.publish(pair.rightIo, b, role, version)
            assertEquals(PeerPairingReason.PEER_UPDATE_REQUIRED, pair.left.retry(a, pair.leftIo).reason)
            assertNull(pair.left.replicaForNode("synthetic-right-node"))
        }
        pair.publish(pair.rightIo, b, "phone")
        assertEquals(PeerPairingReason.WAITING_FOR_PEER, pair.left.retry(a, pair.leftIo).reason)
    }

    @Test fun multipleIdentitiesAndDuplicateRoutesDisableExistingBinding() = worker {
        val pair = PairFixture(); pair.link()
        val other = GmsReplicationTransport(pair.mailbox.sdk("synthetic-other-watch"), budget())
        pair.publish(other, c, "watch")
        assertEquals(PeerPairingReason.MULTIPLE_PEERS, pair.left.retry(a, pair.leftIo).reason)
        assertNull(pair.left.replicaForNode("synthetic-right-node"))
        pair.publish(other, b, "watch")
        assertEquals(PeerPairingReason.MULTIPLE_PEERS, pair.left.retry(a, pair.leftIo).reason)
        assertNull(pair.left.replicaForNode("synthetic-right-node"))
    }

    @Test fun sameRoleIgnoredButOfflineOppositePeerStillCountsForAmbiguity() = worker {
        val pair = PairFixture()
        val extra = GmsReplicationTransport(pair.mailbox.sdk("synthetic-extra-peer"), budget())
        pair.publish(extra, c, "phone")
        assertEquals(PeerPairingReason.ENROLLED, pair.left.retry(a, pair.leftIo).reason)
        pair.mailbox.unreachableNodes.add("synthetic-right-node")
        assertNull(pair.left.replicaForNode("synthetic-right-node"))
        pair.publish(extra, c, "watch")
        assertEquals(PeerPairingReason.MULTIPLE_PEERS, pair.left.retry(a, pair.leftIo).reason)
    }

    @Test fun discoveryFailureIsVisibleAndCannotFallBackToStoredRoute() = worker {
        val pair = PairFixture(); pair.link()
        pair.leftSdk.capabilityError = IllegalStateException("Synthetic SDK discovery failed")
        assertEquals(PeerPairingReason.DISCOVERY_FAILED, pair.left.retry(a, pair.leftIo).reason)
        assertNull(pair.left.replicaForNode("synthetic-right-node"))
        pair.leftSdk.capabilityError = null
        assertEquals(PeerPairingReason.ENROLLED, pair.left.retry(a, pair.leftIo).reason)
    }

    @Test fun selectedRelayRouteUsesFreshOppositeRoleBindingAndRevocationFence() = worker {
        val pair = PairFixture(); pair.link()
        val route = requireNotNull(pair.left.selectedRoute())
        assertEquals("synthetic-right-node", route.nodeId)
        assertTrue(route.stillApproved())
        assertNull(pair.left.selectedRoute("synthetic-unselected-node"))
        EnrolledPeerRegistry(pair.leftContext).use { it.revoke(b) }
        assertFalse(route.stillApproved())
        assertNull(pair.left.selectedRoute())
    }

    @Test fun accessOnlyMessagesUseCurrentPeerAndFenceSignoutReplayAndTransientExportFailures() = worker {
        val pair = PairFixture(); pair.link()
        val phoneFile = ChatGptAuth.managedFile(pair.leftContext).apply { writeText("{}") }
        val watchFile = ChatGptAuth.managedFile(pair.rightContext)
        var exported = JSONObject().put("voxgolemChatgpt", 1).put("openai", JSONObject().put("type", "oauth")
            .put("access", "synthetic-access-only").put("expires", System.currentTimeMillis() + 3_600_000)).toString()
        val requests = mutableListOf<ByteArray>()
        val states = mutableListOf<ByteArray>()
        var exports = 0
        val phone = ChatGptAccessSync.Owner(pair.leftContext, pair.left,
            export = { exports++; exported }, message = { _, kind, bytes -> assertEquals("state", kind); states.add(bytes) })
        val watch = ChatGptAccessSync.Owner(pair.rightContext, pair.right,
            export = { error("Watch must never own refresh/export") },
            message = { _, kind, bytes ->
                if (kind == "request") requests.add(bytes)
                else { assertEquals("ack", kind); phone.receive("synthetic-right-node", "ack", bytes) }
            })
        fun request() {
            watch.receive("synthetic-left-node", "hint", JSONObject().put("exchange", UUID.randomUUID().toString()).toString().toByteArray())
            phone.receive("synthetic-right-node", "request", requests.last())
        }
        phone.receive("synthetic-unselected-node", "request", "{}".toByteArray())
        assertEquals(0, exports)
        request()
        val first = states.last()
        watch.receive("synthetic-unselected-node", "state", first)
        assertFalse(watchFile.exists())
        watch.receive("synthetic-left-node", "state", first)
        assertEquals("synthetic-access-only", JSONObject(watchFile.readText()).getJSONObject("openai").getString("access"))
        assertFalse(watchFile.readText().contains("refresh"))
        val beforeError = watchFile.readBytes()
        exported = """{"error":"synthetic-transient-failure"}"""
        request()
        assertEquals(1, states.size)
        assertArrayEquals(beforeError, watchFile.readBytes())
        exported = "null"
        request()
        assertEquals(1, states.size) // An invalid/missing native store is not a sign-out.
        phoneFile.writeText(ChatGptAccessProtocol.tombstone().toString())
        request()
        val signout = states.last()
        assertTrue(JSONObject(signout.toString(Charsets.UTF_8)).isNull("credential"))
        watch.receive("synthetic-left-node", "state", signout)
        assertTrue(JSONObject(watchFile.readText()).getBoolean("signedOut"))
        val signedOut = watchFile.readBytes()
        // A fresh challenge prevents even a previous-association/process response from restoring access.
        watch.receive("synthetic-left-node", "hint", JSONObject().put("exchange", UUID.randomUUID().toString()).toString().toByteArray())
        try { watch.receive("synthetic-left-node", "state", first); fail("Expected stale response rejection") }
        catch (_: IllegalArgumentException) {}
        assertArrayEquals(signedOut, watchFile.readBytes())
        assertTrue(JSONObject(phoneFile.readText()).getBoolean("signedOut"))
    }

    @Test fun accessAssociationSurvivesOfflineDiscoveryButReplacementAndRevocationInvalidateLocally() = worker {
        val pair = PairFixture(); pair.link()
        val association = pair.right.credentialAssociation()
        val file = ChatGptAuth.managedFile(pair.rightContext)
        ChatGptAccessProtocol.publish(file, JSONObject().put("openai", JSONObject().put("access", "synthetic-access-only")
            .put("expires", System.currentTimeMillis() + 3_600_000)).put(ChatGptAccessProtocol.META,
            JSONObject().put("association", association).put("generation", 7)))
        val watch = ChatGptAccessSync.Owner(pair.rightContext, pair.right, message = { _, _, _ -> })
        pair.rightSdk.capabilityError = IllegalStateException("Synthetic offline SDK")
        pair.right.retry(b, pair.rightIo)
        assertEquals(association, pair.right.credentialAssociation())
        watch.receive("synthetic-unselected-node", "hint", "{}".toByteArray())
        assertTrue(JSONObject(file.readText()).has("openai"))
        pair.rightSdk.capabilityError = null
        pair.publish(pair.leftIo, c, "phone")
        pair.right.retry(b, pair.rightIo)
        assertNotEquals(association, pair.right.credentialAssociation())
        watch.receive("synthetic-unselected-node", "hint", "{}".toByteArray())
        assertTrue(JSONObject(file.readText()).getBoolean("signedOut"))
        EnrolledPeerRegistry(pair.rightContext).use { it.revoke(c) }
        assertNull(pair.right.credentialAssociation())
    }

    @Test fun expiryExportFailureRenewsWithoutAnotherLifecycleEvent() = worker {
        val f = CredentialFixture(); f.boot()
        f.token = "synthetic-renewed-access"; f.expires = f.clock.now + 3_600_000
        f.exportFailures = 1
        f.tick() // Existing 60-second-margin expiry timer, then one failed export.
        assertEquals("synthetic-original-access", f.access())
        f.until { f.access() == "synthetic-renewed-access" }
        assertEquals(0, f.clock.live("phone").size)
        assertEquals(1, f.clock.live("watch").size) // Only next credential expiry remains.
    }

    @Test fun expiryRequestSendFailureRenewsUsingBoundedBackoffWithoutLifecycleEvent() = worker {
        val f = CredentialFixture(); f.boot()
        f.token = "synthetic-renewed-access"; f.expires = f.clock.now + 3_600_000
        f.failSend["watch:request"] = 1
        f.tick()
        assertEquals("synthetic-original-access", f.access())
        assertTrue(f.clock.live().all { it.due > f.clock.now })
        f.until { f.access() == "synthetic-renewed-access" }
        assertEquals(0, f.clock.live("phone").size)
        assertEquals(1, f.clock.live("watch").size)
    }

    @Test fun droppedTombstoneSendAcknowledgementDoesNotCountAsRemotePersistence() = worker {
        val f = CredentialFixture(); f.boot()
        f.phoneFile.writeText(ChatGptAccessProtocol.tombstone().toString())
        f.dropSend["phone:state"] = 1 // SDK send returns success, but remote delivery never happens.
        f.phone.changed(); f.drain()
        assertEquals("synthetic-original-access", f.access())
        f.until { f.watchState().optBoolean("signedOut") }
        assertFalse(f.watchState().has("openai"))
        assertTrue(f.clock.live().isEmpty())
        assertTrue(f.queue.isEmpty())
    }

    @Test fun tombstoneHintSendFailureRecoversAndAcknowledgementLossRetriesWithoutRewritingCredentials() = worker {
        val f = CredentialFixture(); f.boot()
        f.phoneFile.writeText(ChatGptAccessProtocol.tombstone().toString())
        f.failSend["phone:hint"] = 1
        f.dropSend["watch:ack"] = 1
        f.phone.changed(); f.drain()
        assertEquals("synthetic-original-access", f.access())
        f.until { f.watchState().optBoolean("signedOut") }
        val committed = f.watchFile.readBytes()
        assertFalse(f.clock.live("phone").isEmpty()) // Local commit is not a received phone ACK.
        f.until { f.clock.live().isEmpty() }
        assertArrayEquals(committed, f.watchFile.readBytes())
        val ack = f.sent.last { it.first == "watch:ack" }.second
        assertEquals(setOf("association", "nonce", "generation"), JSONObject(ack.toString(Charsets.UTF_8)).keys().asSequence().toSet())
    }

    @Test fun incompleteRenewalStopsAfterFiniteAttemptsWithoutAnImmediateExpiryMarginLoop() = worker {
        val f = CredentialFixture(); f.boot()
        f.exportFailures = 100
        f.tick() // Enter expiry margin.
        val before = f.watchFile.readBytes()
        f.until { f.clock.live().isEmpty() }
        assertArrayEquals(before, f.watchFile.readBytes())
        assertTrue(f.sent.count { it.first == "watch:request" } <= 1 + ChatGptAccessSync.Owner.MAX_ATTEMPTS)
        assertTrue(f.clock.now < f.expires) // Budget ends before token expiry, not a zero-delay loop.
    }

    @Test fun reconnectOpportunityPreservesActiveReceiptAndDoesNotRestartHealthyExchange() = worker {
        val f = CredentialFixture()
        f.dropSend["watch:ack"] = 1
        f.boot()
        val receipt = f.sent.last { it.first == "watch:ack" }.second
        val sends = f.sent.size
        repeat(3) { f.pair.left.retry(a, f.pair.leftIo); f.pair.right.retry(b, f.pair.rightIo) }
        repeat(3) { f.phone.changed(); f.watch.changed() }
        repeat(3) { f.phone.opportunity(); f.watch.opportunity() }
        f.drain()
        assertEquals(sends, f.sent.size)
        f.phone.submit { receive("synthetic-right-node", "ack", receipt) }
        f.drain()
        assertTrue(f.clock.live("phone").isEmpty())
        f.phone.opportunity(); f.watch.opportunity(); f.drain()
        assertEquals(sends, f.sent.size)
    }

    @Test fun actualAccessRotationUnderSameAssociationReplacesPendingExchange() = worker {
        val f = CredentialFixture()
        f.dropSend["watch:ack"] = 1
        f.boot()
        val association = f.pair.left.credentialAssociation()
        val firstReceipt = f.sent.last { it.first == "watch:ack" }.second
        f.token = "synthetic-rotated-access"
        f.expires += 3_600_000
        f.phoneFile.writeText(JSONObject().put("voxgolemChatgpt", 1).put("openai",
            JSONObject().put("access", f.token).put("expires", f.expires)).toString())
        f.phone.changed(); f.drain()
        assertEquals(association, f.pair.left.credentialAssociation())
        assertEquals(f.token, f.access())
        val receipt = f.sent.last { it.first == "watch:ack" }.second
        assertNotEquals(JSONObject(firstReceipt.toString(Charsets.UTF_8)).getString("nonce"),
            JSONObject(receipt.toString(Charsets.UTF_8)).getString("nonce"))
        assertTrue(f.clock.live("phone").isEmpty())
    }

    @Test fun eachObservedOutageAllowsOneFiniteReconnectBudgetAndKeepsNonceAndOfflineAccess() = worker {
        val f = CredentialFixture(); f.boot()
        f.exportFailures = 100
        f.tick()
        f.until { f.clock.live().isEmpty() }
        val before = f.watchFile.readBytes()
        val firstBudget = f.sent.count { it.first == "watch:request" }
        val nonce = JSONObject(f.sent.last { it.first == "watch:request" }.second.toString(Charsets.UTF_8)).getString("nonce")
        f.pair.mailbox.unreachableNodes.add("synthetic-left-node")
        f.watch.opportunity(); f.drain()
        f.pair.mailbox.unreachableNodes.clear()
        f.watch.opportunity(); f.drain()
        f.until { f.clock.live().isEmpty() }
        val rearmed = f.sent.count { it.first == "watch:request" }
        assertEquals(ChatGptAccessSync.Owner.MAX_ATTEMPTS, rearmed - firstBudget)
        repeat(3) { f.watch.opportunity(); f.drain() }
        assertEquals(rearmed, f.sent.count { it.first == "watch:request" })
        assertTrue(f.clock.live().isEmpty())
        assertArrayEquals(before, f.watchFile.readBytes())
        f.pair.mailbox.unreachableNodes.add("synthetic-left-node")
        f.watch.opportunity(); f.drain()
        f.pair.mailbox.unreachableNodes.clear()
        f.exportFailures = 0
        f.token = "synthetic-recovered-access"; f.expires = f.clock.now + 3_600_000
        f.watch.opportunity(); f.drain()
        f.until { f.access() == "synthetic-recovered-access" }
        assertEquals(nonce, JSONObject(f.sent.last { it.first == "watch:request" }.second.toString(Charsets.UTF_8)).getString("nonce"))
        assertEquals(rearmed + 1, f.sent.count { it.first == "watch:request" })
        assertTrue(f.clock.live("phone").isEmpty())
    }

    @Test fun unavailableObservationDuringActiveBudgetIsNotLostAndDuplicateOpportunitiesAreCoalesced() = worker {
        val f = CredentialFixture(); f.boot()
        f.exportFailures = 100
        f.tick()
        val before = f.sent.count { it.first == "watch:request" }
        f.pair.mailbox.unreachableNodes.add("synthetic-left-node")
        repeat(100) { f.watch.opportunity() }
        assertEquals(1, f.queue.size)
        f.drain()
        f.pair.mailbox.unreachableNodes.clear()
        repeat(100) { f.watch.opportunity() }
        assertEquals(1, f.queue.size)
        f.drain()
        assertEquals(before, f.sent.count { it.first == "watch:request" }) // Active attempts are not reset.
        f.until { f.clock.live().isEmpty() }
        assertEquals(2 * ChatGptAccessSync.Owner.MAX_ATTEMPTS,
            f.sent.count { it.first == "watch:request" } - 1) // Initial successful boot is separate.
        val exhausted = f.sent.size
        repeat(100) { f.watch.opportunity() }
        f.drain()
        assertEquals(exhausted, f.sent.size)
    }

    @Test fun secondObservedReconnectRecoversLostAckWithOriginalExactReceipt() = worker {
        val f = CredentialFixture()
        f.expires = f.clock.now + 3_600_000
        f.dropSend["watch:ack"] = 100
        f.boot()
        val committed = f.watchFile.readBytes()
        val receipt = f.sent.last { it.first == "watch:ack" }.second
        f.until { f.clock.live("phone").isEmpty() }
        fun reconnect() {
            f.pair.mailbox.unreachableNodes.add("synthetic-right-node")
            f.phone.opportunity(); f.drain()
            f.pair.mailbox.unreachableNodes.clear()
            f.phone.opportunity(); f.drain()
        }
        reconnect()
        f.until { f.clock.live("phone").isEmpty() }
        assertEquals(2 * ChatGptAccessSync.Owner.MAX_ATTEMPTS, f.sent.count { it.first == "phone:hint" })
        val exhausted = f.sent.size
        repeat(100) { f.phone.opportunity() }
        f.drain()
        assertEquals(exhausted, f.sent.size)
        assertTrue(f.clock.live("phone").isEmpty())
        f.dropSend["watch:ack"] = 0
        reconnect()
        f.until { f.clock.live("phone").isEmpty() }
        assertEquals(2 * ChatGptAccessSync.Owner.MAX_ATTEMPTS + 1, f.sent.count { it.first == "phone:hint" })
        assertArrayEquals(receipt, f.sent.last { it.first == "watch:ack" }.second)
        assertArrayEquals(committed, f.watchFile.readBytes())
    }

    @Test fun onlyCurrentApprovedPeerAndExactPublicationReceiptCanRetirePhoneRetries() = worker {
        val f = CredentialFixture()
        f.dropSend["watch:ack"] = 1
        f.boot()
        val receipt = f.sent.last { it.first == "watch:ack" }.second
        f.phone.submit { receive("synthetic-unselected-node", "ack", receipt) }
        for (field in listOf("association", "nonce", "generation")) {
            val wrong = JSONObject(receipt.toString(Charsets.UTF_8))
                .put(field, if (field == "generation") 999 else "synthetic-stale")
            f.phone.submit { receive("synthetic-right-node", "ack", wrong.toString().toByteArray()) }
        }
        f.drain()
        assertEquals(1, f.clock.live("phone").size)
        f.phone.submit { receive("synthetic-right-node", "ack", receipt) }
        f.drain()
        assertTrue(f.clock.live("phone").isEmpty())
        assertEquals(1, f.clock.live("watch").size) // Normal future expiry only.
    }

    @Test fun revocationFencesScheduledRenewalRetryAndClearsWatchAccess() = worker {
        val f = CredentialFixture(); f.boot()
        f.failSend["watch:request"] = 1
        f.tick()
        EnrolledPeerRegistry(f.pair.rightContext).use { it.revoke(a) }
        assertFalse(f.watch.watchApproved())
        f.tick()
        assertTrue(f.clock.live("watch").isEmpty())
        assertTrue(f.watchState().getBoolean("signedOut"))
        assertFalse(f.watchState().has("openai"))
    }

    @Test fun reconciliationKeepsActualSelectedRouteApprovedThroughDuplexCompletionAndRetry() = worker {
        val pair = PairFixture(); pair.link()
        val route = requireNotNull(pair.left.selectedRoute())
        val relayTests = PhoneRelaySessionTest()
        relayTests.Fixture().use { fixture ->
            fixture.routeApproval = route::stillApproved
            val continueProvider = CountDownLatch(1)
            fixture.native.run = { callback ->
                assertTrue(continueProvider.await(5, TimeUnit.SECONDS))
                callback.onDelta("Synthetic reconciled answer")
                """{"status":"complete","text":"Synthetic reconciled answer"}"""
            }
            val response = fixture.start()
            assertTrue(fixture.native.entered.await(5, TimeUnit.SECONDS))
            repeat(3) { assertEquals(PeerPairingReason.ENROLLED, pair.left.retry(a, pair.leftIo).reason) }
            assertTrue(route.stillApproved())
            continueProvider.countDown()
            assertEquals("complete", JSONObject(response.get(5, TimeUnit.SECONDS)).getString("status"))
            fixture.retireAndAdmitNext()
            EnrolledPeerRegistry(pair.leftContext).use { it.revoke(b) }
            assertFalse(route.stillApproved())
        }
    }

    @Test fun selectedRelayRouteRejectsAmbiguityWrongRoleAndOfflineNodes() = worker {
        val pair = PairFixture(); pair.link()
        pair.mailbox.unreachableNodes.add("synthetic-right-node")
        assertNull(pair.left.selectedRoute())
        pair.mailbox.unreachableNodes.clear()
        pair.publish(pair.rightIo, b, "phone")
        assertNull(pair.left.selectedRoute())
        pair.publish(pair.rightIo, b, "watch")
        pair.publish(GmsReplicationTransport(pair.mailbox.sdk("synthetic-extra-watch"), budget()), c, "watch")
        assertNull(pair.left.selectedRoute())
    }

    @Test fun realReconciliationChangesAndFailuresInvalidatePreviouslySelectedRoutes() = worker {
        val pair = PairFixture(); pair.link()
        var route = requireNotNull(pair.left.selectedRoute())
        EnrolledPeerRegistry(pair.leftContext).use { it.enroll(b) }
        assertTrue(route.stillApproved())
        pair.publish(pair.rightIo, b, "phone")
        assertEquals(PeerPairingReason.WAITING_FOR_PEER, pair.left.retry(a, pair.leftIo).reason)
        assertFalse(route.stillApproved())
        pair.publish(pair.rightIo, b, "watch")
        pair.left.retry(a, pair.leftIo)
        route = requireNotNull(pair.left.selectedRoute())
        val extra = GmsReplicationTransport(pair.mailbox.sdk("synthetic-extra-watch"), budget())
        pair.publish(extra, c, "watch")
        assertEquals(PeerPairingReason.MULTIPLE_PEERS, pair.left.retry(a, pair.leftIo).reason)
        assertFalse(route.stillApproved())
        pair.mailbox.nodes.remove("synthetic-extra-watch")
        pair.left.retry(a, pair.leftIo)
        route = requireNotNull(pair.left.selectedRoute())
        pair.leftSdk.capabilityError = IllegalStateException("Synthetic discovery failure")
        assertEquals(PeerPairingReason.DISCOVERY_FAILED, pair.left.retry(a, pair.leftIo).reason)
        assertFalse(route.stillApproved())
        pair.leftSdk.capabilityError = null
        pair.left.retry(a, pair.leftIo)
        route = requireNotNull(pair.left.selectedRoute())
        pair.publish(pair.rightIo, c, "watch")
        assertEquals(PeerPairingReason.ENROLLED, pair.left.retry(a, pair.leftIo).reason)
        assertFalse(route.stillApproved())
        route = requireNotNull(pair.left.selectedRoute())
        pair.left.recordFailure(PeerPairingReason.STORAGE_FAILURE)
        assertFalse(route.stillApproved())
    }

    @Test fun identityHintsBootstrapAndOldInvitationPathsAreNeverQueued() = worker {
        val pair = PairFixture()
        assertTrue(AssistantReplication.acceptsData(pair.mailbox.events.last()))
        for (kind in listOf("request", "ack")) {
            pair.leftIo.publish("/voxgolem/assistant/v1/pair/$kind", "{}".toByteArray(), emptyMap())
            assertFalse(AssistantReplication.acceptsData(pair.mailbox.events.last()))
        }
    }

    @Test fun persistedInitialBackoffIsBoundedWithoutAnyEnrolledPeer() {
        val context = isolated("synthetic-headless-backoff")
        val now = 1_800_000_000_000L
        val delay = DurableReplicationState(context).use { it.failed(now, kotlin.random.Random(7)) }
        DurableReplicationState(context).use { state ->
            assertEquals(delay, state.retryDelay(now))
            repeat(12) { assertTrue(state.failed(now, kotlin.random.Random(7)) in 30_000..DurableReplicationState.MAX_BACKOFF) }
        }
    }

    private class CredentialClock {
        class Entry(val owner: String, val work: Runnable, val due: Long, var removed: Boolean = false)
        var now = 1_800_000_000_000L
        private val entries = mutableListOf<Entry>()
        fun live(owner: String? = null) = entries.filter { !it.removed && (owner == null || it.owner == owner) }
        fun scheduler(owner: String): (Runnable, Long) -> (() -> Unit) = { work, delay ->
            val entry = Entry(owner, work, now + delay).also(entries::add)
            val cancel: () -> Unit = { entry.removed = true }
            cancel
        }
        fun tick() {
            val entry = live().minBy { it.due }
            entry.removed = true
            now = maxOf(now, entry.due)
            entry.work.run()
        }
    }

    private inner class CredentialFixture {
        val pair = PairFixture().apply { link() }
        val clock = CredentialClock()
        val queue = ArrayDeque<Runnable>()
        val failSend = mutableMapOf<String, Int>()
        val dropSend = mutableMapOf<String, Int>()
        val sent = mutableListOf<Pair<String, ByteArray>>()
        var token = "synthetic-original-access"
        var expires = clock.now + 120_000
        var exportFailures = 0
        val phoneFile = ChatGptAuth.managedFile(pair.leftContext).apply { writeText("{}") }
        val watchFile = ChatGptAuth.managedFile(pair.rightContext)
        val phone: ChatGptAccessSync.Owner
        val watch: ChatGptAccessSync.Owner
        init {
            val executor = Executor { queue.add(it) }
            phone = ChatGptAccessSync.Owner(pair.leftContext, pair.left, export = {
                if (exportFailures > 0) { exportFailures--; """{"error":"synthetic-transient-failure"}""" }
                else if (JSONObject(phoneFile.readText()).optBoolean("signedOut")) "null"
                else JSONObject().put("voxgolemChatgpt", 1).put("openai", JSONObject().put("access", token).put("expires", expires)).toString()
            }, message = { _, kind, bytes -> transmit("phone", kind, bytes) }, executor = executor,
                schedule = clock.scheduler("phone"), now = { clock.now })
            watch = ChatGptAccessSync.Owner(pair.rightContext, pair.right, export = { error("Watch cannot export") },
                message = { _, kind, bytes -> transmit("watch", kind, bytes) }, executor = executor,
                schedule = clock.scheduler("watch"), now = { clock.now })
        }
        private fun transmit(from: String, kind: String, bytes: ByteArray) {
            val key = "$from:$kind"
            sent.add(key to bytes.copyOf())
            if ((failSend[key] ?: 0) > 0) { failSend[key] = failSend.getValue(key) - 1; error("synthetic_send_failed") }
            if ((dropSend[key] ?: 0) > 0) { dropSend[key] = dropSend.getValue(key) - 1; return }
            if (from == "phone") watch.submit { receive("synthetic-left-node", kind, bytes) }
            else phone.submit { receive("synthetic-right-node", kind, bytes) }
        }
        fun drain() {
            var count = 0
            while (queue.isNotEmpty()) { check(++count <= 100); queue.removeFirst().run() }
        }
        fun boot() { phone.changed(); drain(); assertEquals("synthetic-original-access", access()) }
        fun tick() { clock.tick(); drain() }
        fun until(done: () -> Boolean) {
            repeat(25) { if (done()) return; tick() }
            assertTrue("Bounded synthetic exchange did not settle", done())
        }
        fun watchState() = JSONObject(watchFile.readText())
        fun access(): String? = watchState().optJSONObject("openai")?.optString("access")
    }

    private inner class PairFixture {
        val leftContext = isolated("synthetic-auto-phone")
        val rightContext = isolated("synthetic-auto-watch")
        val mailbox = FakeGmsMailbox(folder.root)
        val leftSdk = mailbox.sdk("synthetic-left-node")
        val rightSdk = mailbox.sdk("synthetic-right-node")
        val leftIo = GmsReplicationTransport(leftSdk, budget())
        val rightIo = GmsReplicationTransport(rightSdk, budget())
        val left = setup(leftContext, a, leftSdk, false)
        val right = setup(rightContext, b, rightSdk, true)
        fun setup(context: Context, replica: String, sdk: GmsReplicationSdk, watch: Boolean): AssistantPeerSetup {
            shadowOf(context.packageManager).setSystemFeature(PackageManager.FEATURE_WATCH, watch)
            return AssistantPeerSetup(context, { replica }) { GmsReplicationTransport(sdk, budget()) }
        }
        fun link() { left.retry(a, leftIo); right.retry(b, rightIo) }
        fun publish(io: GmsReplicationTransport, replica: String, role: String?, version: Int = 3) {
            val hello = JSONObject().put("version", version).put("replica", replica)
            if (version == 2) hello.put("installation", c)
            if (role != null) hello.put("role", role)
            io.publish(ReplicationWire.IDENTITY_PATH, hello.toString().toByteArray(), emptyMap())
        }
        init { publish(leftIo, a, "phone"); publish(rightIo, b, "watch") }
    }
    private fun isolated(name: String): Context {
        val root = folder.newFolder(name)
        val files = folder.newFolder("$name-files")
        return object : ContextWrapper(RuntimeEnvironment.getApplication()) {
            override fun getApplicationContext(): Context = this
            override fun getNoBackupFilesDir(): File = root
            override fun getFilesDir(): File = files
        }
    }
    private fun worker(block: () -> Unit) {
        val executor = Executors.newSingleThreadExecutor()
        try { executor.submit(block).get(60, TimeUnit.SECONDS) } finally { executor.shutdownNow() }
    }
    private fun budget() = BoundedGmsWait(System.nanoTime() + TimeUnit.SECONDS.toNanos(30))
}
