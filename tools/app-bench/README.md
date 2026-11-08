# Unified application benchmark

Desktop controllers derive artifact storage from the primary Git checkout's
outer workspace using Git common-directory metadata. Registered linked worktrees
and checkout symlink aliases share that artifact tree and the fixed
`unified-app-bench-live-budget.json` ledger; source paths remain checkout-local.
Discovery is read-only and fails before artifact creation for non-Git, bare,
separate-Git-directory or unsupported layouts. There is no fallback ledger or
automatic ledger migration. Continuity across independent repository copies or
relocation of the primary checkout is not guaranteed. Artifact/ledger symlinks
remain refused.

For the shared cross-platform **typed or synthetic-voice entry**, start with
`make benchmark PLATFORM=desktop BENCHMARK_FLAGS="--help"` and
`tools/benchmark/README.md`. Its scenario/validation/report layer is shared with
the Android driver. This document describes the existing isolated desktop
qualification and voice workflows; their safety and live-call gates remain
separate and are not bypassed by the shared command. Adapter unit tests alone do
not establish desktop runtime acceptance. Native Windows arithmetic is a separate
offline diagnostic, not Linux-equivalent qualification or Windows certification.

One controller exercises the optimized desktop app, normal composer admission,
Custom Responses streaming and MCP tools, canonical settlement, correlated UI
completion, context and cancellation. The `typed` workflow profile excludes
capture; the separate `voice` entry additionally exercises synthetic raw capture
and real wake detection/VAD/Parakeet. Neither legacy profile certifies physical audio,
phone/Wear, Windows or acoustic output. The new `voice-arithmetic` profile below
adds real local reply synthesis/backend output handling; it does not change those
legacy profiles or their qualification evidence.

## Coverage and current status

The opt-in Rust `app-benchmark` feature replaces only hardware input. It feeds the
existing bounded capture worker before downmixing/resampling/framing. CPAL device
negotiation, sample-format conversion, OS microphone permission, acoustic input,
Windows, Android and Wear are **not** established by the legacy Linux software-input
qualification. The Windows arithmetic diagnostic below has its own narrower evidence.
Capture emits continuous silence between real-time fixture clips. A returned
start acknowledgement is not a received sample or a completed utterance.

`--entry typed` is the primary workflow measurement profile. It omits speech
assets; ordinary application capability gating therefore does not start capture.
No readiness is forced, no native history is injected and no direct submit IPC is
used. Synthetic questions go through the actual composer and its normal browser
form admission.

Typed reports distinguish `workflow_completed`, individual `checks`,
`all_checks_passed` and `qualification_eligible`. Canonical answer/source integrity,
model/effort, context, safe admission/cancellation and resource retirement remain
required. The typed qualification profile's displayed-source and history-relaunch
deferrals are explicit flags, not claims about the current implementation. Inspect
the report's individual results; preferences and absence of regeneration on
relaunch are independently checked.

Voice qualification additionally requires measured native frame admission and
processing, explicit zero outbound PCM events, zero legacy frame/silence/activity/
clip IPC calls, and zero native overflow/stall/capture/processing failures. The
capture callback feeds the native voice owner; the renderer observes bounded
`desktop-runtime-changed` invalidations through `read_desktop_runtime`, not PCM
callbacks. No frontend audio-callback hook is needed. Typed measurements do not
certify voice. There is no transcript injection, wake bypass or threshold retuning.
These are acceptance requirements, not a claim that an optimized runtime run has
passed on the current source.

## Prerequisites

- Existing Linux app build prerequisites. Only the voice profile additionally
  requires local wake/VAD/Parakeet assets and `espeak-ng`/`ffmpeg` fixtures.
- `bwrap` with a private network namespace and working loopback.
- `dbus-run-session` and `gdbus` for an owned GUI session, plus the existing
  accessibility runtime. The controller creates a private runtime directory and
  preserves the display-socket reference; it does not reuse a writable WSLg
  runtime directory as the application session directory.
- A private tools directory containing `bin/tauri-driver` (tested: 2.1.0),
  `webkit/extracted/usr/bin/WebKitWebDriver` matching the installed WebKitGTK
  (tested: 2.52.6), and a Python environment with `requirements.txt` installed.
