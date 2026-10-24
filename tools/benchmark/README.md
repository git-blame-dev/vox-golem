# Shared application benchmarks: typed and synthetic voice

Historical results are maintained outside the checkout. In the local workspace,
`<checkout-parent>/docs/benchmarks/README.md` indexes dated, sanitized summaries;
that directory is not part of a Git clone. Keep methodology and scenarios here,
not local run histories, logs or binaries.

Typed normalized JSON reports default to
`<checkout-parent>/artifacts/benchmarks/typed/` (override with `--output`). This is
outside the repository, private and governed by the existing 90-day, 100-file,
8 MiB report limits. Raw reports are not permanent archives; the external dated
summary preserves the conclusions and conditions after raw evidence expires.

`scenarios.json` is the shared six-question source used by the Rust host matrix
and the Python full-app controller. Coding intentionally retains its historical
Web-on flag despite asking for no lookup. History in the app is **one UI seed
plus follow-up, not the Rust host's 24 injected messages**.

## Typed scope and current acceptance boundary

The Android driver attaches to an **existing running** A17 app. It does not
build, install, pair, restart ADB, scan networks, clear data, dismiss keyguards,
or provision providers. Existing Python `websockets` is required. The runner
selects `--model` through the visible model control, confirms it canonically,
and restores the initial model. Prefetch/completion and secondary generation
profiles must be off. A non-empty existing composer or open dialog blocks the
run. Model, TTS and Web changes use semantic UI controls; initial preferences are
checked again after restoration. Old conversations are not exported or erased.
Synthetic conversations remain in the app; the last synthetic conversation stays
selected. Do not interact concurrently with the app during a run.

Desktop dispatch imports the sibling `desktop.py` lazily; it fails closed if the
qualified adapter is absent. Common authorization requires `--live`, `--binary`,
`--tools`, `--config`, `--offline-evidence`, model `custom-luna-none`, exactly one
repetition, a 10–180 second turn timeout, and no physical-device flags. The
adapter owns existing runner namespace, offline-evidence and fixed-ledger gates;
common authorization does not replace or bypass them. Desktop integration is a
separate acceptance boundary. Windows is unsupported for this typed adapter. Desktop runtime
execution remains unverified. The shared command was exercised over SSH/ADB on
the A17 (2026-10-20): six workflows completed, three passed automated checks and
three research cases remained explicitly unverified, with zero failed checks.
Original preferences and owned forwarding/tunnel cleanup were verified. This was
one debug/timing run, not optimized-release or matched-baseline performance.

## Commands

### Synthetic voice: capability and evidence boundaries

Use this same controller with `--workload synthetic-voice`; it does not change the
typed six-case order, history conditions or historical results. Implementation,
fake tests and a completed workflow are not platform runtime acceptance. Current
Android/Linux/Windows acceptance conclusions belong in the outer dated history,
not an inferred pass here. A failed phone attempt with unknown admission counts
or forced profile recovery is not an accepted trial or refunded budget; record
actual attempts and their disposition in the outer history.

| Platform/profile | Input and output | Budget / result boundary |
| --- | --- | --- |
| Android A17 shared arithmetic | Finite synthesized PCM into the real recognizer; real semantic Ask, frontend Channel, canonical/UI answer and native TTS | One explicitly authorized live trial; exact answer and separate retirement/profile proofs; overall unverified, exit 2 on a valid workflow |
| Linux shared arithmetic offline | Normal synthetic capture, wake/VAD/Parakeet; real Piper/backend through private ALSA null output | Zero live admissions, one fake-model admission; schema-3 offline qualification is not acoustic acceptance; shared exit 2 |
| Native Windows shared arithmetic diagnostic | Staged finite PCM and real native voice flow; WASAPI render diagnostic | Zero live admissions; schema 4 stays diagnostic-only, `passed=false`, no live qualification; completed diagnostic maps to shared exit 2 |
| Wear legacy synthetic voice | Existing **four-word** fixture, recognizer, native model/presentation/TTS | Separate protocol-1 one-trial profile; earlier physical acceptance does not qualify shared arithmetic |

Only arithmetic has additive `voice` metadata in `scenarios.json`.
`voice_contract.py` owns the spoken prompt, token aliases, exact-EOF matching and
canonical asset; the expected answer remains the parent scenario's `210`, Web off.
Do not create another question/alias catalog. Current canonical asset: 252 bytes,
SHA-256 `6613aaae4a7b710a34af90ea68ec67231f0494c43521cc0cedcb9d6904a7aa6d`.
Generate only through the production parser:

```sh
python3 tools/benchmark/voice_contract.py --write-android-assets "$GENERATED_ASSET_DIR"
python3 tools/benchmark/benchmark.py --platform android --workload synthetic-voice --list
python3 tools/benchmark/benchmark.py --platform desktop --workload synthetic-voice --list
```

