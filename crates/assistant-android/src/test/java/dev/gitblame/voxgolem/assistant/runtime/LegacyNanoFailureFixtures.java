package dev.gitblame.voxgolem.assistant.runtime;

import com.google.ai.edge.aicore.GenerativeAIException;
import com.google.ai.edge.aicore.InferenceException;

final class LegacyNanoFailureFixtures {
    private LegacyNanoFailureFixtures() {}

    // The SDK exception constructor is JVM-public but Kotlin-internal.
    static GenerativeAIException inference(int errorCode) {
        return new InferenceException("synthetic SDK failure", null, errorCode);
    }
}
