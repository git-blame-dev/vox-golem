package dev.gitblame.voxgolem.assistant.inputtransport

import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.DataInputStream
import java.io.DataOutputStream
import java.nio.ByteBuffer
import java.nio.charset.CodingErrorAction
import java.util.UUID

enum class InputKind { START, READY, PARTIAL, FINAL, STOP, CANCEL, ERROR }

data class InputFrame(val kind: InputKind, val session: String, val source: String, val target: String,
    val language: String, val sequence: Long, val text: String = "", val error: String = "")

object AssistantInputWire {
    const val CAPABILITY = "voxgolem_assistant_input_v1"
    const val PREFIX = "/voxgolem/assistant/input/v1/"
    const val CONTROL_PATH = PREFIX + "control"
    const val AUDIO_PREFIX = PREFIX + "pcm/"
    const val MAX_CONTROL_BYTES = 9000
    const val MAX_TEXT_BYTES = VoiceAudioContract.MAX_TEXT_BYTES
    const val PCM_CHUNK_BYTES = VoiceAudioContract.PCM_CHUNK_BYTES
    const val SAMPLE_RATE = VoiceAudioContract.SAMPLE_RATE
    const val MAX_SESSION_MILLIS = 60000L
    const val IDLE_MILLIS = 5000L
    const val FINAL_MILLIS = 3000L

    fun uuid(value: String): String {
        require(value.length == 36 && UUID.fromString(value).toString() == value)
        return value
    }
    fun language(value: String): String = VoiceAudioContract.language(value)
    fun validate(frame: InputFrame) {
        uuid(frame.session); uuid(frame.source); uuid(frame.target); language(frame.language)
        require(frame.source != frame.target && frame.sequence in 0..100000)
        require(frame.text.toByteArray(Charsets.UTF_8).size <= MAX_TEXT_BYTES)
        require(frame.error.length <= 96 && frame.error.matches(Regex("[a-z0-9_-]*")))
        require(frame.kind in setOf(InputKind.PARTIAL, InputKind.FINAL, InputKind.ERROR) || frame.text.isEmpty())
        require(frame.kind == InputKind.ERROR || frame.error.isEmpty())
    }
    fun encode(frame: InputFrame): ByteArray {
        validate(frame)
        val bytes = ByteArrayOutputStream()
        DataOutputStream(bytes).use { out ->
            out.writeInt(0x56474931); out.writeByte(frame.kind.ordinal)
            for (field in listOf(frame.session, frame.source, frame.target, frame.language)) write(out, field)
            out.writeLong(frame.sequence); write(out, frame.text); write(out, frame.error)
        }
        return bytes.toByteArray().also { require(it.size <= MAX_CONTROL_BYTES) }
    }
    fun decode(bytes: ByteArray): InputFrame {
        require(bytes.size in 1..MAX_CONTROL_BYTES)
        try {
            val input = DataInputStream(ByteArrayInputStream(bytes))
            require(input.readInt() == 0x56474931)
            val kind = InputKind.values().getOrNull(input.readUnsignedByte()) ?: error("Invalid kind")
            val frame = InputFrame(kind, read(input, 36), read(input, 36), read(input, 36), read(input, 64),
                input.readLong(), read(input, MAX_TEXT_BYTES), read(input, 96))
            require(input.available() == 0); validate(frame)
            return frame
        } catch (failure: Exception) { throw IllegalArgumentException("Invalid assistant input frame", failure) }
    }
    fun audioPath(session: String) = AUDIO_PREFIX + uuid(session)
    fun sessionFromPath(path: String): String? = runCatching {
        require(path.startsWith(AUDIO_PREFIX)); uuid(path.removePrefix(AUDIO_PREFIX))
    }.getOrNull()
    private fun write(out: DataOutputStream, text: String) {
        val bytes = text.toByteArray(Charsets.UTF_8); out.writeShort(bytes.size); out.write(bytes)
    }
    private fun read(input: DataInputStream, max: Int): String {
        val size = input.readUnsignedShort(); require(size <= max && size <= input.available())
        val bytes = ByteArray(size); input.readFully(bytes)
        return Charsets.UTF_8.newDecoder().onMalformedInput(CodingErrorAction.REPORT)
            .onUnmappableCharacter(CodingErrorAction.REPORT).decode(ByteBuffer.wrap(bytes)).toString()
    }
}

internal object PcmChannelWire {
    fun write(output: DataOutputStream, pcm: ByteArray) {
        require(pcm.size in 2..AssistantInputWire.PCM_CHUNK_BYTES && pcm.size % 2 == 0)
        output.writeShort(pcm.size); output.write(pcm); output.flush()
    }
    fun end(output: DataOutputStream) { output.writeShort(0); output.flush() }
    fun read(input: DataInputStream): ByteArray? {
        val length = input.readUnsignedShort()
        if (length == 0) return null
        require(length in 2..AssistantInputWire.PCM_CHUNK_BYTES && length % 2 == 0)
        return ByteArray(length).also { input.readFully(it) }
    }
}