The generated relative path is `voxgolem-benchmark/arithmetic-voice.json`.
Listing is transport- and artifact-write-free. The desktop runner's
`fixtures.arithmetic_profile()` consumes this same contract. Fixture generation
and preparation are not part of Activity-relative or submit-to-output timing.

### Synthetic voice: Android A17

This is **not** the typed CDP adapter: the fixed same-signer SDK helper reserves
the process before Application startup, gates normal background initialization,
and drives the production semantic Ask path. It does not inject a native engine
prompt or require WebView debugging. Supported target: non-watch `SM-A176B`, API
35+, ARM64, optimized non-debuggable release/profileable build. Both supplied
bounded local APKs must match installed APK hashes. Signing compatibility and
optimization must be independently verified; labels and hashes alone prove neither.

Build only after the host/toolchain and build permission gates are satisfied.
Use existing JDK 21 / SDK / NDK locations and the existing **Linux Bun**, not a
Windows/npm shim. From the checkout, a benchmark-enabled release build is:

```sh
export JAVA_HOME="$EXISTING_JDK21" ANDROID_HOME="$EXISTING_ANDROID_SDK" NDK_HOME="$EXISTING_ANDROID_NDK"
export PATH="$JAVA_HOME/bin:$HOME/.bun/bin:$PATH"
export GRADLE_OPTS='-Dorg.gradle.workers.max=1 -Dorg.gradle.parallel=false -Dorg.gradle.jvmargs=-Xmx1536m'
(cd apps/android-tauri/src-tauri && \
  ORG_GRADLE_PROJECT_voxgolemSyntheticVoiceBenchmark=true \
  cargo tauri android build --target aarch64 --apk --ci -- --locked)
(cd apps/wear && ./gradlew :voice-benchmark:assemblePerformance --no-daemon)
```

The opt-in Gradle property compiles the phone facade and generated contract asset.
`make android-build` currently builds a **debug development APK**, not this
acceptance artifact. Checked-in release configuration is unsigned: stage candidate
copies outside Git and use the approved compatible signing process for target,
test-only helper and later ordinary release. Never assume the helper's default
debug certificate matches the installed target. Verify actual application IDs,
certificates, optimization and debuggability before any preserving in-place update.
The helper's test debuggability does not authorize a debuggable target.

For separately authorized deployment, use explicit verified transport selection;
the helper requires `install -r -t`. For example, after certificate comparison:

```sh
"$APKSIGNER" verify --print-certs "$PHONE_APK"
"$APKSIGNER" verify --print-certs "$HELPER_APK"
"$ADB" -s "$A17_TRANSPORT" install -r "$PHONE_APK"
"$ADB" -s "$A17_TRANSPORT" install -r -t "$HELPER_APK"
```

These commands do not grant permission to deploy, unlock, collect speech or retry.
Keep the A17 **unlocked and awake at actual test time**, not throughout builds.
Do not automatically dismiss keyguard or grant unrelated permissions. After an
exclusive testing window and exactly one approved provider admission are confirmed:

```sh
python3 tools/benchmark/benchmark.py --platform android --workload synthetic-voice \
  --device a17 --allow-device --live --adb "$ADB" \
  --build-variant release --apk "$PHONE_APK" --helper-apk "$HELPER_APK" \
  --model custom-luna-none --repetitions 1 --max-live-calls 1
```

Explicit authorized SSH-hosted ADB can use the existing `--ssh` / `--remote-adb`
options. There is no port forwarding, typed history, `--cases`, stage collection
or UI probe in this profile. The controller refuses existing target/helper
processes; it never kills foreign work, installs, pairs or automatically reruns.

Protocol 2 separates a complete STATUS receipt from native resource retirement,
independent process/Activity-WebView retirement, and a second fresh-process private
WebView-profile cleanup. Phase-one FINAL must report exactly
`native_resources_retired=true`, `activity_webview_retirement_pending=true`,
`native_cleanup_verified=false`, `tts_retired=true`, `profile_cleanup_pending=true`
and `cleanup_verified=false` before its cleanup token is accepted. This proves
SDK/writer/provider/actor/store retirement, not Activity/WebView retirement; the
helper does not call Activity.finish before FINAL because Tauri may exit the process
before cleanup finishes. Old ambiguous `native_cleanup_verified=true` proof is rejected.
Only independent target/helper process absence after instrumentation ends establishes
process/Activity-WebView retirement and admits fresh cleanup mode. A surviving or
foreign process blocks without killing or retrying: resource success remains recorded,
but full native cleanup stays pending and phase two is not launched.
Process absence records process/Activity-WebView retirement independently; combined
native cleanup remains pending until fresh phase two additionally proves
`producer_exit_verified=true` and `activity_webview_retired=true`, alongside
`profile_cleaned=true`, `storage_cleanup_verified=true` and `cleanup_verified=true`, and its transport/process exit
checks succeed. Missing or false phase-two proof preserves prior resource/process
evidence without promoting combined cleanup. Phase two does not emit or require
`native_cleanup_verified=true` and cannot substitute for prior SDK retirement proof.
WebView-only cleanup receipts are rejected; only the source-defined phone-exclusive
benchmark namespaces (`synthetic-phone-ask-<uuid>`, `app_webview_synthetic_phone_<hex32>`,
`WebView_synthetic_phone_<hex32>`, `WebView/synthetic_phone_<hex32>` and three scoped
`shared_prefs` files) under `no_backup/phone-benchmark.lock` are supported. Failure diagnostics
retain only provided canonical bounded progress counts: `deleted_root_count` (0–3,
WebView roots only), `deleted_owned_directory_count` (0–32),
`deleted_preference_count` (0–24), and `deleted_session_count` (0–8).
The opaque cleanup token is
transport-only, never a report identifier. A valid failed/running STATUS is safe
diagnostic evidence, not a reason to interrupt the helper's `finally` cleanup.
Proven owned resource retirement followed by process absence can authorize cleanup-only even after a failed trial;
that still returns failure. Missing final proof or timeout retains known complete
counters but leaves retirement unverified. Unknown admissions stay unknown, not zero.
Host observation is 300 seconds for phase one and 60 seconds for cleanup, with a
separate bounded local-transport teardown allowance of at most 15 seconds each.
Transport exit is not native SDK/actor/resource retirement.

