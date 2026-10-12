package dev.gitblame.voxgolem.assistant.inputtransport;

import android.os.Bundle;
import android.speech.RecognitionListener;

/** Bridges SDK default callbacks which older Kotlin compilers do not expose as override points. */
public abstract class AssistantSegmentedListener implements RecognitionListener {
    protected abstract void segment(Bundle results);
    protected abstract void segmentedEnd();

    @Override
    public final void onSegmentResults(Bundle results) { segment(results); }

    @Override
    public final void onEndOfSegmentedSession() { segmentedEnd(); }
}
