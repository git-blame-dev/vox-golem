package dev.gitblame.voxgolem.assistant.runtime

import android.content.Context
import android.os.Build
import java.io.File

data class OptionalProviderReadiness(val available: Boolean, val reason: String)

/** Cheap installed-route evidence only. Does not warm a provider or claim account/network health. */
internal object AndroidProviderReadiness {
    fun inspect(context: Context): Map<String, OptionalProviderReadiness> {
        val executable = listOf(File(context.applicationInfo.nativeLibraryDir, "libagy.so"), File(context.filesDir, "libagy.so"))
            .any { it.isFile && it.canExecute() }
        val credentials = AgyPrivateEnvironment.isProvisioned(context)
        val supported = Build.VERSION.SDK_INT >= 26
        val watch = isWatch(context)
        val agy = if (watch) OptionalProviderReadiness(true, "Runs on the paired phone; its Agy readiness is checked on request") else OptionalProviderReadiness(supported && executable && credentials,
            if (!supported) "Agy requires Android 26+ for bounded process retirement" else if (!executable) "Agy executable is not installed" else if (!credentials) "Agy requires externally provisioned app-private access and installation identity" else "Installed Agy route; account access is checked on request")
        val nano = if (watch) OptionalProviderReadiness(true, "Runs on the paired phone; its Gemini Nano readiness is checked on request") else OptionalProviderReadiness(Build.VERSION.SDK_INT >= 34,
            if (Build.VERSION.SDK_INT >= 34) "On-device Gemini Nano via AICore; availability is checked on request" else "Gemini Nano requires Android 14+")
        return mapOf("local-fast" to OptionalProviderReadiness(false, "No owned local server readiness is published on Android"),
            GeminiNanoProvider.MODEL_NAME to nano,
            "agy-flash-none" to agy, "agy-flash-low" to agy, "agy-flash-high" to agy, "agy-opus-high" to agy, "agy-sonnet-low" to agy, "agy-sonnet-high" to agy)
    }
}