Successful phone trials require both `profile_mapping_verified` and
`frontend_ask_clicked`, plus exact canonical/UI answer checks. Optional
`FRONTEND_ASK_CLICKED_ms`, `FRONTEND_SUBMIT_OBSERVED_ms` and
`PRESENTATION_DOM_OBSERVED_ms` are bounded first-observation offsets from Activity
creation in the device-monotonic clock, not painted-pixel or acoustic durations.

If ordinary two-phase proof was lost, stop and obtain separate recovery approval.
The benchmark-only fixed helper mode `cleanup-pending` admits **zero** model turns.
The current session owner blocks new reservations while any phone benchmark namespace
entry is present or the process-lifetime `no_backup/phone-benchmark.lock` lease is held.
Recovery acquires `phone-benchmark.lock`, validates the complete bounded set of
phone-exclusive namespace entries before deletion, and fails closed on active producer
locks, malformed namespace names, symlinks or exceeded aggregate bounds. Zero discovered
tokens reports `not_started`, not successful cleanup.

Cleanup failures can include `cleanup_failure_reason`, a fixed guard category,
and optional established facts: `pending_count_class`, `producer_exit_verified`,
`deletion_started` and `deleted_root_count` (0–3, WebView-only for one session).
Additional current-invocation counts are `deleted_owned_directory_count` (0–32),
`deleted_preference_count` (0–24) and `deleted_session_count` (0–8).
No session tokens, paths or raw
exception messages are exported. A candidate count describes namespace-shaped
entries, not validated ownership. The deleted-root count includes only completed
root deletions; a failing root may have been partially deleted. Missing facts
remain unknown, and failed cleanup never becomes verified from these diagnostics.
These fields do not authorize arbitrary record selection, schema migration,
producer-presence bypass or broader deletion. Older generic failures cannot be diagnosed
retroactively from their exception class.

Before invoking it directly, independently verify target/helper process absence;
do not use instrumentation startup to force existing work out:

```sh
"$ADB" -s "$A17_TRANSPORT" shell am instrument -w -r -e mode cleanup-pending \
  dev.gitblame.voxgolem.android.test/dev.gitblame.voxgolem.android.speechtest.PhoneSyntheticVoiceInstrumentation
```

`cleaned_after_process_exit` explicitly has `native_cleanup_verified=false`;
it requires current `storage_cleanup_verified=true` and reports a bounded
`recovered_session_count` (1–8), not normal-trial admission evidence.
`not_started` is not cleanup success. Recovery does not certify the failed trial,
refund its budget or authorize a rerun. Do not substitute arbitrary paths/tokens,
scripts, native-cache deletion or ordinary preference/data wiping. After owned
cleanup, rebuild with `ORG_GRADLE_PROJECT_voxgolemSyntheticVoiceBenchmark=false`,
verify the facade/asset are absent and restore the compatible ordinary optimized
APK by authorized in-place update, preserving app data. Keep candidate, installed
comparison and ordinary APK copies outside Git.

### Synthetic voice: shared desktop entry

Build/staging and platform prerequisites are in `tools/app-bench/README.md`.
Use a fresh output path outside Git whose parent already exists; the authoritative
child runner creates/protects it. The common wrapper does not apply Unix chmod or
require Git metadata on Windows. Never supply old `--offline-evidence` to this profile.

```sh
python3 tools/benchmark/benchmark.py --platform desktop --workload synthetic-voice \
  --desktop-platform linux --max-live-calls 0 --model custom-luna-none --repetitions 1 \
  --binary "$BINARY" --tools "$BENCH_TOOLS" --config "$SOURCE_CONFIG" \
  --tts-model "$TTS_MODEL" --wake-phrase "$SYNTHETIC_WAKE_PHRASE" --output "$FRESH_RUN"
```

