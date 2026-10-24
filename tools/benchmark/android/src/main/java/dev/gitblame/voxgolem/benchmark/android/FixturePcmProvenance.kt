package dev.gitblame.voxgolem.benchmark.android

import android.os.Bundle
import java.security.MessageDigest

fun fixturePcmProvenance(pcm: ByteArray): Bundle {
    require(pcm.isNotEmpty() && pcm.size % 2 == 0 && pcm.size <= 336_000)
    val digest = MessageDigest.getInstance("SHA-256").digest(pcm).joinToString("") { "%02x".format(it.toInt() and 255) }
    return Bundle().apply {
        putString("fixture_pcm_sha256", digest)
        putInt("fixture_sample_rate_hz", 16_000); putInt("fixture_channels", 1); putInt("fixture_bits_per_sample", 16)
        putInt("fixture_bytes", pcm.size); putInt("fixture_duration_ms", pcm.size * 1000 / 32_000)
    }
}