- Real display/WebKit access. A browser preview or mocked Tauri IPC is not a
  replacement when the real app cannot launch.

Tool installation is explicit and never performed by the runner. No physical
microphone, device, emulator, model download or shared-host configuration change
is needed for the software-input lane.

## Build and offline execution (legacy typed/voice profiles)

Build the benchmark-only app using existing pinned/cached native prerequisites:

```sh
# If the offline build needs ORT, use only an existing approved cached library
# directory: export ORT_LIB_PATH="$EXISTING_ORT_LIB_DIR". If missing, stop rather
# than downloading/installing native prerequisites or a toolchain.
(cd apps/desktop-tauri/src-tauri && \
  CARGO_NET_OFFLINE=true VITE_VOXGOLEM_BENCHMARK=1 cargo tauri build --no-bundle \
  --features app-benchmark \
  --config '{"identifier":"dev.gitblame.voxgolem.benchmark"}' -- --locked)

"$BENCH_TOOLS/venv/bin/python" tools/app-bench/runner.py \
  --binary target/release/vox-golem --tools "$BENCH_TOOLS" \
  --output "$EXTERNAL_BENCH_OUTPUT" --config "$SOURCE_CONFIG" \
  --entry typed --extended
```

The output directory must be new and outside the source checkout. The controller
rejects binaries without the benchmark input capability, then re-executes in an
owned process namespace and private D-Bus session. Offline runs additionally have
loopback-only networking and read-only host files. Private HOME/XDG storage and a
separate app identifier are used. Graphics devices and private shared memory are
available without exposing physical audio input devices. Offline runs write
synthetic credentials; typed runs do not load speech assets or generate audio.
The optional `--wake-diagnostics` is for fixture qualification and changes
instrumentation conditions; do not silently mix those times with final runs.

Fake LLM and MCP services run on owned numeric-loopback addresses. The real HTTP,
SSE, MCP handshake, schema validation and shared tool runner remain in use. The
fake checks critical terms in the actual admitted prompt and checks the selected
model/effort. Offline acceptance also compares the complete fixture answer with
canonical and correlated UI text and checks preservation of the required source
sets. Only booleans/counts leave these comparisons. Synthetic sources do not prove
real-provider quality or latency.

Typed offline streaming uses a bounded gate: the first SSE text delta is flushed,
and the finite remainder is released after actual correlated in-flight DOM text
is observed. A maximum eight-second wait fails the streaming check. Gate duration
is explicit fixture delay, not real-provider computation. Mutation observations
measure DOM submission, not painted pixels. `--extended` adds real UI-seeded warm
and longer canonical histories, duplicate/empty submission, Stop/cancellation,
retirement and relaunch checks. It is prohibited for live execution.

## Shared arithmetic voice: Linux offline and native Windows diagnostic

Use `tools/benchmark/benchmark.py --platform desktop --workload synthetic-voice`
as the common entry; complete variable-based recipes are in
`tools/benchmark/README.md`. It delegates to this runner rather than implementing
another audio/UI lifecycle. No runtime acceptance is asserted by these recipes.
Record actual platform disposition separately in the outer benchmark history.

`fixtures.arithmetic_profile()` loads the authoritative spoken metadata and exact
answer from `tools/benchmark/voice_contract.py` / `scenarios.json`; there is no
second arithmetic question catalog. The real synthetic capture path still traverses
wake detection, VAD and Parakeet, ordinary native voice admission, streaming model
response, correlated canonical/UI settlement, Piper synthesis and native output.
No transcript injection, wake bypass or direct engine submission substitutes for it.

Arithmetic reports include `profile_identity.implementation_sha256` for the actual
imported shared matcher, normalizer and production scenario-parser implementation.
It is separate from canonical contract-data, fixture, native binary/source and
runner harness identities. Linux reports and Windows annotations/early failure
reports carry the same field. Legacy harness/live-qualification rules are unchanged;
older reports without this field do not identify those imported implementation bytes.

### Linux: private ALSA null output

