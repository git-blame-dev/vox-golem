package dev.gitblame.voxgolem.android.speechtest;

import android.app.Instrumentation;
import android.app.Application;
import android.content.ComponentName;
import android.content.Intent;
import android.os.Bundle;
import android.os.SystemClock;
import java.lang.reflect.InvocationTargetException;

/** Fixed phone-only SDK control plane. Never accepts programs, prompts or target class names. */
public final class PhoneSyntheticVoiceInstrumentation extends Instrumentation {
    private static final String TARGET = "dev.gitblame.voxgolem.android";
    private static final String FACADE = "dev.gitblame.voxgolem.assistant.phone.PhoneSyntheticVoiceBenchmark";
    private Bundle arguments;
    private Class<?> facade;
    private String phase = "startup";
    private boolean processReserved;

    @Override public void onCreate(Bundle arguments) {
        super.onCreate(arguments);
        this.arguments = arguments == null ? new Bundle() : new Bundle(arguments);
        try {
            String mode = this.arguments.getString("mode", "trial");
            facade = Class.forName(FACADE, true, getTargetContext().getClassLoader());
            facade.getMethod("reserveProcess", Instrumentation.class, String.class).invoke(null, this, mode);
            processReserved = true;
        } catch (Throwable error) {
            Bundle result = new Bundle();
            result.putInt("synthetic_protocol", 2); result.putString("benchmark_result", "failed");
            result.putString("phase", "process_reservation"); result.putString("failure_class", "ProcessReservationFailure");
            result.putBoolean("cleanup_verified", false); result.putInt("completed_trials", 0);
            finish(0, result);
            return;
        }
        start();
    }

    @Override public void callApplicationOnCreate(Application application) {
        // A missing/mixed target facade must not fall through to normal user-state startup.
        if (processReserved && "dev.gitblame.voxgolem.assistant.phone.PhoneBenchmarkApplication".equals(application.getClass().getName())) {
            super.callApplicationOnCreate(application);
        }
    }

    private Bundle call(String name) throws Exception {
        return (Bundle) facade.getMethod(name, Instrumentation.class).invoke(null, this);
    }

