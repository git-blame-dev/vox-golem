package dev.gitblame.voxgolem.assistant.replication

import org.json.JSONArray
import org.json.JSONObject
import java.util.UUID

/** Finite, request-driven DataItem slots. Durable session metadata contains no record bodies. */
internal class ReplicationController(
    private val store: ReplicationStore,
    private val domain: ReplicationDomain,
    private val state: ReplicationState,
    private val transport: ReplicationTransport,
    private val association: (String) -> String?,
    private val newToken: () -> String = { UUID.randomUUID().toString() },
    private val checkBudget: () -> Unit = {},
    private val onRejected: (Exception) -> Unit = {},
) {
    fun reconcile() {
        val peers = linkedSetOf<String>()
        val enrolled = state.enrolledReplicas()
        for (node in transport.reachable().take(64)) {
            checkBudget()
            val peer = association(node.nodeId)
            checkBudget()
            if (peer != null && peer != store.replica && peer in enrolled) peers.add(peer)
            if (peers.size == ReplicationWire.MAX_PEERS) break
        }
        for (peer in peers) {
            checkBudget()
            val saved = state.load(peer)
            if (saved == null) begin(peer) else send(saved.getJSONObject("pending"))
            val snapshot = store.snapshot()
            val retained = outgoing(snapshot)
            val offers = ReplicationWire.arrayObjects(store.publications(peer, null, 32))
                .filter { ReplicationWire.idKey(it.getJSONObject("operation").getJSONObject("id")) in retained }
                .map(::descriptor).sortedWith { a, b -> compareId(a.getJSONObject("id"), b.getJSONObject("id")) }
            send(ReplicationWire.frame(store.replica, peer, newToken(), newToken(), "hint", JSONObject()
                .put("revision", snapshot.getString("revision")).put("offers", JSONArray(offers))))
        }
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
                        domain.reconcile(store.snapshot(), JSONObject().put("version", 1).put("revision", payload.getString("revision"))
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
        require(page.length() == 4 && page.get("version") == 1)
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
        val snapshot = store.snapshot()
        val plan = domain.reconcile(snapshot, page)
        val unknown = (ReplicationWire.arrayObjects(plan.getJSONArray("fetch_tombstones")) + ReplicationWire.arrayObjects(plan.getJSONArray("fetch_records")))
            .map { ReplicationWire.idKey(it.getJSONObject("id")) }.toSet()
        val knownIds = JSONArray(ReplicationWire.arrayObjects(entries).filter { ReplicationWire.idKey(it.getJSONObject("id")) !in unknown }
            .filter { known(snapshot, it) }.map { it.getJSONObject("id") })
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
        if (knownIds.length() > 0) pending(peer, session, "ack", JSONObject().put("receipts", domain.receipts(snapshot, store.replica, knownIds)))
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
            records.put(record); ids.put(entry.getJSONObject("id"))
        }
        for (id in ReplicationWire.arrayObjects(unavailable)) require(answered.add(ReplicationWire.idKey(id)) && ReplicationWire.idKey(id) in expected)
        require(answered == expected.keys)
        if (records.length() > 0) {
            require(frame.getString("sender") in state.enrolledReplicas() && association(node) == frame.getString("sender"))
            checkBudget()
            store.importRecords(records) // Rust recomputes hashes; SQLite commits before ACK.
            val expectedReceipts = domain.receipts(store.snapshot(), store.replica, ids)
            val receipts = store.durableReceipts(ids, checkBudget) ?: expectedReceipts
            val expectedKeys = ReplicationWire.arrayObjects(expectedReceipts).associateBy { ReplicationWire.idKey(it.getJSONObject("operation")) }
            require(receipts.length() == expectedKeys.size)
            for (receipt in ReplicationWire.arrayObjects(receipts)) {
                val expectedReceipt = expectedKeys.getValue(ReplicationWire.idKey(receipt.getJSONObject("operation")))
                require(receipt.length() == 4 && receipt.get("version") == expectedReceipt.get("version") &&
                    receipt.getString("recipient_replica") == expectedReceipt.getString("recipient_replica") && receipt.getString("body_key") == expectedReceipt.getString("body_key"))
            }
            requestNewAncestors(session)
            pending(frame.getString("sender"), session, "ack", JSONObject().put("receipts", receipts))
        } else advance(frame.getString("sender"), session)
    }

    private fun attemptedAncestors(session: JSONObject): MutableSet<String> {
        val attempted = session.optJSONArray("ancestor_attempts") ?: JSONArray().also { session.put("ancestor_attempts", it) }
        return (0 until attempted.length()).map { attempted.getString(it) }.toMutableSet()
    }

    private fun requestNewAncestors(session: JSONObject) {
        if (session.getString("phase") != "records") return
        val page = JSONObject().put("version", 1).put("revision", session.getString("revision")).put("entries", JSONArray()).put("next", JSONObject.NULL)
        val missing = domain.reconcile(store.snapshot(), page).getJSONArray("missing_ancestors")
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
            require(receipt.length() == 4 && receipt.get("version") == 1 && receipt.getString("recipient_replica") == frame.getString("sender"))
            ReplicationWire.idKey(receipt.getJSONObject("operation")); ReplicationWire.bodyKey(receipt.getString("body_key"))
        }
        store.acknowledge(frame.getString("sender"), receipts)
        val ids = JSONArray(ReplicationWire.arrayObjects(receipts).map { it.getJSONObject("operation") })
        // Confirmation also proves our committed copy, so relayed publications acquire exact
        // reciprocal receipts without inferring persistence from a record or SDK submission.
        reply(frame, "ack_confirmed", JSONObject().put("receipts", domain.receipts(store.snapshot(), store.replica, ids)))
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

    private fun known(snapshot: JSONObject, entry: JSONObject): Boolean {
        val id = ReplicationWire.idKey(entry.getJSONObject("id"))
        return ReplicationWire.arrayObjects(snapshot.getJSONArray("records")).any { ReplicationWire.idKey(it.getJSONObject("operation").getJSONObject("id")) == id } ||
            ReplicationWire.arrayObjects(snapshot.getJSONArray("retired")).any { ReplicationWire.idKey(it.getJSONObject("id")) == id }
    }

    private fun descriptor(record: JSONObject): JSONObject {
        val operation = record.getJSONObject("operation")
        val body = operation.getJSONObject("body")
        val kind = body.getString("kind")
        return JSONObject().put("id", operation.getJSONObject("id")).put("body_key", ReplicationWire.bodyKey(record.getString("body_key")))
            .put("conversation", if (kind == "conversation_created") operation.getJSONObject("id") else body.getJSONObject("conversation"))
            .put("tombstone", kind == "conversation_deleted")
    }

    private fun sameEntry(a: JSONObject, b: JSONObject) = ReplicationWire.idKey(a.getJSONObject("id")) == ReplicationWire.idKey(b.getJSONObject("id")) &&
        ReplicationWire.idKey(a.getJSONObject("conversation")) == ReplicationWire.idKey(b.getJSONObject("conversation")) &&
        a.getString("body_key") == b.getString("body_key") && a.getBoolean("tombstone") == b.getBoolean("tombstone")

    private fun compareId(a: JSONObject, b: JSONObject): Int {
        val replica = a.getString("replica").compareTo(b.getString("replica"))
        return if (replica != 0) replica else a.getString("sequence").compareTo(b.getString("sequence"))
    }
}
