package dev.gitblame.voxgolem.assistant.replication

import org.json.JSONArray
import org.json.JSONObject
import java.util.ArrayDeque
import java.util.UUID

/** Connected-session catch-up controller backed by canonical SQLite truth. */
internal class ReplicationController(
    private val store: ReplicationStore,
    private val domain: ReplicationDomain,
    private val state: ReplicationState,
    private val transport: ReplicationTransport,
    private val association: (String) -> String?,
    private val newToken: () -> String = { UUID.randomUUID().toString() },
    private val checkBudget: () -> Unit = {},
    private val onRejected: (Exception) -> Unit = {},
    private val role: AssistantPeerRole = AssistantPeerRole.PHONE,
) {
    fun reconcile() {
        val peers = linkedMapOf<String, String>()
        val enrolled = state.enrolledReplicas()
        for (node in transport.reachable().take(64)) {
            checkBudget()
            val peer = association(node.nodeId)
            checkBudget()
            if (peer != null && peer != store.replica && peer in enrolled && peer !in peers) {
                peers[peer] = node.nodeId
            }
            if (peers.size == ReplicationWire.MAX_PEERS) break
        }
        for ((peer, nodeId) in peers) {
            checkBudget()
            if (role == AssistantPeerRole.WATCH) {
                try {
                    transport.sendHint(nodeId, store.replica, peer)
                } catch (error: Exception) {
                    onRejected(error)
                }
            } else if (transport is GmsReplicationTransport && store.nativeRead(JSONObject().put("kind", "revision")) == null) {
                reconcileLegacy(peer)
            } else {
                connect(nodeId, peer)
            }
        }
    }

    fun onHint(node: String, bytes: ByteArray): Boolean {
        return try {
            checkBudget()
            val peer = association(node) ?: return false
            checkBudget()
            if (peer == store.replica || peer !in state.enrolledReplicas()) return false
            ReplicationWire.validateHint(bytes, store.replica, peer)
            if (role == AssistantPeerRole.PHONE) return connect(node, peer)
            true
        } catch (error: Exception) {
            onRejected(error)
            false
        }
    }

    fun connect(node: String, expectedPeer: String? = null): Boolean {
        return try {
            checkBudget()
            val peer = association(node) ?: return false
            checkBudget()
            if (expectedPeer != null && peer != expectedPeer) return false
            if (peer == store.replica || peer !in state.enrolledReplicas()) return false
            transport.openSession(node, newToken()).use { connection ->
                ReplicationWire.uuid(connection.session)
                require(connection.path == ReplicationWire.sessionPath(connection.session))
                runConnectedSession(connection, peer, initiator = true)
            }
            true
        } catch (error: Exception) {
            onRejected(error)
            false
        }
    }

    fun accept(connection: ReplicationSessionConnection): Boolean {
        return try {
            connection.use {
                checkBudget()
                val peer = association(connection.nodeId) ?: return false
                checkBudget()
                if (peer == store.replica || peer !in state.enrolledReplicas()) return false
                ReplicationWire.uuid(connection.session)
                require(connection.path == ReplicationWire.sessionPath(connection.session))
                runConnectedSession(connection, peer, initiator = false)
                true
            }
        } catch (error: Exception) {
            onRejected(error)
            false
        }
    }

    private fun runConnectedSession(connection: ReplicationSessionConnection, peer: String, initiator: Boolean) {
        if (initiator) {
            pullFromPeer(connection, peer)
            serveToPeer(connection, peer)
        } else {
            serveToPeer(connection, peer)
            pullFromPeer(connection, peer)
        }
    }

    private fun pullFromPeer(connection: ReplicationSessionConnection, peer: String) {
        var totalPages = 0
        var phase = "tombstones"
        var pinnedRevision: String? = null
        var cursor: JSONObject? = null
        val ancestorAttempts = LinkedHashSet<String>()

        while (true) {
            checkBudget()
            require(peer in state.enrolledReplicas() && association(connection.nodeId) == peer)
            require(++totalPages <= ReplicationWire.MAX_SESSION_PAGES)

            val requestToken = newToken()
            connection.writeFrame(
                ReplicationWire.sessionFrame(
                    store.replica,
                    peer,
                    connection.session,
                    requestToken,
                    "inventory_request",
                    JSONObject().put("phase", phase).put("cursor", cursor ?: JSONObject.NULL),
                )
            )
            checkBudget()
            val response = connection.readFrame()
            val responsePayload = ReplicationWire.validateSessionFrame(response, connection.session, store.replica, peer)
            require(response.getString("token") == requestToken)
            when (response.getString("kind")) {
                "restart" -> {
                    require(responsePayload.length() == 0)
                    phase = "tombstones"
                    pinnedRevision = null
                    cursor = null
                    continue
                }
                "inventory" -> Unit
                else -> error("Unexpected replication inventory response kind")
            }

            require(responsePayload.length() == 1)
            val page = responsePayload.getJSONObject("page")
            require(page.length() == 4 && page.get("version") == 2)
            val entries = page.getJSONArray("entries")
            ReplicationWire.entries(entries)
            val revision = page.getString("revision")
            val currentCursor = cursor
            if (currentCursor != null && !currentCursor.isNull("after")) {
                val after = currentCursor.getJSONObject("after")
                require(entries.length() == 0 || compareId(entries.getJSONObject(0).getJSONObject("id"), after) > 0)
            }
            if (!page.isNull("next")) {
                val next = page.getJSONObject("next")
                require(next.length() == 2 && next.getString("revision") == revision && entries.length() > 0)
                require(
                    ReplicationWire.idKey(next.getJSONObject("after")) ==
                        ReplicationWire.idKey(entries.getJSONObject(entries.length() - 1).getJSONObject("id"))
                )
            }
            if (pinnedRevision != null && revision != pinnedRevision) {
                phase = "tombstones"
                pinnedRevision = null
                cursor = null
                continue
            }
            if (currentCursor != null) {
                require(currentCursor.getString("revision") == revision)
            }
            pinnedRevision = revision

            val reconciled = reconcilePage(page)
            val plan = reconciled.getJSONObject("plan")
            val knownReceipts = reconciled.getJSONArray("receipts")
            if (knownReceipts.length() > 0) {
                sendAckAndConfirm(connection, peer, knownReceipts)
            }

            val fetch = if (phase == "tombstones") plan.getJSONArray("fetch_tombstones") else plan.getJSONArray("fetch_records")
            val remaining = ArrayDeque<JSONObject>(ReplicationWire.arrayObjects(fetch))
            if (phase == "records") {
                appendMissingAncestors(plan.getJSONArray("missing_ancestors"), remaining, ancestorAttempts)
            }

            var restarted = false
            while (remaining.isNotEmpty()) {
                checkBudget()
                require(peer in state.enrolledReplicas() && association(connection.nodeId) == peer)
                val batchEntries = JSONArray()
                while (batchEntries.length() < ReplicationWire.MAX_BATCH_RECORDS && remaining.isNotEmpty()) {
                    batchEntries.put(remaining.removeFirst())
                }
                val batchToken = newToken()
                connection.writeFrame(
                    ReplicationWire.sessionFrame(
                        store.replica,
                        peer,
                        connection.session,
                        batchToken,
                        "record_request",
                        JSONObject()
                            .put("revision", revision)
                            .put("phase", phase)
                            .put("entries", batchEntries),
                    )
                )
                checkBudget()
                val batchResponse = connection.readFrame()
                val batchPayload = ReplicationWire.validateSessionFrame(batchResponse, connection.session, store.replica, peer)
                require(batchResponse.getString("token") == batchToken)
                when (batchResponse.getString("kind")) {
                    "restart" -> {
                        require(batchPayload.length() == 0)
                        phase = "tombstones"
                        pinnedRevision = null
                        cursor = null
                        restarted = true
                        break
                    }
                    "records" -> Unit
                    else -> error("Unexpected replication record response kind")
                }

                val batch = ReplicationWire.readRecordBatch(
                    connection = connection,
                    recordsFrame = batchResponse,
                    session = connection.session,
                    local = store.replica,
                    peer = peer,
                    expectedEntries = batchEntries,
                    phase = phase,
                    checkBudget = checkBudget,
                )
                if (batch.records.length() > 0) {
                    require(peer in state.enrolledReplicas() && association(connection.nodeId) == peer)
                    checkBudget()
                    val receipts = store.importVerified(batch.records, batch.verifiedEntries)
                        ?: store.durableReceipts(batch.ids, checkBudget)
                        ?: exactReceipts(batch.ids)
                    validateLocalReceipts(receipts, batch.verifiedEntries)
                    if (phase == "records") {
                        requestNewAncestors(revision, remaining, ancestorAttempts)
                    }
                    sendAckAndConfirm(connection, peer, receipts)
                }
            }
            if (restarted) continue

            if (!page.isNull("next")) {
                cursor = page.getJSONObject("next")
                continue
            }
            if (phase == "tombstones") {
                phase = "records"
                cursor = JSONObject().put("revision", revision).put("after", JSONObject.NULL)
                continue
            }
            checkBudget()
            connection.writeFrame(
                ReplicationWire.sessionFrame(
                    store.replica,
                    peer,
                    connection.session,
                    newToken(),
                    "done",
                    JSONObject(),
                )
            )
            return
        }
    }

    private fun validateLocalReceipts(receipts: JSONArray, verifiedEntries: JSONArray) {
        val expectedKeys = ReplicationWire.arrayObjects(verifiedEntries)
            .associateBy { ReplicationWire.idKey(it.getJSONObject("id")) }
        require(receipts.length() == expectedKeys.size)
        val receiptIds = HashSet<String>()
        for (receipt in ReplicationWire.arrayObjects(receipts)) {
            val key = ReplicationWire.idKey(receipt.getJSONObject("operation"))
            val expectedEntry = expectedKeys.getValue(key)
            require(
                receiptIds.add(key) &&
                    receipt.length() == 4 &&
                    receipt.get("version") == 2 &&
                    receipt.getString("recipient_replica") == store.replica &&
                    ReplicationWire.bodyKey(receipt.getString("body_key")) == expectedEntry.getString("body_key")
            )
        }
    }

    private fun appendMissingAncestors(
        missingAncestors: JSONArray,
        remaining: ArrayDeque<JSONObject>,
        ancestorAttempts: MutableSet<String>,
    ) {
        val present = remaining.map { ReplicationWire.idKey(it.getJSONObject("id")) }.toMutableSet()
        for (id in ReplicationWire.arrayObjects(missingAncestors)) {
            val key = ReplicationWire.idKey(id)
            if (ancestorAttempts.size >= ReplicationWire.MAX_ITEMS || remaining.size >= ReplicationWire.MAX_ITEMS) break
            if (key !in present && ancestorAttempts.add(key)) {
                present.add(key)
                remaining.addLast(JSONObject().put("id", id).put("body_key", JSONObject.NULL))
            }
        }
    }

    private fun requestNewAncestors(
        revision: String,
        remaining: ArrayDeque<JSONObject>,
        ancestorAttempts: MutableSet<String>,
    ) {
        val page = JSONObject()
            .put("version", 2)
            .put("revision", revision)
            .put("entries", JSONArray())
            .put("next", JSONObject.NULL)
        val missing = reconcilePage(page).getJSONObject("plan").getJSONArray("missing_ancestors")
        appendMissingAncestors(missing, remaining, ancestorAttempts)
    }

    private fun sendAckAndConfirm(connection: ReplicationSessionConnection, peer: String, receipts: JSONArray) {
        checkBudget()
        val ackToken = newToken()
        connection.writeFrame(
            ReplicationWire.sessionFrame(
                store.replica,
                peer,
                connection.session,
                ackToken,
                "ack",
                JSONObject().put("receipts", receipts),
            )
        )
        checkBudget()
        val confirmFrame = connection.readFrame()
        val payload = ReplicationWire.validateSessionFrame(confirmFrame, connection.session, store.replica, peer)
        require(confirmFrame.getString("token") == ackToken && confirmFrame.getString("kind") == "ack_confirmed")
        require(payload.length() == 1)
        val confirmed = payload.getJSONArray("receipts")
        val expected = ReplicationWire.arrayObjects(receipts)
            .associateBy { ReplicationWire.idKey(it.getJSONObject("operation")) }
        require(confirmed.length() == expected.size)
        val seen = HashSet<String>()
        for (receipt in ReplicationWire.arrayObjects(confirmed)) {
            require(receipt.length() == 4 && receipt.get("version") == 2)
            val key = ReplicationWire.idKey(receipt.getJSONObject("operation"))
            val expectedReceipt = expected.getValue(key)
            require(
                seen.add(key) &&
                    receipt.getString("recipient_replica") == peer &&
                    ReplicationWire.bodyKey(receipt.getString("body_key")) == expectedReceipt.getString("body_key")
            )
        }
        require(peer in state.enrolledReplicas() && association(connection.nodeId) == peer)
        checkBudget()
        store.acknowledge(peer, confirmed)
    }

    private fun serveToPeer(connection: ReplicationSessionConnection, peer: String) {
        while (true) {
            checkBudget()
            val request = connection.readFrame()
            checkBudget()
            require(peer in state.enrolledReplicas() && association(connection.nodeId) == peer)
            val payload = ReplicationWire.validateSessionFrame(request, connection.session, store.replica, peer)
            val token = request.getString("token")
            when (request.getString("kind")) {
                "inventory_request" -> serveInventory(connection, peer, token, payload)
                "record_request" -> serveRecords(connection, peer, token, payload)
                "ack" -> serveAck(connection, peer, token, payload)
                "done" -> {
                    require(payload.length() == 0)
                    return
                }
                else -> error("Unexpected replication session request kind")
            }
        }
    }

    private fun serveInventory(
        connection: ReplicationSessionConnection,
        peer: String,
        token: String,
        payload: JSONObject,
    ) {
        require(payload.length() == 2 && payload.getString("phase") in setOf("tombstones", "records"))
        val native = store.nativeRead(JSONObject().put("kind", "inventory").put("cursor", payload.get("cursor"))) as JSONObject?
        if (native != null) {
            if (native.optBoolean("restart", false)) {
                connection.writeFrame(ReplicationWire.sessionFrame(store.replica, peer, connection.session, token, "restart", JSONObject()))
            } else {
                connection.writeFrame(
                    ReplicationWire.sessionFrame(store.replica, peer, connection.session, token, "inventory", JSONObject().put("page", native))
                )
            }
            return
        }
        val snapshot = store.snapshot()
        val cursor = if (payload.isNull("cursor")) {
            JSONObject().put("revision", snapshot.getString("revision")).put("after", JSONObject.NULL)
        } else {
            payload.getJSONObject("cursor")
        }
        if (cursor.getString("revision") != snapshot.getString("revision")) {
            connection.writeFrame(ReplicationWire.sessionFrame(store.replica, peer, connection.session, token, "restart", JSONObject()))
            return
        }
        val page = domain.inventory(snapshot, cursor)
        connection.writeFrame(
            ReplicationWire.sessionFrame(store.replica, peer, connection.session, token, "inventory", JSONObject().put("page", page))
        )
    }

    private fun serveRecords(
        connection: ReplicationSessionConnection,
        peer: String,
        token: String,
        payload: JSONObject,
    ) {
        require(payload.length() == 3 && payload.getString("phase") in setOf("tombstones", "records"))
        val entries = payload.getJSONArray("entries")
        ReplicationWire.entries(entries)
        require(entries.length() in 1..ReplicationWire.MAX_BATCH_RECORDS)
        val native = store.nativeRead(
            JSONObject()
                .put("kind", "bodies")
                .put("revision", payload.getString("revision"))
                .put("entries", entries)
                .put("tombstones", payload.getString("phase") == "tombstones")
        ) as JSONObject?
        if (native != null) {
            if (native.getBoolean("restart")) {
                connection.writeFrame(ReplicationWire.sessionFrame(store.replica, peer, connection.session, token, "restart", JSONObject()))
                return
            }
            val preparedTransfers = mutableListOf<PreparedRecordTransfer>()
            for (body in ReplicationWire.arrayObjects(native.getJSONArray("bodies"))) {
                checkBudget()
                val raw = body.getString("body").toByteArray(Charsets.UTF_8)
                preparedTransfers.add(PreparedRecordTransfer(body.getJSONObject("entry"), raw))
            }
            if (store.nativeRead(JSONObject().put("kind", "revision")) != payload.getString("revision")) {
                connection.writeFrame(ReplicationWire.sessionFrame(store.replica, peer, connection.session, token, "restart", JSONObject()))
                return
            }
            ReplicationWire.writeRecordBatch(
                connection = connection,
                sender = store.replica,
                target = peer,
                session = connection.session,
                token = token,
                transfer = newToken(),
                bodies = preparedTransfers,
                unavailable = native.getJSONArray("unavailable"),
                checkBudget = checkBudget,
            )
            return
        }
        val snapshot = store.snapshot()
        if (payload.getString("revision") != snapshot.getString("revision")) {
            connection.writeFrame(ReplicationWire.sessionFrame(store.replica, peer, connection.session, token, "restart", JSONObject()))
            return
        }
        val retained = outgoing(snapshot)
        val preparedTransfers = mutableListOf<PreparedRecordTransfer>()
        val unavailable = JSONArray()
        var bytes = 0
        for (entry in ReplicationWire.arrayObjects(entries)) {
            checkBudget()
            val key = ReplicationWire.idKey(entry.getJSONObject("id"))
            val record = retained[key]
            if (record == null) {
                unavailable.put(entry.getJSONObject("id"))
                continue
            }
            val descriptor = descriptor(record)
            if (!entry.isNull("body_key")) require(sameEntry(entry, descriptor))
            if (payload.getString("phase") == "tombstones") require(descriptor.getBoolean("tombstone"))
            val raw = record.toString().toByteArray(Charsets.UTF_8)
            require(raw.size <= ReplicationWire.MAX_RECORD_BYTES)
            if (bytes + raw.size > ReplicationWire.MAX_BATCH_BYTES) {
                unavailable.put(entry.getJSONObject("id"))
                continue
            }
            bytes += raw.size
            preparedTransfers.add(PreparedRecordTransfer(descriptor, raw))
        }
        if (store.snapshot().getString("revision") != snapshot.getString("revision")) {
            connection.writeFrame(ReplicationWire.sessionFrame(store.replica, peer, connection.session, token, "restart", JSONObject()))
            return
        }
        ReplicationWire.writeRecordBatch(
            connection = connection,
            sender = store.replica,
            target = peer,
            session = connection.session,
            token = token,
            transfer = newToken(),
            bodies = preparedTransfers,
            unavailable = unavailable,
            checkBudget = checkBudget,
        )
    }

    private fun serveAck(
        connection: ReplicationSessionConnection,
        peer: String,
        token: String,
        payload: JSONObject,
    ) {
        require(payload.length() == 1)
        val receipts = payload.getJSONArray("receipts")
        require(receipts.length() in 1..ReplicationWire.MAX_ITEMS)
        val seen = HashSet<String>()
        for (receipt in ReplicationWire.arrayObjects(receipts)) {
            require(receipt.length() == 4 && receipt.get("version") == 2 && receipt.getString("recipient_replica") == peer)
            require(seen.add(ReplicationWire.idKey(receipt.getJSONObject("operation"))))
            ReplicationWire.bodyKey(receipt.getString("body_key"))
        }
        checkBudget()
        store.acknowledge(peer, receipts)
        val ids = JSONArray(ReplicationWire.arrayObjects(receipts).map { it.getJSONObject("operation") })
        connection.writeFrame(
            ReplicationWire.sessionFrame(
                store.replica,
                peer,
                connection.session,
                token,
                "ack_confirmed",
                JSONObject().put("receipts", exactReceipts(ids)),
            )
        )
    }

    private fun reconcileLegacy(peer: String) {
        val saved = state.load(peer)
        if (saved == null) begin(peer) else send(saved.getJSONObject("pending"))
        val native = store.nativeRead(JSONObject().put("kind", "inventory").put("cursor", JSONObject.NULL)) as JSONObject?
        val snapshot = if (native == null) store.snapshot() else null
        val retained = snapshot?.let(::outgoing)
        val offers = if (native != null) ReplicationWire.arrayObjects(native.getJSONArray("entries")).take(32)
            else ReplicationWire.arrayObjects(store.publications(peer, null, 32))
                .filter { ReplicationWire.idKey(it.getJSONObject("operation").getJSONObject("id")) in retained!! }
                .map(::descriptor).sortedWith { a, b -> compareId(a.getJSONObject("id"), b.getJSONObject("id")) }
        send(ReplicationWire.frame(store.replica, peer, newToken(), newToken(), "hint", JSONObject()
            .put("revision", (native ?: snapshot!!).getString("revision")).put("offers", JSONArray(offers))))
    }

    fun onData(node: String, path: String, bytes: ByteArray, assets: Map<String, ByteArray>): Boolean =
        onData(node, path, bytes) { name, max -> assets[name]?.also { require(it.size <= max) } ?: error("Missing record Asset") }

    fun onData(node: String, path: String, bytes: ByteArray, readAsset: (String, Int) -> ByteArray): Boolean {
        return try {
            checkBudget()
            val peer = association(node) ?: return false
            checkBudget()
            if (peer == store.replica || peer !in state.enrolledReplicas()) return false
            val frame = ReplicationWire.decode(bytes)
            ReplicationWire.validateFrame(frame, path, store.replica, peer)
            when (frame.getString("kind")) {
                "hint" -> {
                    val payload = frame.getJSONObject("payload")
                    if (payload.length() != 0) {
                        require(payload.length() == 2)
                        val offers = payload.getJSONArray("offers")
                        ReplicationWire.entries(offers)
                        reconcilePage(JSONObject().put("version", 2).put("revision", payload.getString("revision"))
                            .put("entries", offers).put("next", JSONObject.NULL))
                    }
                    if (state.load(peer) == null) begin(peer)
                }
                "inventory_request" -> respondInventory(frame)
                "inventory" -> receiveInventory(frame)
                "record_request" -> respondRecords(frame)
                "records" -> receiveRecords(frame, readAsset, node)
                "ack" -> receiveAck(frame)
                "ack_confirmed" -> {
                    val session = matches(frame, "ack") ?: return true
                    val payload = frame.getJSONObject("payload")
                    require(payload.length() == 1)
                    val confirmed = payload.getJSONArray("receipts")
                    val original = session.getJSONObject("pending").getJSONObject("payload").getJSONArray("receipts")
                    val expected = ReplicationWire.arrayObjects(original).associateBy { ReplicationWire.idKey(it.getJSONObject("operation")) }
                    require(confirmed.length() == expected.size)
                    val seen = HashSet<String>()
                    for (receipt in ReplicationWire.arrayObjects(confirmed)) {
                        val key = ReplicationWire.idKey(receipt.getJSONObject("operation"))
                        require(seen.add(key) && receipt.getString("recipient_replica") == peer &&
                            receipt.getString("body_key") == expected.getValue(key).getString("body_key"))
                    }
                    store.acknowledge(peer, confirmed)
                    advance(peer, session)
                }
                "restart" -> {
                    require(frame.getJSONObject("payload").length() == 0)
                    if (matches(frame) != null) begin(peer)
                }
            }
            true
        } catch (error: Exception) { onRejected(error); false }
    }

    private fun begin(peer: String) {
        val session = JSONObject().put("session", newToken()).put("phase", "tombstones")
            .put("revision", JSONObject.NULL).put("pages", 0).put("ancestor_attempts", JSONArray())
        requestInventory(peer, session, null)
    }

    private fun requestInventory(peer: String, session: JSONObject, cursor: JSONObject?) {
        val payload = JSONObject().put("phase", session.getString("phase")).put("cursor", cursor ?: JSONObject.NULL)
        pending(peer, session, "inventory_request", payload)
    }

    private fun pending(peer: String, session: JSONObject, kind: String, payload: JSONObject) {
        val frame = ReplicationWire.frame(store.replica, peer, session.getString("session"), newToken(), kind, payload)
        session.put("pending", frame)
        state.save(peer, session) // Crash before publication is retried from this exact request.
        send(frame)
    }

    private fun respondInventory(request: JSONObject) {
        val payload = request.getJSONObject("payload")
        require(payload.length() == 2 && payload.getString("phase") in setOf("tombstones", "records"))
        val native = store.nativeRead(JSONObject().put("kind", "inventory").put("cursor", payload.get("cursor"))) as JSONObject?
        if (native != null) {
            if (native.optBoolean("restart", false)) { reply(request, "restart", JSONObject()); return }
            reply(request, "inventory", JSONObject().put("page", native))
            return
        }
        val snapshot = store.snapshot()
        val cursor = if (payload.isNull("cursor")) JSONObject().put("revision", snapshot.getString("revision")).put("after", JSONObject.NULL)
            else payload.getJSONObject("cursor")
        if (cursor.getString("revision") != snapshot.getString("revision")) { reply(request, "restart", JSONObject()); return }
        val page = domain.inventory(snapshot, cursor)
        reply(request, "inventory", JSONObject().put("page", page))
    }

    private fun receiveInventory(frame: JSONObject) {
        val session = matches(frame, "inventory_request") ?: return
        val payload = frame.getJSONObject("payload")
        require(payload.length() == 1)
        val page = payload.getJSONObject("page")
        require(page.length() == 4 && page.get("version") == 2)
        val entries = page.getJSONArray("entries")
        ReplicationWire.entries(entries)
        val peer = frame.getString("sender")
        val revision = page.getString("revision")
        if (!session.isNull("revision") && revision != session.getString("revision")) { begin(peer); return }
        val requested = session.getJSONObject("pending").getJSONObject("payload")
        if (!requested.isNull("cursor")) {
            val cursor = requested.getJSONObject("cursor")
            require(cursor.getString("revision") == revision)
            if (!cursor.isNull("after")) {
                val after = cursor.getJSONObject("after")
                require(entries.length() == 0 || compareId(entries.getJSONObject(0).getJSONObject("id"), after) > 0)
            }
        }
        if (!page.isNull("next")) {
            val next = page.getJSONObject("next")
            require(next.length() == 2 && next.getString("revision") == revision && entries.length() > 0)
            require(ReplicationWire.idKey(next.getJSONObject("after")) == ReplicationWire.idKey(entries.getJSONObject(entries.length() - 1).getJSONObject("id")))
        }
        val pages = session.getInt("pages") + 1
        require(pages <= 4096)
        val reconciled = reconcilePage(page)
        val plan = reconciled.getJSONObject("plan")
        val knownReceipts = reconciled.getJSONArray("receipts")
        session.put("revision", revision).put("pages", pages).put("next", page.get("next"))
        val fetch = if (session.getString("phase") == "tombstones") plan.getJSONArray("fetch_tombstones") else plan.getJSONArray("fetch_records")
        val remaining = ReplicationWire.arrayObjects(fetch).toMutableList()
        if (session.getString("phase") == "records") {
            val present = remaining.map { ReplicationWire.idKey(it.getJSONObject("id")) }.toMutableSet()
            val attempts = attemptedAncestors(session)
            for (id in ReplicationWire.arrayObjects(plan.getJSONArray("missing_ancestors"))) {
                val key = ReplicationWire.idKey(id)
                if (attempts.size >= ReplicationWire.MAX_ITEMS || remaining.size >= ReplicationWire.MAX_ITEMS) break
                if (key !in attempts && present.add(key)) {
                    remaining.add(JSONObject().put("id", id).put("body_key", JSONObject.NULL))
                    session.getJSONArray("ancestor_attempts").put(key); attempts.add(key)
                }
                if (remaining.size == ReplicationWire.MAX_ITEMS) break
            }
        }
        session.put("remaining", JSONArray(remaining))
        if (plan.getJSONArray("offer_tombstones").length() > 0) reply(frame, "hint", JSONObject())
        if (knownReceipts.length() > 0) pending(peer, session, "ack", JSONObject().put("receipts", knownReceipts))
        else advance(peer, session)
    }

    private fun advance(peer: String, session: JSONObject) {
        val remaining = session.getJSONArray("remaining")
        if (remaining.length() > 0) {
            val entries = ReplicationWire.arrayObjects(remaining).take(ReplicationWire.MAX_ASSETS)
            session.put("remaining", JSONArray(ReplicationWire.arrayObjects(remaining).drop(entries.size)))
            pending(peer, session, "record_request", JSONObject().put("revision", session.getString("revision"))
                .put("phase", session.getString("phase")).put("entries", JSONArray(entries)))
        } else if (!session.isNull("next")) {
            requestInventory(peer, session, session.getJSONObject("next"))
        } else if (session.getString("phase") == "tombstones") {
            session.put("phase", "records")
            requestInventory(peer, session, JSONObject().put("revision", session.getString("revision")).put("after", JSONObject.NULL))
        } else {
            state.save(peer, null)
        }
    }

    private fun respondRecords(request: JSONObject) {
        val payload = request.getJSONObject("payload")
        require(payload.length() == 3 && payload.getString("phase") in setOf("tombstones", "records"))
        val entries = payload.getJSONArray("entries")
        ReplicationWire.entries(entries)
        require(entries.length() in 1..ReplicationWire.MAX_ASSETS)
        val native = store.nativeRead(JSONObject().put("kind", "bodies").put("revision", payload.getString("revision"))
            .put("entries", entries).put("tombstones", payload.getString("phase") == "tombstones")) as JSONObject?
        if (native != null) {
            if (native.getBoolean("restart")) { reply(request, "restart", JSONObject()); return }
            val descriptors = JSONArray()
            val assets = linkedMapOf<String, ByteArray>()
            for (body in ReplicationWire.arrayObjects(native.getJSONArray("bodies"))) {
                checkBudget()
                val raw = body.getString("body").toByteArray(Charsets.UTF_8)
                val name = "body_${assets.size}"
                assets[name] = raw
                descriptors.put(JSONObject().put("entry", body.getJSONObject("entry")).put("asset", name).put("bytes", raw.size))
            }
            // Kotlin Asset encoding can overlap a disposition commit. Preserve the
            // original last-moment revision fence without exporting another history.
            if (store.nativeRead(JSONObject().put("kind", "revision")) != payload.getString("revision")) {
                reply(request, "restart", JSONObject()); return
            }
            reply(request, "records", JSONObject().put("records", descriptors).put("unavailable", native.getJSONArray("unavailable")), assets)
            return
        }
        val snapshot = store.snapshot()
        if (payload.getString("revision") != snapshot.getString("revision")) { reply(request, "restart", JSONObject()); return }
        val retained = outgoing(snapshot)
        val descriptors = JSONArray()
        val unavailable = JSONArray()
        val assets = linkedMapOf<String, ByteArray>()
        var bytes = 0
        for (entry in ReplicationWire.arrayObjects(entries)) {
            checkBudget()
            val key = ReplicationWire.idKey(entry.getJSONObject("id"))
            val record = retained[key]
            // ACKed records can still be requested; serve the retained canonical body.
            if (record == null) { unavailable.put(entry.getJSONObject("id")); continue }
            val descriptor = descriptor(record)
            if (!entry.isNull("body_key")) require(sameEntry(entry, descriptor))
            if (payload.getString("phase") == "tombstones") require(descriptor.getBoolean("tombstone"))
            val raw = record.toString().toByteArray(Charsets.UTF_8)
            require(raw.size <= ReplicationWire.MAX_RECORD_BYTES)
            if (bytes + raw.size > ReplicationWire.MAX_ASSET_BYTES) { unavailable.put(entry.getJSONObject("id")); continue }
            bytes += raw.size
            val name = "body_${assets.size}"
            assets[name] = raw
            descriptors.put(JSONObject().put("entry", descriptor).put("asset", name).put("bytes", raw.size))
        }
        if (store.snapshot().getString("revision") != snapshot.getString("revision")) { reply(request, "restart", JSONObject()); return }
        reply(request, "records", JSONObject().put("records", descriptors).put("unavailable", unavailable), assets)
    }

    private fun receiveRecords(frame: JSONObject, readAsset: (String, Int) -> ByteArray, node: String) {
        val session = matches(frame, "record_request") ?: return
        val payload = frame.getJSONObject("payload")
        require(payload.length() == 2)
        val transfers = payload.getJSONArray("records")
        val unavailable = payload.getJSONArray("unavailable")
        require(transfers.length() <= ReplicationWire.MAX_ASSETS)
        ReplicationWire.entries(unavailable, descriptors = false)
        val pending = session.getJSONObject("pending").getJSONObject("payload")
        val expected = ReplicationWire.arrayObjects(pending.getJSONArray("entries")).associateBy { ReplicationWire.idKey(it.getJSONObject("id")) }
        val answered = HashSet<String>()
        var bytes = 0
        val records = JSONArray()
        val ids = JSONArray()
        val verifiedEntries = JSONArray()
        for (transfer in ReplicationWire.arrayObjects(transfers)) {
            checkBudget()
            require(transfer.length() == 3)
            val entry = transfer.getJSONObject("entry")
            ReplicationWire.entries(JSONArray().put(entry))
            val key = ReplicationWire.idKey(entry.getJSONObject("id"))
            require(answered.add(key))
            val requested = expected[key] ?: error("Unrequested record")
            if (!requested.isNull("body_key")) require(sameEntry(requested, entry))
            if (session.getString("phase") == "tombstones") require(entry.getBoolean("tombstone"))
            require(transfer.get("bytes") is Int)
            val size = transfer.getInt("bytes")
            require(size in 2..ReplicationWire.MAX_RECORD_BYTES && bytes + size <= ReplicationWire.MAX_ASSET_BYTES)
            val name = transfer.getString("asset")
            require(name == "body_${records.length()}")
            val raw = readAsset(name, size)
            require(raw.size == size)
            bytes += size
            val record = ReplicationWire.decode(raw, ReplicationWire.MAX_RECORD_BYTES)
            require(record.length() == 2 && sameEntry(descriptor(record), entry))
            records.put(record); ids.put(entry.getJSONObject("id")); verifiedEntries.put(entry)
        }
        for (id in ReplicationWire.arrayObjects(unavailable)) require(answered.add(ReplicationWire.idKey(id)) && ReplicationWire.idKey(id) in expected)
        require(answered == expected.keys)
        if (records.length() > 0) {
            require(frame.getString("sender") in state.enrolledReplicas() && association(node) == frame.getString("sender"))
            checkBudget()
            val receipts = store.importVerified(records, verifiedEntries) ?: store.durableReceipts(ids, checkBudget) ?: exactReceipts(ids)
            validateLocalReceipts(receipts, verifiedEntries)
            requestNewAncestorsLegacy(session)
            pending(frame.getString("sender"), session, "ack", JSONObject().put("receipts", receipts))
        } else advance(frame.getString("sender"), session)
    }

    private fun attemptedAncestors(session: JSONObject): MutableSet<String> {
        val attempted = session.optJSONArray("ancestor_attempts") ?: JSONArray().also { session.put("ancestor_attempts", it) }
        return (0 until attempted.length()).map { attempted.getString(it) }.toMutableSet()
    }

    private fun requestNewAncestorsLegacy(session: JSONObject) {
        if (session.getString("phase") != "records") return
        val page = JSONObject().put("version", 2).put("revision", session.getString("revision")).put("entries", JSONArray()).put("next", JSONObject.NULL)
        val missing = reconcilePage(page).getJSONObject("plan").getJSONArray("missing_ancestors")
        val attempts = attemptedAncestors(session)
        val remaining = session.getJSONArray("remaining")
        val present = ReplicationWire.arrayObjects(remaining).map { ReplicationWire.idKey(it.getJSONObject("id")) }.toSet()
        for (id in ReplicationWire.arrayObjects(missing)) {
            val key = ReplicationWire.idKey(id)
            if (attempts.size >= ReplicationWire.MAX_ITEMS || remaining.length() >= ReplicationWire.MAX_ITEMS) break
            if (key !in present && attempts.add(key)) {
                remaining.put(JSONObject().put("id", id).put("body_key", JSONObject.NULL))
                session.getJSONArray("ancestor_attempts").put(key)
            }
        }
    }

    private fun receiveAck(frame: JSONObject) {
        val payload = frame.getJSONObject("payload")
        require(payload.length() == 1)
        val receipts = payload.getJSONArray("receipts")
        require(receipts.length() in 1..ReplicationWire.MAX_ITEMS)
        for (receipt in ReplicationWire.arrayObjects(receipts)) {
            require(receipt.length() == 4 && receipt.get("version") == 2 && receipt.getString("recipient_replica") == frame.getString("sender"))
            ReplicationWire.idKey(receipt.getJSONObject("operation")); ReplicationWire.bodyKey(receipt.getString("body_key"))
        }
        store.acknowledge(frame.getString("sender"), receipts)
        val ids = JSONArray(ReplicationWire.arrayObjects(receipts).map { it.getJSONObject("operation") })
        // Confirmation also proves our committed copy, so relayed publications acquire exact
        // reciprocal receipts without inferring persistence from a record or SDK submission.
        reply(frame, "ack_confirmed", JSONObject().put("receipts", exactReceipts(ids)))
    }

    private fun matches(frame: JSONObject, kind: String? = null): JSONObject? {
        val session = state.load(frame.getString("sender")) ?: return null
        val pending = session.getJSONObject("pending")
        return session.takeIf { pending.getString("session") == frame.getString("session") && pending.getString("token") == frame.getString("token") &&
            (kind == null || pending.getString("kind") == kind) }
    }

    private fun reply(request: JSONObject, kind: String, payload: JSONObject, assets: Map<String, ByteArray> = emptyMap()) =
        send(ReplicationWire.frame(store.replica, request.getString("sender"), request.getString("session"), request.getString("token"), kind, payload), assets)

    private fun send(frame: JSONObject, assets: Map<String, ByteArray> = emptyMap()) {
        if (frame.getString("target") !in state.enrolledReplicas()) return
        transport.publish(ReplicationWire.path(store.replica, frame.getString("target"), frame.getString("kind")), ReplicationWire.encode(frame), assets)
    }

    private fun outgoing(snapshot: JSONObject): Map<String, JSONObject> {
        val quarantine = snapshot.optJSONArray("quarantine") ?: JSONArray()
        val excluded = ReplicationWire.arrayObjects(quarantine).map { ReplicationWire.idKey(it.getJSONObject("id")) }.toSet()
        return ReplicationWire.arrayObjects(snapshot.getJSONArray("records"))
            .associateBy { ReplicationWire.idKey(it.getJSONObject("operation").getJSONObject("id")) }.filterKeys { it !in excluded }
    }

    private fun exactReceipts(ids: JSONArray): JSONArray =
        store.nativeRead(JSONObject().put("kind", "receipts").put("ids", ids)) as JSONArray?
            ?: domain.receipts(store.snapshot(), store.replica, ids)

    private fun reconcilePage(page: JSONObject): JSONObject {
        val native = store.nativeRead(JSONObject().put("kind", "reconcile").put("page", page)) as JSONObject?
        if (native != null) return native
        val snapshot = store.snapshot()
        val plan = domain.reconcile(snapshot, page)
        val unknown = (ReplicationWire.arrayObjects(plan.getJSONArray("fetch_tombstones")) + ReplicationWire.arrayObjects(plan.getJSONArray("fetch_records")))
            .map { ReplicationWire.idKey(it.getJSONObject("id")) }.toSet()
        val ids = JSONArray(ReplicationWire.arrayObjects(page.getJSONArray("entries"))
            .filter { ReplicationWire.idKey(it.getJSONObject("id")) !in unknown && known(snapshot, it) }.map { it.getJSONObject("id") })
        return JSONObject().put("plan", plan).put("receipts", domain.receipts(snapshot, store.replica, ids))
    }

    private fun known(snapshot: JSONObject, entry: JSONObject): Boolean {
        val id = ReplicationWire.idKey(entry.getJSONObject("id"))
        return ReplicationWire.arrayObjects(snapshot.getJSONArray("records")).any { ReplicationWire.idKey(it.getJSONObject("operation").getJSONObject("id")) == id } ||
            ReplicationWire.arrayObjects(snapshot.getJSONArray("retired")).any { ReplicationWire.idKey(it.getJSONObject("id")) == id }
    }

    private fun descriptor(record: JSONObject): JSONObject = ReplicationWire.descriptor(record)

    private fun sameEntry(a: JSONObject, b: JSONObject) = ReplicationWire.sameEntry(a, b)

    private fun compareId(a: JSONObject, b: JSONObject): Int {
        val replica = a.getString("replica").compareTo(b.getString("replica"))
        return if (replica != 0) replica else a.getString("sequence").compareTo(b.getString("sequence"))
    }
}