Native Windows PowerShell, from the explicitly staged checkout and existing private
Python environment with Selenium (not Linux Python invoking a guessed bridge):

```powershell
& $PRIVATE_PYTHON tools/benchmark/benchmark.py --platform desktop --workload synthetic-voice `
  --desktop-platform windows --max-live-calls 0 --model custom-luna-none --repetitions 1 `
  --binary $BINARY --tools $WINDOWS_DRIVERS --config $SOURCE_CONFIG `
  --tts-model $TTS_MODEL --wake-phrase $SYNTHETIC_WAKE_PHRASE `
  --windows-manifest $WINDOWS_MANIFEST --output $FRESH_RUN
```

`--desktop-platform` defaults only within this desktop voice adapter to the actual
native Linux/Windows host; explicit mismatches fail closed. The four new desktop
voice options are rejected for typed, Android and Wear workloads. Desktop voice
requires **exactly zero live budget**, not the default seven: `--live`, physical
flags, old qualification evidence, history, cases, stage/UI-probe and port controls
are rejected. It delegates to the existing single arithmetic runner profile, not
the legacy three-case/fixed-12 live ledger. Neither desktop profile is live-eligible.

The wrapper reads only bounded child `report.json` metadata and prints a content-free
summary; it does not rewrite the child report or create a second qualification.
Linux child exit 0 plus offline qualification maps to shared `unverified` / exit 2.
Windows child exit 1 is expected even for a completed diagnostic; only a valid
schema-4 workflow with owned cleanup maps to shared exit 2. Failures, unexpected
exits, missing/malformed reports and timeout exit 1 with cleanup unverified where
proof is absent. The 900-second desktop observation watchdog has a separate
at-most-10-second local reap allowance; killing the runner proves no app cleanup.

Phone schema-2 and Wear schema-1 reports share outer `artifacts/benchmarks/voice/`
retention: private files, 128 KiB/report, 90 days, 100 files, 8 MiB. Desktop uses
its existing marked child-report collection (90 days, 128 files, 8 MiB); the common
wrapper refuses child reports above 128 KiB. See desktop docs for the distinct
16 MiB fixture-bundle limit and retained private profiles. Export only safe hashes,
categorical stages/outcomes and bounded counters/offsets: no raw content, tokens,
device IDs, credentials, provider bodies or endpoints.

### Synthetic voice: legacy Wear pipeline (preserved profile)

Transport-free listing:

```sh
make benchmark PLATFORM=wear DEVICE=watch7 BENCHMARK_FLAGS="--workload synthetic-voice --list"
```

After separately authorized build/installation, artifact/signing verification,
physical testing, playback and live-provider approval:

```sh
make benchmark PLATFORM=wear DEVICE=watch7 BENCHMARK_FLAGS="--workload synthetic-voice --allow-device --live --adb $ADB --build-variant performance --apk $WEAR_APK --helper-apk $HELPER_APK --model custom-luna-none --max-live-calls 1"
```

For an authorized SSH-hosted ADB, append the existing explicit `--ssh` and
`--remote-adb` arguments. Both supplied APKs must match their installed hashes;
the runner does not install them. The normal default seven-call typed budget is
rejected: this workload requires `--max-live-calls 1`. Typed `--cases`, timing,
history, port, stage-collection and UI-probe controls do not apply. Native timing
thresholds are fixed at 30 seconds per trial and 90 seconds for the workload;
the host watchdog is separate. New shared voice reports default to outer
`artifacts/benchmarks/voice/voice-benchmark-*.json`, with the same private-file,
128 KiB/report, 90-day, 100-file and 8 MiB limits. A valid pipeline remains overall
`unverified` (exit 2): it is not answer-correctness or acoustic certification.

This Wear synthetic-voice workload is separate from shared arithmetic and the
six-case typed suite: **one existing four-word Wear fixture, one `custom-luna-none` trial**:
supplied PCM through the real recognizer, production voice admission, model,
native presentation and local TTS. It does not run arithmetic/history or certify
answer correctness. The earlier physically passed Wear profile remains distinct;
do not relabel it as arithmetic acceptance. The desktop legacy `voice` entry is
also a different, TTS-disabled profile, not equivalent acceptance.

The native helper remains SDK-only, same-signer and compatible with an
independently optimized non-debuggable target. Use a matching newly built helper
and target: old instrumentation without the versioned final cleanup proof fails
closed. A complete trial status precedes cleanup and is **not** sufficient. Final
acceptance requires the helper's successful terminal result and explicit output
retirement proof, including retained TTS SDK ownership rather than merely an empty
playback lease. Failed resources/leases are never reset to manufacture idle.