Use the optimized `app-benchmark` build above and already approved local tools and
assets. Supply the **actual configured** wake phrase and explicit Piper ONNX model;
do not assume the generator's legacy default phrase matches the wake model. The
adjacent `<model>.onnx.json` is also hashed. The current profile configures 22050 Hz:
verify that the actual model JSON declares that sample rate. Do not tune the model,
wake threshold or silence policy merely to force a pass. Record CPU/CUDA provider
selection and fallback as observed; a missing CUDA library is not permission to
install GPU packages or change the host.

The approved `alsa-null-software` route uses a private ALSA null configuration,
real Piper synthesis and rodio/backend handling. The namespace does not expose
`/dev/snd`; host Pulse/PipeWire sockets are masked and inherited audio overrides
are removed. Do not bind host Pulse/PipeWire/JACK transports or substitute speaker
output. Null completion is software evidence, not acoustic output, speaker timing
or a security certification. Source configuration supplies existing speech asset
locations; ordinary config/secrets must not be exported into reports.

For the repository's supported Hey LiveKit test profile, pair the explicit
`--wake-phrase "hey livekit"` with
`apps/desktop-tauri/src-tauri/tests/fixtures/hey_livekit.onnx` (SHA256
`e773490192400f19a9840acb909c2afc43f99f08ca6ae43552e582deea2cb560`).
This is the LiveKit Rust SDK test classifier; it is not interchangeable with the
hello-wakeword example classifier. The framed production-model test uses `0.68`.
Select this profile through a private speech-only source configuration, not by
changing ordinary application preferences. The runner does not infer phrase/model
compatibility: record both identities and generate a new matching fixture rather
than relabeling PCM produced for another wake phrase. A test operating point is
not accuracy, false-activation or full-app acceptance evidence.

For focused direct qualification through the same authoritative child (the shared
entry is preferred for cross-platform orchestration):

```sh
"$BENCH_TOOLS/venv/bin/python" tools/app-bench/runner.py \
  --binary "$BINARY" --tools "$BENCH_TOOLS" --config "$SOURCE_CONFIG" \
  --output "$FRESH_RUN" --entry voice-arithmetic \
  --tts-model "$TTS_MODEL" --wake-phrase "$SYNTHETIC_WAKE_PHRASE"
```

The output must be fresh and outside Git. This one-case profile uses zero live
provider admissions and one fake-model admission, not the legacy three-case matrix.
Native synthesis-valid, submitted, completed, failed and retired counters are
checked separately; idle alone is insufficient. Schema 3 reports
`offline_profile_qualified` and always `qualified_for_live=false`. Child exit 0
means this offline profile qualified. The shared wrapper intentionally returns
overall **unverified / exit 2**, not physical/acoustic acceptance. Current Linux
child JSON lacks top-level `passed`/`workflow_completed`; its stdout `passed` and
qualification/cleanup fields must not be confused with missing JSON evidence.

Rejected arithmetic requests may include
`rows[].service_metadata.rejection_diagnostics`: fixed shape enums, bounded
message/part/token counts, a zero-based first mismatch index and a categorical
comparison result. No rejected text, tokens or token hashes are retained.
`first_mismatch_index=-1` means normalized text matches; `-2` means unavailable.
`analysis=bounded` means counts may be partial/capped and `mismatch=unknown`;
it is not a full-request comparison. `mismatch=match` concerns only normalized
prompt text, not model/history/other contract validation. These fields do not
relax the shared oracle or identify the rejected words, and older reports cannot
be retroactively diagnosed from their match boolean.

### Windows: stage on Linux, execute with native private Python

Build with existing Linux cross-build prerequisites, without a Windows CI runner:

```sh
make pc-benchmark
```

This target uses `cargo-xwin`, `app-benchmark`, the frontend benchmark flag and
private identifier `dev.gitblame.voxgolem.benchmark`; output is isolated under
`target/app-benchmark-windows/<Windows-target>/release/vox-golem.exe`. Stage a copy
outside Git. Do not run `pc-dist` over a staged benchmark EXE or replace it with an
ordinary application binary. Preserve the actual builder/source record alongside
the EXE identity; a declared source hash alone cannot prove how an EXE was built.

