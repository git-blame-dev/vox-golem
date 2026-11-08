package dev.gitblame.voxgolem.android.wear.speechtest;

import android.accessibilityservice.AccessibilityServiceInfo;
import android.app.Activity;
import android.app.Instrumentation;
import android.app.KeyguardManager;
import android.content.Context;
import android.content.ComponentName;
import android.content.Intent;
import android.os.Bundle;
import android.os.SystemClock;
import android.os.PowerManager;
import android.graphics.Rect;
import android.view.MotionEvent;
import android.view.InputDevice;
import android.view.accessibility.AccessibilityNodeInfo;
import org.json.JSONObject;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;
import java.util.UUID;

/** SDK-only real-UI driver. Host owns scenarios, validation policy and the live budget. */
public final class TypedAskInstrumentation extends Instrumentation {
    private static final String TARGET = "dev.gitblame.voxgolem.android";
    private static final int MAX_COMMAND = 65536;
    private static final int MAX_RESPONSE = 1048576;
    private Bundle arguments;
    private Class<?> facade;
    private long expires;
    private File directory;
    private boolean installed;
    private boolean cleaned;
    private boolean blocked;
    private boolean configured;
    private int maxTurns;
    private int turnTimeoutSeconds;
    private int submitted;
    private long responseBytes;
    private String phase = "install";
    private JSONObject readinessDiagnostics;
    private boolean latestRequested;
    private boolean latestAcked;

    @Override public void onCreate(Bundle input) {
        super.onCreate(input);
        arguments = input == null ? new Bundle() : new Bundle(input);
        start();
    }

    @Override public void onStart() {
        Throwable failure = null;
        String failurePhase = null;
        try {
            facade = Class.forName(TARGET + ".wear.assistant.TypedAskBenchmark", true, getTargetContext().getClassLoader());
            Bundle ready = invoke("install", arguments);
            installed = true;
            String run = ready.getString("run");
            require(UUID.fromString(run).toString().equals(run));
            maxTurns = ready.getInt("maxTurns");
            turnTimeoutSeconds = ready.getInt("turnTimeoutSeconds");
            expires = SystemClock.elapsedRealtime() + ready.getInt("timeout") * 1000L;
            File external = getTargetContext().getExternalFilesDir(null);
            require(external != null);
            File root = new File(external, "diagnostics/typed-benchmark");
            require(root.mkdirs() || root.isDirectory());
            require(root.getCanonicalFile().equals(new File(external.getCanonicalFile(), "diagnostics/typed-benchmark")));
            pruneExpired(root);
            File candidate = new File(root, run);
            require(candidate.mkdir()); // Never reuse or clean another owner's mailbox.
            directory = candidate;
            AccessibilityServiceInfo info = getUiAutomation().getServiceInfo();
            info.flags |= AccessibilityServiceInfo.FLAG_REPORT_VIEW_IDS;
            getUiAutomation().setServiceInfo(info);
            write(new File(directory, "ready.json"), new JSONObject().put("status", "ready").put("protocol", 1)
                .put("max_turns", maxTurns).put("model", "custom-luna-none").put("timeout_seconds", ready.getInt("timeout"))
                .put("turn_timeout_seconds", turnTimeoutSeconds));
            Bundle status = new Bundle(); status.putString("typed_benchmark", "ready"); sendStatus(0, status);
            for (int sequence = 1; sequence <= 128; sequence++) {
                File command = new File(directory, "command-" + sequence + ".json");
                while (!command.exists() && SystemClock.elapsedRealtime() < expires) SystemClock.sleep(50);
                require(SystemClock.elapsedRealtime() < expires);
                JSONObject request = read(command);
                require(request.getInt("sequence") == sequence);
                String operation = request.getString("command");
                JSONObject response;
                boolean closing = "close".equals(operation);
                readinessDiagnostics = null;
                try {
                    response = dispatch(operation, request);
                } catch (Exception error) {
                    blocked = true;
                    try { if (configured && !"await_ready".equals(phase)) captureReadiness(); }
                    catch (Exception ignored) { /* Preserve the last bounded readiness observation. */ }
                    response = new JSONObject().put("status", "failed").put("failure_class", error.getClass().getName())
                        .put("execution_valid", false).put("phase", phase);
                    if (readinessDiagnostics != null) response.put("readiness_diagnostics", readinessDiagnostics);
                }
                response.put("sequence", sequence);
                write(new File(directory, "response-" + sequence + ".json"), response);
                require(command.delete());
                if (closing) break;
            }
        } catch (Throwable error) {
            failure = error;
            failurePhase = phase;
        } finally {
            try { if (installed && !cleaned) { phase = "close"; invoke("close", null); } }
            catch (Throwable error) { if (failure == null) { failure = error; failurePhase = phase; } }
            // Allow retrieval of the last response. Host removes ready.json only after retrieval.
            if (directory != null) {
                long deadline = SystemClock.elapsedRealtime() + 30000;
                while (new File(directory, "ready.json").exists() && SystemClock.elapsedRealtime() < deadline) SystemClock.sleep(50);
                try { deleteOwned(directory); }
                catch (Exception error) { if (failure == null) { failure = error; failurePhase = "close"; } }
            }
        }
        Bundle result = new Bundle();
        result.putString("typed_benchmark", failure == null && cleaned ? "cleaned" : "failed");
        result.putString("phase", failurePhase == null ? phase : failurePhase);
        if (failure != null) result.putString("failure_class", failure.getClass().getName());
        finish(failure == null && cleaned ? Activity.RESULT_OK : Activity.RESULT_CANCELED, result);
    }