The host observation watchdog is 300 seconds: a conservative 239-second allowance
for existing explicit waits, up to two additional five-second output-retirement
polls, 30 seconds for startup and 21 seconds for transport/scheduling. This is
**not a hard native completion bound**. Main-thread synchronization, SDK and file
operations can remain blocked. Timeout, interrupted transport or incomplete final
evidence leaves native retirement unverified; terminating the owned local ADB/SSH
transport does not establish native cleanup. Stop and reconcile before any new
run; there are no automatic retries or extra trial admissions.

Stage offsets are first observations relative to Activity creation in the same
device-monotonic clock, not complete duration spans. They exclude fixture setup
and host transport. One accepted final is not a raw SDK callback count; one
generation-begin observation is not an independently measured HTTP request count.
TTS callbacks do not prove acoustic output, and native draw submission does not
prove painted pixels. Fixture provenance describes the actual generated PCM;
missing generator/voice/provider identity remains unverified. No matched baseline,
cold-start coverage or performance improvement is claimed.

Offline parser/controller tests are not optimized-device acceptance. Build and
installation, an exclusive Watch testing window, physical automated testing,
synthetic speech playback and live-provider use require their existing explicit
authorizations. The runner must refuse pre-existing target processes rather than
terminate user or relay work. It does not install, pair, restart ADB, dismiss a
keyguard, clear data or access a phone. Historical typed/history results remain
separate and must not be rerun or relabelled by a voice invocation.

For a later authorized acceptance run, verify the installed optimized target and
helper identities first, reserve an exclusive testing window and exactly one
trial admission, then inspect the versioned final cleanup proof separately from
the trial counters. Check the actual PCM hash/format/duration and timing meanings,
and retain a sanitized dated interpretation in the outer benchmark history. A
blocked preflight, missing final evidence or failed cleanup is not permission to
rerun, retire an unrelated process or spend another admission.

### Native Wear adapter

Wear is a separate native acceptance boundary, not the Android WebView adapter.
The initial supported profile is the physical Watch7 `SM-L315F`, API 36,
`armeabi-v7a`, a matching non-debuggable optimized `performance` app, and the
same-signer typed instrumentation helper. Runtime acceptance of a changed driver
must be recorded separately from offline adapter tests.

Wear runtime acceptance remains partial: the initial six-case selection completed
the first five workflows and its history seed, but the history follow-up failed
Send readiness. After the subsequent UI corrections, a separately approved
targeted `--cases history --max-live-calls 2` run (2026-10-21) completed the seed
and follow-up with exact answers, canonical/projected equality, accessibility
answer equality and native cleanup; rendered-row identity remains unverified.
That targeted run is partial coverage under different artifact conditions, so the
two reports must not be combined into a six-case pass. Keep the adapter
experimental until one unchanged six-case run completes.

Each native turn also reports `canonical_ancestry`: the canonical head turn's
explicit `parent` must equal the previous validated turn in the same isolated
session (`null` for a session's first turn), so a history follow-up is
structurally proven to extend its seed. A parent mismatch invalidates native
execution and is reported as `wear_native_canonical_ancestry_failed`; a malformed
parent or a canonical page whose last turn is not its head fails native canonical
validation instead. The
2026-10-21 targeted run above predates this check; its runtime acceptance is
recorded separately in the outer benchmark history.

The Python controller retains all six scenarios, ordering, answer validation,
history-seed accounting, live budgets and report retention. The native driver
prepares its synthetic draft through the existing validated native draft API,
waits for that draft to reach the real chat UI, then uses the real Send action
in an isolated typed session. Draft preparation is outside the timed interval;
it must not submit through a direct engine shortcut, invoke speech, or change user preferences.
Configuration of the isolated session is not a benchmark of the production model
selector or settings UI. Owned synthetic stores are retired and cleaned up; this
differs from the phone adapter's retained synthetic chats.

Device-monotonic turn timings exclude SSH/mailbox response retrieval. They are
labelled separately from phone/desktop host-observed timing and are not directly
interchangeable baselines. Native presentation projection, accessibility-visible
text, and layout/draw-submission observations are distinct evidence; none proves
painted pixels. Wear source-list presentation/page-read execution checks remain
unverified where the native surface does not expose the requisite evidence.

Help/list remain transport-free:

```sh
make benchmark PLATFORM=wear DEVICE=watch7 BENCHMARK_FLAGS="--list"
```

After separately authorized build/installation, signing/artifact verification,
physical automated-testing and live-provider approval:

```sh
make benchmark PLATFORM=wear DEVICE=watch7 BENCHMARK_FLAGS="--allow-device --live --adb $ADB --ssh $SSH_HOST --remote-adb $REMOTE_ADB --build-variant performance --apk $WEAR_APK --model custom-luna-none --max-live-calls 7"
```

Omit SSH/remote-ADB arguments for an explicitly configured local ADB. The runner
does not pair, install, clear data, reboot, restart ADB, select a phone, or collect
user speech. The initial profile permits one repetition, one history seed and at
most seven turn admissions. Subsets use the same `--cases` mechanism and must be
reported as partial coverage. Phone `--collect-stages` is not enabled for Wear.
There is no cold-start or device-boot workload in this extension.

Before spending live admissions, append `--wear-ui-probe` to the same authorized
Wear command to prepare a long synthetic draft, exercise Latest viewport restoration,
and reach enabled Send **without pressing it**. An unarmed native Send fence prevents
incidental probe touches from starting generation; the timed submit path receives
one request-bound permit immediately before its real Send action.
This reports a separate `wear-native-ui-probe` workload with zero submissions,
not a six-case suite pass. It uses an existing shared synthetic prompt and still
requires explicit device authorization and a matching installed artifact.
Starting instrumentation can terminate an old process, so Wear refuses any
existing target app process before startup; the controller never force-stops
pre-existing work to get past that check.

Android force-stops the target when instrumentation finishes. The process is then
commonly restarted within seconds to host `AssistantReplicationListenerService`
(the Wearable Data Layer listener; the triggering event and caller are not
established), so a probe or run is commonly followed by a cached app process that
blocks the next run's preflight. Confirm it is that process (event log
`am_proc_start ... service ... AssistantReplicationListenerService`, cached state,
no running services, so no phone-relay request is in flight) and retire it only
with explicit approval, for example `am kill`, which affects background processes
only. The controller does not do this automatically.

