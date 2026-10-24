package dev.gitblame.voxgolem.android.wear.assistant

import android.app.Instrumentation
import android.content.Context
import dev.gitblame.voxgolem.benchmark.android.LocalFixtureSynthesis

internal object SyntheticAskFixture {
    fun create(instrumentation: Instrumentation, context: Context) =
        LocalFixtureSynthesis.create(instrumentation, context, SyntheticAskContract.PHRASE)
}
