package dev.gitblame.voxgolem.assistant.replication

import android.util.AtomicFile
import org.json.JSONObject
import java.io.File

/** Not a history protocol. Only this exact access-only shape may cross the paired channel. */
internal object ChatGptAccessProtocol {
    const val PREFIX = "/voxgolem/chatgpt-access/v1/"
    const val MAX_BYTES = 64 * 1024
    const val META = "_chatgptPeer"
    fun keys(value: JSONObject) = value.keys().asSequence().toSet()
    fun access(value: JSONObject, now: Long): JSONObject {
        require(keys(value) == setOf("openai") || keys(value) == setOf("openai", "voxgolemChatgpt"))
        if (value.has("voxgolemChatgpt")) require(value.get("voxgolemChatgpt") == 1)
        val token = value.getJSONObject("openai")
        require(keys(token).all { it in setOf("type", "access", "expires", "accountId") })
        if (token.has("type")) require(token.get("type") == "oauth")
        val bearer = token.get("access")
        require(bearer is String && bearer.isNotEmpty() && bearer.length <= 24_000 && bearer.all { it.code in 33..126 })
        val expires = token.get("expires")
        require(expires is Number && expires.toDouble().isFinite() && expires.toDouble() == expires.toLong().toDouble())
        require(expires.toLong() > now && expires.toLong() <= 9_007_199_254_740_991L)
        // Same maximum access lifetime admitted by the shared Rust token exchange.
        require(expires.toLong() - now <= 30L * 24 * 60 * 60 * 1000)
        if (token.has("accountId")) {
            val account = token.get("accountId")
            require(account is String && account.isNotEmpty() && account.length <= 256 && account.all { it.code in 33..126 })
        }
        require(value.toString().toByteArray(Charsets.UTF_8).size <= MAX_BYTES - 2048)
        return JSONObject(value.toString()).put("voxgolemChatgpt", 1)
    }

    fun packet(raw: ByteArray, association: String, nonce: String, previous: Long, now: Long): JSONObject {
        require(raw.size <= MAX_BYTES)
        val value = JSONObject(raw.toString(Charsets.UTF_8))
        require(keys(value) == setOf("version", "association", "nonce", "generation", "credential"))
        require(value.getInt("version") == 1 && value.getString("association") == association && value.getString("nonce") == nonce)
        val generation = value.get("generation")
        require(generation is Long || generation is Int)
        require(value.getLong("generation") > previous)
        val credential = if (value.isNull("credential")) tombstone() else access(value.getJSONObject("credential"), now)
        return credential.put(META, JSONObject().put("association", association).put("nonce", nonce).put("generation", value.getLong("generation")))
    }

    fun tombstone() = JSONObject().put("voxgolemChatgpt", 1).put("signedOut", true)

    fun receiptFor(packet: JSONObject): JSONObject = JSONObject().put("association", packet.getString("association"))
        .put("nonce", packet.getString("nonce")).put("generation", packet.getLong("generation"))

    /** An ACK confirms this exact atomic publication; it contains no credential material. */
    fun receipt(bytes: ByteArray): JSONObject {
        require(bytes.size <= 512)
        val value = JSONObject(bytes.toString(Charsets.UTF_8))
        require(keys(value) == setOf("association", "nonce", "generation"))
        val generation = value.get("generation")
        require((generation is Int || generation is Long) && value.getLong("generation") > 0)
        return receiptFor(value)
    }

    fun sameReceipt(a: JSONObject, b: JSONObject) = a.getString("association") == b.getString("association") &&
        a.getString("nonce") == b.getString("nonce") && a.getLong("generation") == b.getLong("generation")

    fun read(file: File): JSONObject? {
        if (!file.exists() && !File(file.path + ".bak").exists()) return null
        require(file.length() <= MAX_BYTES && File(file.path + ".bak").length() <= MAX_BYTES)
        return JSONObject(AtomicFile(file).readFully().also { require(it.size <= MAX_BYTES) }.toString(Charsets.UTF_8))
    }

    fun publish(file: File, value: JSONObject, approved: () -> Boolean = { true }) {
        val bytes = value.toString().toByteArray(Charsets.UTF_8)
        require(bytes.size <= MAX_BYTES)
        val atomic = AtomicFile(file)
        val output = atomic.startWrite()
        try {
            android.system.Os.fchmod(output.fd, 384) // 0600, including AtomicFile's staging file.
            output.write(bytes)
            check(approved()) { "association_changed" }
            atomic.finishWrite(output)
            // AtomicFile can report some rename/write failures only through platform logs.
            // A paired ACK must describe the actual published file, not just finishWrite().
            check(file.length() == bytes.size.toLong() && file.readBytes().contentEquals(bytes)) { "publication_unconfirmed" }
        } catch (error: Exception) { atomic.failWrite(output); throw error }
    }
}
