package dev.gitblame.voxgolem.capture;

import android.os.Bundle;
import android.speech.RecognitionListener;

/** Exposes recent default RecognitionListener callbacks as ordinary Kotlin override points. */
abstract class SegmentedRecognitionCallbacks implements RecognitionListener {
    protected abstract void onSegmentResultsReceived(Bundle results);

    protected abstract void onSegmentedSessionEnded();

    @Override
    public final void onSegmentResults(Bundle results) {
        onSegmentResultsReceived(results);
    }

    @Override
    public final void onEndOfSegmentedSession() {
        onSegmentedSessionEnded();
    }
}