Windows needs an explicitly staged checkout/controller and complete local bundle:
matching DLLs, all model/config/espeak assets, the arithmetic PCM generated by
`generate_fixtures(..., profile=arithmetic_profile())`, and the code/harness used to
interpret them. The schema-1 manifest must bind the contract, private application
ID, `native_source_sha256`, per-file SHA-256 values and exactly these ten roles:
`binary`, `tauri_driver`, `native_driver`, `config`, `fixture`,
`wake_word_model_path`, `silero_vad_model`, `parakeet_model_dir`, `tts_model`,
`espeak_data_dir`. Include adjacent Piper JSON, all speech-directory files and
every file beside app/driver executables; undeclared load-affecting files fail.
Use a synthetic speech-only source config, not a copied credential-bearing config.
Keep the private Python venv outside the app/driver directories.

The existing validator pins Tauri driver 2.1.0 and requires declared EdgeDriver and
WebView versions to agree in their first three version components. Actual file
hashes, native-driver identity and runtime compatibility still require explicit
evidence. Do not silently install global tools, change PATH/registry/firewall, or
claim private Windows storage/Job isolation equals Linux network namespaces.
The manifest is bounded to 256 KiB and at most 4096 file entries. Validate the
staged manifest through production code, not a copied check list:

```powershell
& $PRIVATE_PYTHON -c "import sys; sys.path.insert(0, 'tools/app-bench'); from pathlib import Path; from fixtures import arithmetic_profile; from windows_effects import load_manifest; load_manifest(Path(sys.argv[1]), arithmetic_profile())" $WINDOWS_MANIFEST
```

Run on **native Windows** using the already approved private venv with Selenium,
from the staged checkout. The shared entry requires `--max-live-calls 0`,
`--desktop-platform windows` and `--windows-manifest`; its `--tools` argument is
the driver bundle, not the Python venv. The equivalent direct child command is:

```powershell
& $PRIVATE_PYTHON tools/app-bench/runner.py --desktop-platform windows `
  --windows-manifest $WINDOWS_MANIFEST --entry voice-arithmetic `
  --binary $BINARY --tools $WINDOWS_DRIVERS --config $SOURCE_CONFIG `
  --tts-model $TTS_MODEL --wake-phrase $SYNTHETIC_WAKE_PHRASE --output $FRESH_RUN