    @Override public void onStart() {
        Bundle result = new Bundle();
        result.putInt("synthetic_protocol", 2);
        result.putInt("completed_trials", 0);
        boolean installed = false;
        boolean complete = false;
        boolean notStarted = false;
        boolean diagnosticResult = false;
        Throwable failure = null;
        try {
            facade = Class.forName(FACADE, true, getTargetContext().getClassLoader());
            String mode = arguments.getString("mode", "trial");
            require("trial".equals(mode) || "readiness".equals(mode) || "cleanup".equals(mode) || "cleanup-pending".equals(mode));
            if ("cleanup".equals(mode) || "cleanup-pending".equals(mode)) {
                phase = "cleanup-pending".equals(mode) ? "profile_cleanup_pending" : "profile_cleanup";
                result = (Bundle) facade.getMethod("cleanup", Instrumentation.class, Bundle.class).invoke(null, this, arguments);
                require(result.getInt("synthetic_protocol") == 2 && result.getInt("completed_trials", -1) == 0);
                notStarted = "cleanup-pending".equals(mode) && "not_started".equals(result.getString("benchmark_result"));
                complete = result.getBoolean("cleanup_verified") && result.getBoolean("profile_cleaned") &&
                    result.getBoolean("storage_cleanup_verified") &&
                    ("cleanup".equals(mode) ? "cleaned".equals(result.getString("benchmark_result")) &&
                     result.getBoolean("producer_exit_verified") && result.getBoolean("activity_webview_retired") :
                     "cleaned_after_process_exit".equals(result.getString("benchmark_result")) &&
                     result.getBoolean("producer_exit_verified") && !result.getBoolean("native_cleanup_verified"));
                require(complete || notStarted);
            } else {
                boolean diagnostic = "readiness".equals(mode);
                phase = diagnostic ? "readiness_setup" : "fixture";
                installed = true;
                result.putBoolean("native_resources_retired", false);
                result.putBoolean("native_cleanup_verified", false);
                result.putBoolean("activity_webview_retirement_pending", true);
                Bundle trialArguments = new Bundle(arguments);
                if (!diagnostic) trialArguments.remove("mode");
                Bundle ready = (Bundle) facade.getMethod("install", Instrumentation.class, Bundle.class).invoke(null, this, trialArguments);
                require(ready.getInt("synthetic_protocol") == 2 && (diagnostic ? "readiness_ready" : "fixture_ready").equals(ready.getString("stage")));
                require(arguments.getString("contractSha").equals(ready.getString("contract_sha256")));
                result.putString("cleanup_token", ready.getString("cleanup_token"));
                result.putBoolean("profile_cleanup_pending", true);
                phase = diagnostic ? "readiness" : "trial_1";
                require("armed".equals(call("prepareTrial").getString("stage")));
                getTargetContext().startActivity(new Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER)
                    .setComponent(new ComponentName(TARGET, TARGET + ".MainActivity")).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));
                long deadline = SystemClock.elapsedRealtime() + (diagnostic ? 15_000 : 60_000);
                Bundle status;
                do {
                    status = call("status");
                    if (!"running".equals(status.getString("stage"))) break;
                    SystemClock.sleep(50);
                } while (SystemClock.elapsedRealtime() < deadline);
                sendStatus(0, status);
                result.putAll(status);
                if (diagnostic) {
                    require(status.getInt("voice_admission_budget", -1) == 0 && status.getInt("model_admission_budget", -1) == 0);
                    require(status.getInt("voice_admissions", -1) == 0 && status.getInt("prompt_admissions", -1) == 0);
                    require(status.getInt("generation", -1) == 0 && status.getInt("frontend_submit_observed", -1) == 0 && !status.getBoolean("frontend_ask_clicked"));
                    diagnosticResult = true;
                    complete = "readiness_observed".equals(status.getString("stage"));
                    result.putString("benchmark_result", complete ? "readiness_observed" : "readiness_unverified");
                    result.putInt("completed_trials", 0);
                } else {
                    require("complete".equals(status.getString("stage")));
                    require(status.getInt("finals") == 1 && status.getInt("generation") == 1 && status.getInt("frontend_submit_observed") == 1);
                    require(status.getBoolean("canonical_answer_exact") && status.getBoolean("canonical_ui_equal") && status.getInt("ui_correlated_rows") == 1);
                    result.putString("benchmark_result", "complete");
                    result.putInt("completed_trials", 1);
                    complete = true;
                }
            }
        } catch (Throwable error) {
            failure = error;
        } finally {
            if (installed) {
                try {
                    Bundle cleaned = call("close");
                    require(cleaned.getInt("synthetic_protocol") == 2 && cleaned.getBoolean("native_resources_retired") && cleaned.getBoolean("tts_retired"));
                    require(cleaned.getBoolean("activity_webview_retirement_pending") && !cleaned.getBoolean("native_cleanup_verified"));
                    require(cleaned.getBoolean("profile_cleanup_pending") && !cleaned.getBoolean("cleanup_verified"));
                    result.putAll(cleaned);
                } catch (Throwable error) { if (failure == null) failure = error; }
            }
        }
        result.putInt("synthetic_protocol", 2);
        result.putString("phase", phase);
        if (failure != null || (!complete && !notStarted && !diagnosticResult)) {
            result.putString("benchmark_result", "failed");
            result.putBoolean("cleanup_verified", false);
            result.putInt("completed_trials", 0);
            Throwable category = failure;
            if (category instanceof InvocationTargetException) category = ((InvocationTargetException) category).getTargetException();
            result.putString("failure_class", category == null ? "Incomplete" : category.getClass().getSimpleName());
        }
        finish(failure == null && complete ? -1 : 0, result);
    }

    private static void require(boolean condition) { if (!condition) throw new IllegalStateException(); }
}