    private JSONObject dispatch(String operation, JSONObject request) throws Exception {
        switch (operation) {
            case "setup_case": {
                phase = "setup_case";
                keys(request, "sequence", "command", "web_enabled");
                require(!blocked && request.get("web_enabled") instanceof Boolean);
                Bundle config = new Bundle(); config.putBoolean("web_enabled", request.getBoolean("web_enabled"));
                phase = "native_setup";
                Bundle result = invoke("setupCase", config);
                phase = "launch_activity";
                getTargetContext().startActivity(new Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER)
                    .setComponent(new ComponentName(TARGET, TARGET + ".wear.assistant.AssistantActivity"))
                    .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));
                awaitReady(); configured = true;
                return json(result);
            }
            case "await_ready": {
                phase = "await_ready";
                keys(request, "sequence", "command");
                require(!blocked && configured);
                awaitReady();
                return new JSONObject().put("status", "ready").put("readiness_complete", true);
            }
            case "probe": {
                phase = "prepare_draft";
                keys(request, "sequence", "command", "prompt");
                require(!blocked && configured && submitted == 0);
                String prompt = boundedPrompt(request);
                prepareDraft(prompt);
                boolean latestRestored = probeViewport(prompt);
                Bundle state = invoke("status", null);
                require(!state.getBoolean("busy") && state.getBoolean("context_ready") && !state.getBoolean("failed"));
                return new JSONObject().put("status", "ready").put("ui_probe", true).put("send_ready", true)
                    .put("latest_restored", latestRestored);
            }
            case "submit": {
                phase = "native_setup";
                keys(request, "sequence", "command", "prompt");
                require(!blocked && configured && submitted < maxTurns);
                String prompt = boundedPrompt(request);
                prepareDraft(prompt);
                Bundle input = new Bundle(); input.putString("prompt", prompt);
                phase = "native_setup";
                Bundle armed = invoke("arm", input);
                submitted++; // No retries after any uncertainty at the UI boundary.
                phase = "send_click";
                long origin = SystemClock.elapsedRealtimeNanos();
                require(clickLabel("Send draft"));
                Long first = null;
                Bundle state;
                boolean terminal = false;
                boolean rateLimited = false;
                long deadline = Math.min(expires, SystemClock.elapsedRealtime() + turnTimeoutSeconds * 1000L);
                phase = "turn_observation";
                do {
                    state = invoke("status", null);
                    boolean textSeen = nonblank(state.getString("provisional")) ||
                        (prompt.equals(state.getString("projected_prompt")) && nonblank(state.getString("projected_answer")));
                    if (first == null && textSeen) first = (SystemClock.elapsedRealtimeNanos() - origin) / 1000000;
                    String tool = state.getString("tool_status");
                    if (tool != null && tool.startsWith("Rate limited")) { rateLimited = true; blocked = true; break; }
                    if (state.getBoolean("failed")) { blocked = true; break; }
                    if (!state.getBoolean("busy") && prompt.equals(state.getString("projected_prompt")) &&
                        nonblank(state.getString("projected_answer"))) { terminal = true; break; }
                    SystemClock.sleep(50);
                } while (SystemClock.elapsedRealtime() < deadline);
                long completed = (SystemClock.elapsedRealtimeNanos() - origin) / 1000000;
                if (!terminal) blocked = true;
                if (terminal) phase = "canonical_validation";
                JSONObject result = terminal ? json(invoke("validate", null)) : json(state);
                boolean valid = terminal && result.optBoolean("execution_valid");
                if (!valid) {
                    blocked = true;
                    if ("none".equals(result.optString("failure_reason")))
                        result.put("failure_reason", terminal ? "canonical_validation_failed" : "turn_incomplete");
                }
                result.put("status", blocked ? "blocked" : "complete").put("submitted", true)
                    .put("native_rate_limit_observed", rateLimited)
                    .put("request", armed.getString("request")).put("execution_valid", valid)
                    .put("first_answer_ms", first == null ? JSONObject.NULL : first)
                    .put("completed_ms", terminal ? completed : JSONObject.NULL)
                    .put("timing_scope", "device_monotonic_accessibility_click_to_first_presented_projection_and_terminal_including_polling_not_paint")
                    .put("ui_visible_equal", JSONObject.NULL).put("ui_visible_status", "unverified")
                    .put("sources_status", "unsupported").put("page_read_status", "unsupported");
                // Independently compare the full answer only if one actual accessibility text node contains it.
                String answer = result.optString("canonical_answer", "");
                if (!answer.isEmpty() && hasExactText(answer)) result.put("ui_visible_equal", true).put("ui_visible_status", "pass");
                return result;
            }
            case "snapshot":
            case "status": {
                phase = "await_ready";
                keys(request, "sequence", "command");
                require(configured);
                return json(invoke("status", null)).put("status", blocked ? "blocked" : "ready");
            }
            case "close": {
                phase = "close";
                keys(request, "sequence", "command");
                Bundle result = invoke("close", null); cleaned = true;
                return json(result);
            }
            default: throw new IllegalArgumentException("unknown_command");
        }
    }

    private void awaitReady() throws Exception {
        phase = "await_ready";
        Bundle state = invoke("awaitReady", null);
        readinessDiagnostics = json(state);
        require(state.getBoolean("readiness_complete"));
    }

    private static String boundedPrompt(JSONObject request) throws Exception {
        String prompt = request.getString("prompt");
        require(!prompt.trim().isEmpty() && prompt.getBytes(StandardCharsets.UTF_8).length <= 32768);
        return prompt;
    }

    private void prepareDraft(String prompt) throws Exception {
        awaitReady();
        phase = "prepare_draft";
        Bundle input = new Bundle(); input.putString("prompt", prompt);
        Bundle prepared = invoke("prepareDraft", input);
        require(prepared.getBoolean("draft_prepared"));
        phase = "draft_ready";
        long deadline = Math.min(expires, SystemClock.elapsedRealtime() + 10000);
        latestRequested = false;
        latestAcked = false;
        boolean touchRequested = false;
        do {
            Bundle state = invoke("status", null);
            require(!state.getBoolean("busy") && state.getBoolean("context_ready") && !state.getBoolean("failed"));
            if (prompt.equals(state.getString("presented_draft"))) {
                phase = "send_ready";
                // Let draft-driven list/dock accessibility updates settle before
                // selecting the real control. This is fixture setup, not timing.
                long remaining = deadline - SystemClock.elapsedRealtime();
                if (remaining > 0) {
                    try { getUiAutomation().waitForIdle(Math.min(100, remaining), Math.min(1000, remaining)); }
                    catch (java.util.concurrent.TimeoutException ignored) { /* Recheck within the same deadline. */ }
                }
                if (hasLabel("Send draft")) return;
                // A history follow-up can grow beyond the prior viewport end.
                // Use the same one-shot UI action as the reader, before timing.
                if (!latestRequested && hasLabel("Jump to latest")) {
                    latestRequested = true;
                    latestAcked = clickLabel("Jump to latest");
                } else if (latestRequested && !touchRequested && hasLabel("Jump to latest")) {
                    // An accessibility acknowledgement is not a completed jump.
                    // After settlement, try the actual, freshly bounded control once.
                    touchRequested = true;
                    touchLabel("Jump to latest");
                }
            }
            SystemClock.sleep(50);
        } while (SystemClock.elapsedRealtime() < deadline);
        captureReadiness();
        throw new IllegalStateException("draft_not_presented_or_send_not_ready");
    }

    private boolean probeViewport(String prompt) throws Exception {
        phase = "send_ready";
        long deadline = Math.min(expires, SystemClock.elapsedRealtime() + 10000);
        latestRequested = false; latestAcked = false;
        boolean latestObserved = false;
        for (int attempt = 0; attempt < 8 && SystemClock.elapsedRealtime() < deadline; attempt++) {
            if (hasLabel("Jump to latest")) { latestObserved = true; break; }
            int action = attempt < 4 ? AccessibilityNodeInfo.ACTION_SCROLL_BACKWARD : AccessibilityNodeInfo.ACTION_SCROLL_FORWARD;
            for (AccessibilityNodeInfo node : nodes())
                if (node.isVisibleToUser() && node.isScrollable() && node.performAction(action)) break;
            settle(deadline);
        }
        latestObserved |= hasLabel("Jump to latest");
        if (!latestObserved) { captureReadiness(); throw new IllegalStateException("viewport_probe_latest_not_observed"); }
        latestRequested = true;
        latestAcked = clickLabel("Jump to latest");
        boolean touchRequested = false;
        do {
            settle(deadline);
            Bundle state = invoke("status", null);
            require(!state.getBoolean("busy") && !state.getBoolean("failed") && state.getBoolean("context_ready"));
            if (prompt.equals(state.getString("presented_draft")) && hasLabel("Send draft")) return true;
            if (!touchRequested && hasLabel("Jump to latest")) {
                touchRequested = true;
                touchLabel("Jump to latest");
            }
            SystemClock.sleep(50);
        } while (SystemClock.elapsedRealtime() < deadline);
        captureReadiness();
        throw new IllegalStateException("viewport_probe_latest_not_restored");
    }

    private void settle(long deadline) {
        long remaining = deadline - SystemClock.elapsedRealtime();
        if (remaining <= 0) return;
        try { getUiAutomation().waitForIdle(Math.min(100, remaining), Math.min(1000, remaining)); }
        catch (java.util.concurrent.TimeoutException expected) { /* Reobserve within the same deadline. */ }
    }

    private boolean screenInteractive() {
        PowerManager power = (PowerManager) getTargetContext().getSystemService(Context.POWER_SERVICE);
        return power != null && power.isInteractive();
    }
    private boolean keyguardLocked() {
        KeyguardManager keyguard = (KeyguardManager) getTargetContext().getSystemService(Context.KEYGUARD_SERVICE);
        return keyguard == null || keyguard.isKeyguardLocked();
    }
    private void captureReadiness() throws Exception {
        JSONObject facts = new JSONObject();
        for (String label : new String[] { "Send draft", "Jump to latest" }) {
            String prefix = "Send draft".equals(label) ? "send" : "latest";
            boolean present = false, visible = false, enabled = false;
            for (AccessibilityNodeInfo node : nodes())
                if (matches(label, node.getText()) || matches(label, node.getContentDescription())) {
                    present = true; visible |= node.isVisibleToUser(); enabled |= node.isEnabled();
                }
            facts.put(prefix + "_present", present).put(prefix + "_visible", visible).put(prefix + "_enabled", enabled);
        }
        AccessibilityNodeInfo root = activeRoot();
        Bundle state = invoke("status", null);
        facts.put("latest_requested", latestRequested).put("latest_acked", latestAcked)
            .put("root_target", root != null && matches(TARGET, root.getPackageName()))
            .put("screen_interactive", screenInteractive()).put("keyguard_locked", keyguardLocked())
            .put("primary_action", state.getString("primary_action")).put("primary_enabled", state.getBoolean("primary_enabled"))
            .put("engine_foreground", state.getBoolean("engine_foreground")).put("branch_count", state.getInt("branch_count"));
        for (String key : new String[] { "context_ready", "busy", "failed", "credential_ready", "credential_reason",
                "error_present", "error_code", "failure_reason", "readiness_complete" })
            if (state.containsKey(key)) facts.put(key, state.get(key));
        readinessDiagnostics = facts;
    }

    private List<AccessibilityNodeInfo> nodes() {
        List<AccessibilityNodeInfo> result = new ArrayList<>();
        AccessibilityNodeInfo root = activeRoot();
        if (root != null && matches(TARGET, root.getPackageName())) collect(root, result);
        return result;
    }
    private AccessibilityNodeInfo activeRoot() {
        // Send/Latest replace each other in the same window. Reacquire current
        // semantics rather than trusting a cached node from the previous turn.
        if (android.os.Build.VERSION.SDK_INT >= 33) getUiAutomation().clearCache();
        return getUiAutomation().getRootInActiveWindow();
    }
    private void collect(AccessibilityNodeInfo node, List<AccessibilityNodeInfo> result) {
        if (result.size() >= 512) return;
        result.add(node);
        for (int i = 0; i < node.getChildCount(); i++) { AccessibilityNodeInfo child = node.getChild(i); if (child != null) collect(child, result); }
    }
    private boolean hasLabel(String label) {
        for (AccessibilityNodeInfo node : nodes()) if (node.isEnabled() && node.isVisibleToUser() &&
            (matches(label, node.getText()) || matches(label, node.getContentDescription()))) return true;
        return false;
    }
    private boolean hasExactText(String text) {
        for (AccessibilityNodeInfo node : nodes()) if (node.isVisibleToUser() && matches(text, node.getText())) return true;
        return false;
    }
    private boolean clickLabel(String label) {
        for (AccessibilityNodeInfo node : nodes()) if (node.isEnabled() && node.isVisibleToUser() &&
            (matches(label, node.getText()) || matches(label, node.getContentDescription()))) {
            AccessibilityNodeInfo click = node;
            for (int depth = 0; click != null && depth < 4; depth++, click = click.getParent())
                if (click.isClickable() && click.performAction(AccessibilityNodeInfo.ACTION_CLICK)) return true;
        }
        return touchLabel(label);
    }

    private boolean touchLabel(String label) {
        if (!("Send draft".equals(label) || "Jump to latest".equals(label)) || !screenInteractive() || keyguardLocked()) return false;
        AccessibilityNodeInfo root = activeRoot();
        if (root == null || !matches(TARGET, root.getPackageName())) return false;
        List<AccessibilityNodeInfo> fresh = new ArrayList<>(); collect(root, fresh);
        Rect window = new Rect(); root.getBoundsInScreen(window);
        for (AccessibilityNodeInfo node : fresh) {
            if (!node.isVisibleToUser() || !node.isEnabled() || node.getWindowId() != root.getWindowId() ||
                !(matches(label, node.getText()) || matches(label, node.getContentDescription()))) continue;
            Rect bounds = new Rect(); node.getBoundsInScreen(bounds);
            if (!bounds.intersect(window) || bounds.isEmpty()) continue;
            float x = bounds.exactCenterX(), y = bounds.exactCenterY();
            long now = SystemClock.uptimeMillis();
            MotionEvent down = MotionEvent.obtain(now, now, MotionEvent.ACTION_DOWN, x, y, 0);
            MotionEvent up = MotionEvent.obtain(now, SystemClock.uptimeMillis(), MotionEvent.ACTION_UP, x, y, 0);
            down.setSource(InputDevice.SOURCE_TOUCHSCREEN); up.setSource(InputDevice.SOURCE_TOUCHSCREEN);
            try {
                boolean accepted = getUiAutomation().injectInputEvent(down, true);
                boolean released = getUiAutomation().injectInputEvent(up, true);
                return accepted && released;
            } finally { down.recycle(); up.recycle(); }
        }
        return false;
    }

    private Bundle invoke(String method, Bundle input) throws Exception {
        return (Bundle) (input == null ? facade.getMethod(method, Instrumentation.class).invoke(null, this) :
            facade.getMethod(method, Instrumentation.class, Bundle.class).invoke(null, this, input));
    }
    private static JSONObject json(Bundle bundle) throws Exception {
        JSONObject result = new JSONObject();
        for (String key : bundle.keySet()) { Object value = bundle.get(key); result.put(key, value == null ? JSONObject.NULL : value); }
        return result;
    }
    private static void keys(JSONObject object, String... allowed) {
        require(object.length() == allowed.length);
        for (String key : allowed) require(object.has(key));
    }
    private JSONObject read(File file) throws Exception {
        require(file.getCanonicalFile().equals(new File(directory.getCanonicalFile(), file.getName())));
        require(file.isFile() && file.length() > 0 && file.length() <= MAX_COMMAND);
        try (FileInputStream input = new FileInputStream(file)) {
            byte[] bytes = input.readNBytes(MAX_COMMAND + 1);
            require(bytes.length <= MAX_COMMAND);
            return new JSONObject(new String(bytes, StandardCharsets.UTF_8));
        }
    }
    private void write(File destination, JSONObject object) throws Exception {
        byte[] bytes = object.toString().getBytes(StandardCharsets.UTF_8);
        require(bytes.length <= MAX_RESPONSE && responseBytes + bytes.length <= 8 * 1024 * 1024);
        responseBytes += bytes.length;
        File temporary = new File(destination.getParentFile(), destination.getName() + ".tmp");
        require(!destination.exists() && temporary.createNewFile());
        try (FileOutputStream output = new FileOutputStream(temporary)) { output.write(bytes); }
        require(temporary.renameTo(destination));
    }
    private static void pruneExpired(File root) throws Exception {
        File[] runs = root.listFiles(); require(runs != null && runs.length <= 16);
        for (File run : runs) {
            if (!run.getName().matches("[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}")) continue;
            require(run.getCanonicalFile().equals(new File(root.getCanonicalFile(), run.getName())));
            if (System.currentTimeMillis() - run.lastModified() > 15 * 60 * 1000L) deleteOwned(run);
        }
        File[] retained = root.listFiles(); require(retained != null && retained.length < 16);
    }
    private static void deleteOwned(File run) throws Exception {
        File[] files = run.listFiles(); require(files != null);
        for (File file : files) {
            require(file.getCanonicalFile().equals(new File(run.getCanonicalFile(), file.getName())) && file.isFile());
            require(file.delete());
        }
        require(run.delete());
    }
    private static boolean nonblank(String text) { return text != null && !text.trim().isEmpty(); }
    private static boolean matches(String expected, CharSequence actual) { return actual != null && expected.contentEquals(actual); }
    private static void require(boolean value) { if (!value) throw new IllegalStateException("typed_benchmark_check_failed"); }
}
