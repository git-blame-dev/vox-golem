-keepclassmembers class * implements android.speech.RecognitionListener {
    public void onResults(android.os.Bundle);
    public void onPartialResults(android.os.Bundle);
    public void onError(int);
    public void onSegmentResults(android.os.Bundle);
    public void onEndOfSegmentedSession();
}
-keepclassmembers class * implements android.speech.RecognitionSupportCallback {
    public void onSupportResult(android.speech.RecognitionSupport);
    public void onError(int);
}
-keepclassmembers class * extends android.speech.tts.UtteranceProgressListener {
    public void onStart(java.lang.String);
    public void onDone(java.lang.String);
    public void onError(java.lang.String);
    public void onError(java.lang.String, int);
    public void onStop(java.lang.String, boolean);
}