```

Its parent artifact directory must already exist. The child owns private DACL
verification, fresh app/WebView storage, executable creation identity and a
kill-on-close Windows Job. No Unix chmod substitute or WSL bridge establishes
those properties. It preserves private profiles for inspection; forced Job
retirement is distinct from graceful app exit and does not certify every resource.

Windows failure reports include `session_creation_cause`, a conservative fixed
category derived in memory from explicit known session-creation driver statements:
`version_mismatch`, `unsupported_capability`, `application_launch_failed`,
`webview_attach_timeout`, or `unknown`. Missing, oversized, ambiguous or unrecognized
messages remain unknown; raw messages/paths/endpoints are not retained. A category
describes a driver statement, not independent proof of its underlying cause.
Older failures that retained only an exception class cannot be retroactively
classified.

Schema 4 remains `diagnostic_only=true`, `passed=false`,
`offline_profile_qualified=false`, `qualified_for_live=false`,
`overall_status=unverified`, with `workflow_completed` and cleanup reported
separately. **Direct child exit 1 is expected even when that diagnostic completes.**
The shared wrapper maps only a valid completed diagnostic with owned cleanup to
unverified / exit 2; it does not discard the child report or promote it to Linux
qualification. `wasapi-render-diagnostic` is not audible/speaker acceptance;
network containment, hardware isolation and breakaway-process coverage remain
explicitly unverified. No live run or security certification follows from this report.

### Budgets, evidence and cleanup

Both arithmetic profiles reject live operation in CLI/environment/native admission
guards. They do not spend, reset or qualify the legacy fixed-12 ledger. No automatic
retry, model sweep, fixture retuning or extra trial follows failure. Real runtime
verification must follow the separately authorized Android → Linux → Windows order.

Retain binary/harness/contract, actual PCM, wake/STT assets, TTS ONNX/JSON and actual
provider/route identities with build, repetition and instrumentation conditions.
Do not export raw audio, transcripts, prompts, answers, tokens, endpoints, secrets
or provider bodies. Fixture generation/setup, native monotonic intervals and
WebView/host observations are separate clock/input domains. Never subtract
unrelated clocks or compare null-output, WASAPI diagnostic, Wear and phone timings
as matched acoustic or cold-start baselines. Fake tests do not prove physical
acceptance; TTS callbacks and DOM observation do not prove sound or painted pixels.

The child owns marked metadata retention: at most 90 days, 128 files and 8 MiB per
collection. The common wrapper refuses a child report over 128 KiB; do not infer
that this is a per-file limit enforced by the child's `private_json` writer.
Generated fixture bundles are separately capped at 16 MiB/run. Metadata pruning
does not erase private profiles, fixtures, credentials or ledgers; keep those
artifacts outside Git and arrange their separately authorized lifecycle rather
than assuming report pruning bounds all run-directory storage.

## Offline native-observer hold diagnostic (legacy voice)

Add `--entry voice --cases quick --hold-native-observer --wake-phrase "$SYNTHETIC_WAKE_PHRASE"`
to the ordinary offline invocation (with the same binary/tools/config/output arguments).
This lane is deliberately **diagnostic-only**: `passed` stays false, live
qualification stays false, and the controller exits **1 even on diagnostic success**.
It rejects live, typed, multiple/non-quick cases and combinations with other probes.

After initial capture readiness and the initial application snapshot, the runner
temporarily wraps `window.__TAURI_INTERNALS__.invoke`. Only the first
`read_desktop_runtime` invocation is withheld **before** calling native code, so
it cannot acknowledge a runtime read. At most one deferred closure is retained;
a second attempted read rejects and fails the diagnostic. Other commands retain
their original receiver and arguments, including `benchmark_queue` and
`get_app_snapshot`. The synthetic clip still traverses real capture, wake/VAD,
Parakeet and normal native prompt admission; no transcript/prompt is injected.

After the queue completion receipt, independent canonical snapshots and fake-service
terminal/count metadata are polled for at most ten seconds, also bounded by the
normal flow deadline. `native_observer_hold` records booleans/counts only: an
actual held read, canonical completion with no projected user/pending row, exact
generation count, restored invocation, matching canonical/UI reconciliation once,
normal native flow/audio ownership and a bounded half-second no-extra-admission
observation. The original invocation is restored and the held read executed through
it; the frontend uses its real snapshot/delta protocol to catch up. A `finally`
also restores/releases on failure before owned application shutdown.

Report-level `diagnostic_success` additionally requires observed application exit
before process-group termination. Full in-flight streaming presentation is not
observed while withholding through completion, so normal `accepted_flow` and
voice qualification requirements are not relaxed. This simulates a stalled
observer, **not** unresponsive renderer JavaScript, physical capture, painted
pixels or acoustic output. TTS remains disabled in this synthetic lane; absence
of acoustic replay is not established. Unit/mock-driver and executed JavaScript
wrapper tests do not replace the optimized target-app diagnostic run.

## Live safety (legacy profiles only)

Live execution is disabled by default and reserved for the final minimum flows
after offline acceptance and independent review. `--live` additionally requires a
matching successful `--offline-evidence` report for the same binary and all three
workflow cases, matching profile/binary/harness/prompt identities, the offline
context/safety matrix, no diagnostic probes
and observed app exit before process-group termination. Process disappearance is
not a successful exit status or proof of every cleanup callback.
Do not use live requests to debug audio, UI or fake protocols.

The initial live batch reserves at most 12 generation attempts against the fixed
assignment ledger outside disposable run data; the total ceiling is 100, not a
target. Each continuation and failed/uncertain dispatch consumes a permit. Rust
guards both generation HTTP send paths, disables benchmark retries and permits
only Custom Luna Low for live mode. A reservation is atomically claimed once and
never reused after a crash. No automatic repetitions or allowance expansion occur.
Live runs pass the explicit canonical absolute source config path to the
benchmark-only native adapter (`VOXGOLEM_BENCH_AUTH_SOURCE_CONFIG`). The config
must be a regular file outside the private run; ambiguous/symlink selections and
missing or unsafe managed auth files fail startup validation. Offline mode rejects
this override and keeps synthetic auth private. Shipping builds do not consume it.
Native capability, request and authentication paths select the original canonical
managed store through the existing auth owner and OS lock, including refresh and
revision fencing. Sign-out, replacement or removal never falls back to isolated
or custom credentials. Python handles config metadata, not credential bytes:
there is no credential staging, copyback, import or rotation bookkeeping.
Config, history and HOME/XDG state remain directed into the private run. The live
bubblewrap wrapper binds the host root writable: this is not sandboxed filesystem
isolation. The v2 canonical-managed-auth capability rejects old benchmark binaries;
same-binary offline qualification and all existing budget/namespace/session gates
still apply. No credential data belongs in fixture/report artifacts.

The typed qualification profile explicitly defers capture, displayed source-list
projection and desktop history restoration. Those failed/untested checks remain
visible; they never become a full voice or all-checks-passed claim. Only the three
approved main cases run once in order during the final live batch. Voice reports
and legacy typed controls cannot qualify a typed live batch, or vice versa.

## Evidence

Reports contain fixed stages, ephemeral flow/round ordinals, bounded numerical
metrics, source counts and artifact hashes, not audio, transcripts, answers,
credentials, URLs or raw provider errors. Native source timestamps are raw queue
admission receipts, not frontend consumption or acoustic output. Runner UI timing
uses one WebView monotonic submit-to-DOM clock; controller observations include
WebDriver delay. Native admission/dispatch/tool/run/commit/result-ready intervals
use a separate common native clock. `canonical_turn_committed` means an in-memory
desktop canonical commit, not durable disk storage. `command_result_ready` is
before IPC return, not actual terminal delivery. Raw timestamps from different clock domains are never
subtracted; network/provider residuals are not labelled pure model computation.
Marked metadata reports have a 90-day maximum age and an 8 MiB/128-file budget per
run collection. Cleanup targets only the controller's marked report/diagnostic
files, never the assignment ledger, code snapshots, credentials or synthetic
fixtures. Frozen synthetic fixture bundles are limited to 16 MiB per run.

Voice rows retain `native_audio` and `handler_call_metadata`. Native frame/sample
admission and processed-frame counters are process-lifetime atomics differenced
at each flow's reset; continuous background silence may leave admission and
processing at different counts. Failure counters are also per-flow differences.
`native_audio.pcm_events_emitted` counts the instrumented outbound PCM route, and
legacy handlers have explicit zero-count entries even when never invoked.
Missing fields fail qualification rather than meaning zero. Keep source inspection
of PCM emitters alongside this instrumentation; a counter cannot detect an
un-instrumented new emitter. `dropped` counts trace-observation overflow, **not**
audio-frame loss, and cannot substitute for native overflow/stall counters.

The standard voice matrix does not prove native completion while a renderer is
stalled/detached or duplicate-free reload/reattachment. Focus loss is not renderer
suspension. Those paths require separate authorized runtime evidence of native
canonical completion during the hold and UI reconciliation afterward, including
generation/turn/speech duplication checks. No driver busy-loop or fragile invoke
interception is installed here. The existing longer research question exercises a
longer utterance, but does not establish stress-length capture or recovery limits.

`--entry typed-control` is a single-case offline control using the actual composer
and shared generation path. It never qualifies as voice acceptance. Optional
event-loop and driver-free controls likewise remain diagnostic-only; they isolate
observer/runtime effects rather than substituting a passing path for voice.

```sh
python3 -m unittest discover -s tools/app-bench -p 'test_*.py'
cargo test --offline --locked -p voxgolem-audio --features benchmark-input
cargo test --offline --locked -p voxgolem-platform --lib \
  --features app-benchmark app_benchmark::