This typed suite does not measure keyboard/IME navigation, microphone readiness,
speech recognition, or spoken playback. Existing synthetic voice benchmarks remain
separate workloads with their own permissions, evidence and budgets.

### Android and desktop

Help/list are transport-free:

```sh
python3 tools/benchmark/benchmark.py --help
python3 tools/benchmark/benchmark.py --platform android --list
```

The project entry point wraps the same script:

```sh
make benchmark PLATFORM=android DEVICE=a17 BENCHMARK_FLAGS="--list"
```

For an explicitly authorized Linux desktop live run, after producing the existing
same-binary offline qualification described in `tools/app-bench/README.md`:

```sh
make benchmark PLATFORM=desktop BENCHMARK_FLAGS="--live --binary $BINARY --tools $BENCH_TOOLS --config $SOURCE_CONFIG --offline-evidence $OFFLINE_REPORT --model custom-luna-none --output $PRIVATE_REPORT_DIR"
```

Desktop uses a fresh isolated app process, not the attached Android process.
The prior three-case offline report is a lifecycle/safety foundation, **not**
offline qualification of the new six-question suite. The existing runner reserves
its fixed live allowance and requires matching current harness/binary hashes;
stale qualification is rejected. Desktop runtime execution remains unverified
until that command is explicitly authorized and actually run.

Following explicit physical-device/live authorization and the host's device
access instructions, local ADB:

```sh
python3 tools/benchmark/benchmark.py --platform android \
  --device a17 --allow-device --live --adb "$ADB" \
  --model "$MODEL" --port 9223 --build-variant debug \
  --output ../artifacts/benchmarks/typed
```

ADB on an explicitly selected SSH host:

```sh
python3 tools/benchmark/benchmark.py --platform android \
  --device a17 --allow-device --live --adb "$ADB" \
  --ssh "$SSH_HOST" --remote-adb "$REMOTE_ADB" \
  --model "$MODEL" --port 9223 --build-variant debug \
  --output ../artifacts/benchmarks/typed
```

Only advertised `model:SM_A176B` transports are candidates, and exactly one is
required. Model/API/ARM64 identity is queried before app access. Android API **35
or later** is the explicit minimum (Android 15), not an assumption that every A17
is API 36. The actual API is recorded. A unique app
WebView, visible focused page, idle canonical state and selected model are
required. Both ADB forward and optional SSH loopback tunnel are run-owned.
The forward uses `--no-rebind`; cleanup refuses to remove a changed mapping.
Neither serials nor SSH hosts, socket targets or provider endpoints enter reports.
SSH uses argv and a quoted remote command, never a local shell; both operations
and the tunnel use noninteractive BatchMode and bounded connection timeouts. No SSH tunnel
or ADB forwarding is established by list/help.

`--apk /outside/repository/synthetic-build.apk` hashes a supplied artifact and
requires equality with the installed APK; it does not install anything. Reports
include installed APK SHA-256, package version code, debuggability, API/ABI,
source revision/dirty flag, scenario digest and harness digest. Signing certificate validation
remains explicitly unverified. `--build-variant` is a caller-supplied label, not
proof of optimization, but a `debug` label requires installed debuggability and
`release`/`profileable` require non-debuggability; contradictions block before
generation. WebView debugging availability is required even when an
optimized build is requested; debug timings never establish release performance.

Default repetition count is one (seven UI admissions including history seed),
bounded to three. To request two repetitions also supply `--max-live-calls 14`.
Android may select a subset with `--cases ID [ID ...]`; IDs must be unique and
always run in canonical suite order. For a later explicitly authorized retry of
only the page-read and mixed cases, append **`--cases page_read mixed`** to the
Android command above. `--list --cases page_read mixed` lists that selection
without transport access. Desktop accepts only the full suite; subsets block
before its adapter is called, without changing its fixed live ledger.

