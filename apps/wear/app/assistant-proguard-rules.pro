-keep class dev.gitblame.voxgolem.android.wear.assistant.WearAssistantNative { *; }
-keep class dev.gitblame.voxgolem.android.wear.assistant.TypedAskBenchmark {
    public static android.os.Bundle install(android.app.Instrumentation, android.os.Bundle);
    public static android.os.Bundle setupCase(android.app.Instrumentation, android.os.Bundle);
    public static android.os.Bundle prepareDraft(android.app.Instrumentation, android.os.Bundle);
    public static android.os.Bundle arm(android.app.Instrumentation, android.os.Bundle);
    public static android.os.Bundle status(android.app.Instrumentation);
    public static android.os.Bundle validate(android.app.Instrumentation);
    public static android.os.Bundle close(android.app.Instrumentation);
}
-keep class dev.gitblame.voxgolem.android.wear.assistant.SyntheticAskBenchmark {
    public static android.os.Bundle install(android.app.Instrumentation, android.os.Bundle);
    public static android.os.Bundle prepareTrial(android.app.Instrumentation);
    public static android.os.Bundle status(android.app.Instrumentation);
    public static android.os.Bundle finishTrial(android.app.Instrumentation);
    public static android.os.Bundle close(android.app.Instrumentation);
}
