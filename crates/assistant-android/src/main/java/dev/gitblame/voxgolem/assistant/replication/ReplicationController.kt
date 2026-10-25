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
    fun reconcile(): Boolean {
        var accepted = true
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
                    accepted = false
                }

            } else {
                if (!connect(nodeId, peer)) accepted = false
            }
        }
        return accepted
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
