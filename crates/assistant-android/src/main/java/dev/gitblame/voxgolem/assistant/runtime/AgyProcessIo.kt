package dev.gitblame.voxgolem.assistant.runtime

import android.os.Build
import java.io.InputStream
import java.io.ByteArrayOutputStream
import java.nio.charset.StandardCharsets
import java.nio.ByteBuffer
import java.nio.charset.CodingErrorAction
import java.util.concurrent.TimeUnit

/** No line-oriented reader: an engine can omit a newline forever or emit one enormous line. */
internal class AgyLines(private val input: InputStream, private val maxLineBytes: Int = 1024 * 1024,
    private val maxTotalBytes: Long = 8L * 1024 * 1024) {
    private val buffer = ByteArray(4096)
    private var offset = 0
    private var available = 0
    private var total = 0L
    fun next(): String? {
        val line = ByteArrayOutputStream()
        while (true) {
            if (offset == available) {
                available = input.read(buffer)
                offset = 0
                if (available < 0) return if (line.size() == 0) null else decode(line)
                total += available
                check(total <= maxTotalBytes) { "Agy output budget exceeded" }
            }
            if (available == 0) continue
            val byte = buffer[offset++]
            if (byte == 10.toByte()) return decode(line)
            check(line.size() < maxLineBytes) { "Agy line budget exceeded" }
            line.write(byte.toInt())
        }
    }
    private fun decode(line: ByteArrayOutputStream) = StandardCharsets.UTF_8.newDecoder()
        .onMalformedInput(CodingErrorAction.REPORT).onUnmappableCharacter(CodingErrorAction.REPORT)
        .decode(ByteBuffer.wrap(line.toByteArray())).toString()
}

/** Retain the Process until exit is confirmed; destroy() alone is only a request. */
internal fun retireAgyProcess(process: Process, budgetMs: Long = 1000): Boolean {
    if (!agyAlive(process)) return true
    process.destroy()
    // Bounded wait and forced retirement are supported Process APIs only from Android 26.
    if (Build.VERSION.SDK_INT < 26) return !agyAlive(process)
    if (process.waitFor(budgetMs, TimeUnit.MILLISECONDS)) return true
    process.destroyForcibly()
    return process.waitFor(budgetMs, TimeUnit.MILLISECONDS)
}

internal fun agyAlive(process: Process?): Boolean = process != null && try {
    process.exitValue(); false
} catch (_: IllegalThreadStateException) { true }

internal fun writeAgyInput(process: Process, serialized: String) {
    process.outputStream.write(serialized.toByteArray(StandardCharsets.UTF_8))
    process.outputStream.flush()
}
