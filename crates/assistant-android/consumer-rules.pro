-keep class dev.gitblame.voxgolem.assistant.AssistantDomainNative { *; }
-keep class dev.gitblame.voxgolem.assistant.runtime.AssistantProviderNative { *; }
-keep interface dev.gitblame.voxgolem.assistant.runtime.ProviderDeltaCallback { *; }
-keepclassmembers class * implements dev.gitblame.voxgolem.assistant.runtime.ProviderDeltaCallback {
    public void onReady();
    public void onRequestDispatched();
    public void onDelta(java.lang.String);
    public void onStatus(java.lang.String);
    public void onFirstText(long);
}