The full scenario source is validated before selection. Reports record requested,
selected and omitted IDs, full/partial coverage, and planned admissions. Each case
costs one admission except history, which costs two (UI seed plus follow-up), per
repetition. The default admission budget remains seven; an explicit smaller
budget must cover the selected cases. A successful subset is still an overall
`unverified` partial suite, never a full-suite pass. Executed case rows/counts
remain separate from the selected count, including when a blocker stops later cases.
There are no controller generation retries; failure stops the suite. The budget
bounds UI admissions, not native provider/tool-internal requests or retries.
The report separately counts acknowledged Send submissions and newly observed UI
admissions, including the history seed and partial failures. An overall suite
deadline bounds the selected admissions per repetition plus explicit transition overhead;
cleanup has additional bounded cancellation/control deadlines.

## Validation and measurement

One shared controller calls `scripts/android-e2e-smoke.py`'s `exercise_ui`, with
optional pre-submit, submission, admission and terminal-validation callbacks. There is no
second Android admission/polling loop. Terminal validation requires one canonical
turn and one UI answer row for the newly admitted request, fresh execution metadata
in the current session, exact canonical/UI text equality and source-list equality.
Arithmetic and marker answers are exact; coding checks Rust function/slice/return
structure, not compilation. Research requires sources and relevant official domains.
Research semantic/factual accuracy remains explicitly `unverified`.
For a correlated research reply with zero UI and canonical sources, observed native
provider diagnostics take priority and classify the case as **`blocked`** with
`reason=rate_limited`, `evidence=native_provider_status` and the same provenance.
Integrity checks must pass, and missing-source checks must be the only failures;
they remain visible. If sources and integrity checks succeed after provider fallback,
native diagnostics are retained but do **not** turn that result into a blocker.

The per-turn observer accepts only these exact native status forms (the middle dot
and spacing are significant):

```text
Rate limited · Exa · provider wait 60 s · local wait 300 s
Rate limited · Parallel · provider wait unknown · local wait 60 s
```

Providers are only `Exa` or `Parallel`. Seconds are nonnegative integers up to
31,536,000; `unknown` has no `s` suffix. Native code produces this category from an
HTTP 429 or recognized MCP rate-limit classification, not generated answer prose.
The observer is installed before submission on the normal composer form and examines
only `.composer__actions--secondary [role=status]` or `.composer__status` status
elements. It does not inspect transcript/answer nodes or unrelated form statuses.
Only matching provider enums/numbers are retained, latest per provider, at most two;
no raw status text, headers, URLs or identifiers enter the report. It is bound to
the owned submitted/admitted turn, read at terminal/failure even after the status
element disappears, and disconnected/deleted in `finally`. Web-off history seed
turns do not install an observer. There is no new polling loop or permanent logger.

`native_rate_limits` records distinguish `provider_retry_after_seconds` (nullable)
from `local_cooldown_seconds` (local fallback/cooldown policy). Both are remaining
delays, decayed conservatively with `ceil` using elapsed `performance.now()` in the
same WebView clock. A known native provider wait sets `provider_reset` to
`minimum_retry_delay`, **not a guaranteed quota reset or promise that a retry succeeds**.
An unknown provider wait remains `unknown` even when a local cooldown is known.
React may coalesce/remove a transient status before observation, or transport/ownership
checks may prevent collection: absent native evidence stays `unverified`, not proof
that no rate limit occurred. No automatic waiting, retry or additional admission occurs.

Only when native evidence is absent, an explicit model rate-limit/HTTP-429 claim may
instead classify the case as **`blocked`** with
`reason=rate_limited`, `evidence=model_reported` and
`provenance=model_reported_rate_limit`. This is **not authoritative provider HTTP
status evidence**. Integrity checks must pass, and missing-source checks must be
the only failures; they remain visible in the report. Ordinary source-free answers,
negative claims such as “not rate limited”, wrong arithmetic and integrity mismatches
are not reclassified. Timings and `workflow_completed` remain available, but a
completed blocked reply is not success. A blocker stops the remaining cases and
never retries automatically. `provider_reset` remains `unknown` and
`retry_after_seconds` null: model-generated dates do not establish provider reset
times or countdowns. The benchmark consumes the scoped native status; it does not
add provider telemetry, HTTP parsing, native callbacks or app logging itself.
The desktop wrapper propagates blocked child status/exit 3 and selection/coverage
fields without changing its qualification, namespace, reservation or full-suite gates.
Mixed research accepts async.rs, the specific `docs.rs/async-std` documentation
prefix, or `github.com/async-rs/async-std` repository prefix, alongside Tokio's
official domain or its specific `docs.rs/tokio` / `github.com/tokio-rs/tokio`
prefixes. Arbitrary GitHub repositories, lookalike prefixes, non-HTTPS
URLs and path-traversal alternatives are not official-source evidence.
Sources and answer prose do **not** prove page-read execution: those checks remain
`unverified`, not `pass`, until an authoritative scoped event surface is available.

