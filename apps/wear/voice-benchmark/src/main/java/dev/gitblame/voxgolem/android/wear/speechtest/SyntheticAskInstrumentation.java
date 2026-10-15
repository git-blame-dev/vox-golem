package dev.gitblame.voxgolem.android.wear.speechtest;

import android.app.Activity;
import android.app.Instrumentation;
import android.content.ComponentName;
import android.content.Intent;
import android.content.pm.ApplicationInfo;
import android.os.Bundle;
import android.os.SystemClock;
import java.lang.reflect.InvocationTargetException;

/** SDK-only control plane; the target owns all real speech, model, UI and persistence work. */
public final class SyntheticAskInstrumentation extends Instrumentation {
    private static final String TARGET = "dev.gitblame.voxgolem.android";
    private static final String FACADE = TARGET + ".wear.assistant.SyntheticAskBenchmark";
    private Bundle arguments;
    private Class<?> facade;
    private String phase = "startup";
    private int completed;

    @Override public void onCreate(Bundle arguments) {
        super.onCreate(arguments);
        this.arguments = arguments == null ? new Bundle() : new Bundle(arguments);
        start();
    }

    @Override public void onStart() {
        boolean installed = false;
        Throwable failure = null;
        boolean startupOnly = "startup".equals(arguments.getString("mode"));
        try {
            require(TARGET.equals(getTargetContext().getPackageName()));
            require((getTargetContext().getApplicationInfo().flags & ApplicationInfo.FLAG_DEBUGGABLE) == 0);
            facade = Class.forName(FACADE, true, getTargetContext().getClassLoader());
            if (!startupOnly) {
                long deadline = SystemClock.elapsedRealtime() + 90_000;
                phase = "fixture";
                Bundle ready = (Bundle) facade.getMethod("install", Instrumentation.class, Bundle.class)
                    .invoke(null, this, arguments);
                installed = true;
                require("fixture_ready".equals(ready.getString("stage")));
                for (int trial = 1; trial <= 3; trial++) {
                    require(SystemClock.elapsedRealtime() < deadline);
                    phase = "trial_" + trial;
                    call("prepareTrial");
                    try {
                        getTargetContext().startActivity(new Intent(Intent.ACTION_MAIN)
                            .addCategory(Intent.CATEGORY_LAUNCHER)
                            .setComponent(new ComponentName(TARGET, TARGET + ".wear.assistant.AssistantVoiceEntry"))
                            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_CLEAR_TASK));
                        long trialDeadline = Math.min(deadline, SystemClock.elapsedRealtime() + 30_000);
                        Bundle status;
                        do {
                            status = call("status");
                            if (!"running".equals(status.getString("stage"))) break;
                            SystemClock.sleep(50);
                        } while (SystemClock.elapsedRealtime() < trialDeadline);
                        status.putInt("synthetic_trial", trial);
                        sendStatus(0, status);
                        require("complete".equals(status.getString("stage")));
                        require(status.getInt("finals") == 1 && status.getInt("generation") == 1);
                        require(status.getInt("GENERATION_DURABLE_COMMITTED") == 1);
                        require(status.getInt("GENERATION_SETTLED") == 1);
                        require(status.getInt("PRESENTATION_TEXT_LAYOUT_OBSERVED") >= 1);
                        require(status.getInt("PRESENTATION_TEXT_DRAW_SUBMITTED") >= 1);
                        require(status.getInt("TTS_UTTERANCE_ON_START") >= 1);
                        require(status.getInt("TTS_UTTERANCE_ON_DONE") >= 1 && status.getBoolean("idle"));
                        completed++;
                    } finally {
                        call("finishTrial");
                    }
                }
            }
        } catch (Throwable error) {
            failure = error;
        } finally {
            try {
                if (installed) call("close");
            } catch (Throwable error) {
                if (failure == null) failure = error;
            } finally {
                if (!startupOnly) {
                    try {
                        getTargetContext().startActivity(new Intent(Intent.ACTION_MAIN)
                            .addCategory(Intent.CATEGORY_HOME).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));
                        waitForIdleSync();
                    } catch (Throwable error) {
                        if (failure == null) failure = error;
                    }
                }
            }
        }
        Bundle result = new Bundle();
        result.putString("benchmark_result", failure == null ? (startupOnly ? "ready" : "complete") : "failed");
        result.putInt("completed_trials", completed);
        result.putString("phase", phase);
        if (failure != null) {
            Throwable cause = failure instanceof InvocationTargetException && failure.getCause() != null
                ? failure.getCause() : failure;
            // Class/category only: exception messages can carry private provider or filesystem data.
            result.putString("failure_class", cause.getClass().getName());
        }
        finish(failure == null ? Activity.RESULT_OK : Activity.RESULT_CANCELED, result);
    }

    private Bundle call(String method) throws Exception {
        return (Bundle) facade.getMethod(method, Instrumentation.class).invoke(null, this);
    }

    private static void require(boolean condition) {
        if (!condition) throw new IllegalStateException("synthetic_benchmark_check_failed");
    }
}
