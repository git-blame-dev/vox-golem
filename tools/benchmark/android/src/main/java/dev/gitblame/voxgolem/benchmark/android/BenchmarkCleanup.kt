package dev.gitblame.voxgolem.benchmark.android

/** Attempt every owned teardown, but never promote a partial retirement into success. */
fun runBenchmarkCleanup(stages: List<() -> Unit>) {
    var failure: Exception? = null
    stages.forEach { stage ->
        try { stage() } catch (error: Exception) { if (failure == null) failure = error }
    }
    failure?.let { throw it }
}