Android WebView timings are host-monotonic UI-submit to first nonblank text and terminal-idle
observations, including 100 ms polling and transport overhead. They are warm
attached-app measurements, not startup, provider-only, painted-pixel or acoustic
latency; provider cold/warm state is uncontrolled. No matched baseline or speedup
is claimed. Per-turn observer bookkeeping and its CDP/WebDriver round trips can
perturb observation latency; they are not subtracted or treated as provider timing.

The scenario parser rejects unknown keys, incorrect types/IDs/policies, duplicate
keys and oversized values before any transport access. Fresh-chat confirmation
requires a changed canonical session, an empty transcript and empty composer;
an already-empty old session is insufficient.

Every typed non-list run preflights a writable private output directory before transport,
then attempts a schema-1 JSON report, including partial failures and
cleanup status, plus a categorical stdout summary. No prompts, answers, source URLs,
request/execution/conversation identifiers or raw exceptions enter these reports.
Reports require a private directory outside Git, use mode 0600, and are capped at
128 KiB each with retention at most 90 days, 100 files and 8 MiB. Cleanup attempts
cancellation of this run's active synthetic request, all three preference restorations,
its forward and tunnel even on errors. If the app disconnects or refuses UI controls,
restoration is reported failed; hard process termination cannot guarantee cleanup.
Cancellation is addressed to the admitted request after checking its exact canonical
session/execution, rather than clicking a potentially replaced composer Stop closure.
If another actor changes the session/request, cleanup never cancels the successor or
changes its preferences; the ownership-fence failure is explicit. Do not interact
concurrently. A transport failure before canonical admission can prevent safe claiming
and cancellation; this is a failed cleanup, never silently certified restoration.
Metrics and `workflow_completed` survive answer-validation failures. Stdout includes
completion and pass/unverified/fail/unsupported counts, admission counts and the report
path, including a separate blocked count. `pass` exits **0**, `fail` exits **1**,
`unverified` exits **2**, and `blocked` exits **3**. A completed
workflow is not equivalent to an accepted answer or verified page read.

## Android local-stage and SQLite attribution

Add **`--collect-stages`** to an authorized Android invocation to collect bounded,
content-free summaries from the app's existing diagnostic journal. A matching
instrumented APK is required. Desktop collection is explicitly unsupported for
now; shared portable storage instrumentation alone is not desktop acceptance.

Useful focused workloads (append these flags to the documented Android command):

```text
--collect-stages --cases arithmetic --repetitions 3 --max-live-calls 3
--collect-stages --cases history --history-seed-turns 8 --max-live-calls 9
--collect-stages --cases coding lookup page_read mixed --max-live-calls 4
```

The default history depth remains one normal UI seed. A deeper run records its
actual seed count and budgets every seed plus follow-up; it does not inject native
history. Stop on rate-limited search rather than retrying it to obtain timings.

Reports keep host-observed UI timings separate from Android workflow intervals
and Rust-local storage durations. SQL begin/read/write/prepare/commit are distinct
from codec, reducer and projection/context work. Begin may contain SQLite lock
contention; commit may contain WAL/sync work and is not an isolated fsync measure.
Inclusive API/dispatch/projection/quota spans overlap: **do not sum all phases**.
`new_chat_ready_observed_ms` measures separate host-observed setup, not pure DB
time. JNI output allocation and uncovered wrapper work remain residuals.

Collection uses same-device boot-time bounds and ephemeral journal relationships
only for correlation. Durations are calculated within their own clock domains;
no host/device timestamp subtraction is performed. Missing, ambiguous, dropped or
incomplete per-call diagnostics remain unverified, not zero. Raw journal records,
join identities and user data do not enter the exported report. Collection occurs
outside timed streaming/admission. Report truncation is explicit within the
existing 128 KiB budget. Instrumented debug results establish a current diagnostic
baseline, not optimized-release performance or historical SQLite regression proof.

## Offline verification commands

```sh
python3 -m unittest discover -s tools/benchmark -p 'test_*.py'
python3 -m unittest discover -s scripts -p 'test_android_e2e_smoke.py'
python3 -m unittest discover -s tools/app-bench -p 'test_runner.py'
cargo test --locked -p vox-golem --example search_bench
```

Python tests use fake CDP/settings/ADB subprocess boundaries. They prove controller
ordering, validation policy and ownership handling, not real Tauri execution,
provider behavior, physical device selection, SSH connectivity or UI restoration.
When an existing Node or Bun executable is available, optional unittest cases also
execute the generated guard JavaScript against synthetic native/DOM state, including
exact-execution cancellation, cancelled requests absent from committed history,
foreign successors and unchanged-empty-session rejection. This is still not a
real WebView or device test; no JavaScript dependency is installed for these checks.