```

Normal shipping builds omit the benchmark Rust features and frontend flag. They
must not expose synthetic input commands or depend on these tools.

### Windows typed speech with an existing Neural service

`--entry typed-speech --desktop-platform windows` is a separate offline diagnostic.
It runs exactly one `fixtures.arithmetic_profile()` prompt through the normal UI
composer and the existing typed observer, with one fake-model admission and zero
live model calls. It does not load wake/VAD/ASR/PCM/Piper assets. Legacy `typed`,
`voice-arithmetic`, schema-1 staging and live qualification/budget rules are unchanged.

Use native Windows private Python with existing Selenium and the existing Windows
Job/private-DACL/fresh-profile/retained application creation-identity protections.
The schema-2 staging manifest must have `entry="typed-speech-windows"`, the shared
`contract_sha256`, benchmark `application_id`, `native_source_sha256`,
`driver_versions`, and `files` SHA-256 map. Its `roles` keys are exactly `binary`,
`tauri_driver`, `native_driver`, `config`; paths are bundle-relative. The existing
256 KiB/4096-file bounds, PE verification, pinned Tauri driver 2.1.0, matching
Edge/WebView version checks, reparse refusal and declared executable-directory
siblings (including DLLs) apply. No PCM metadata or speech asset roles are needed.
Keep the manifest outside the executable/driver directories.

The Windows adapter also passes `webviewOptions.userDataFolder` through Tauri to
EdgeDriver, using the same private `run/webview` folder as the environment and
native storage guard. Without this capability, EdgeDriver chooses a temporary
folder and the exact-folder guard rejects startup. See Microsoft's
[WebView options](https://learn.microsoft.com/en-us/microsoft-edge/webdriver/capabilities-edge-options#webviewoptions-object).
The benchmark binary writes a bounded categorical `startup-validation.json`
receipt when environment validation runs. `ready` confirms that boundary only;
a missing receipt does not identify a failure cause. Shipping builds omit it.

The hash-bound synthetic source config contains only this table (replace the
synthetic voice with an explicitly selected public voice ID in the descriptor):

```toml
[tts]
provider = "neural"
endpoint = "http://127.0.0.1:19765"
voice = "synthetic-a"
start_command = []
```

Only literal loopback HTTP with an explicit port is accepted; no credentials,
queries, paths, hostnames or launch command. At runtime the harness makes one
bounded, no-proxy/no-redirect GET to `/v1/synthesis/capabilities` on the already
running service and verifies readiness, protocol, voice, format and catalog bounds.
It does not launch, reconfigure or stop the service. App-owned synthesis sessions
remain the native client's responsibility. External shared-service lifecycle is
reported `unverified`, separately from local speech retirement and owned Job cleanup.

```powershell
& $PRIVATE_PYTHON tools/app-bench/runner.py `
  --binary $BENCHMARK_EXE --tools $PRIVATE_DRIVERS --config $SYNTHETIC_CONFIG `
  --windows-manifest $WINDOWS_MANIFEST --desktop-platform windows `
  --entry typed-speech --output $FRESH_RUN
```

Do not supply `--wake-phrase`, `--tts-model`, `--live`, extended controls or probes.
The runner sets native `VOXGOLEM_BENCH_ENTRY=typed-speech-windows`, offline mode,
TTS enabled and Web disabled in private storage. The native benchmark must accept
that entry without capture assets and expose `snapshot.speech_identity` with
`engine="neural"`, `backend`, canonical `voice`, `sample_rate_hz`,
`incremental_audio`, `protocol_version=1`. This must equal the descriptor identity.
The flow uses existing admission/commit stages and `reply_speech` counters/retired
proof, plus `first_model_text`, `first_model_word`, `reply_synthesis_requested`,
`reply_synthesis_valid`, `reply_output_submitted`, `reply_output_activated` and
`reply_output_completed`. Text completion does not replace local speech retirement.

The schema-4 report preserves `ui_submit_to_first_text_dom_ms` and adds
`ui_submit_to_first_complete_word_dom_ms`: first nonblank word followed by
whitespace, or the first nonblank word at terminal outcome (including `210`).
`ui_submit_to_speech_observation_ms` uses the same WebView `performance.now()`
observer at native snapshot payload receipt after observing output activation. It
includes IPC/poll observer overhead and is not exact audio start. Native
`native_breakdown` intervals use only native `elapsed_us`: prompt command to first
model text/word, valid PCM, submitted output and activated output; synthesis request
to valid PCM. Missing, duplicate or reordered interval boundaries produce `null`.
DOM and native raw timestamps are never subtracted. All metrics remain in reports.

Acoustics remain unverified. Even a completed diagnostic has `passed=false`,
`diagnostic_only=true`, `qualified_for_live=false`, `offline_profile_qualified=false`,
`live_calls=0`, and runner exit 1. `workflow_completed` separately requires the
synthetic typed/speech checks, matching identity, private storage, retained app
identity, graceful app exit and owned Job retirement. This report cannot authorize
live measurement or establish Windows acoustic acceptance.
