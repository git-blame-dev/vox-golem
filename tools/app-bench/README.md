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

For the shared cross-platform **typed question suite**, start with
`make benchmark PLATFORM=desktop BENCHMARK_FLAGS="--help"` and
`tools/benchmark/README.md`. Its scenario/validation/report layer is shared with
the Android driver. This document describes the existing isolated desktop
qualification and voice workflows; their safety and live-call gates remain
separate and are not bypassed by the shared command. Adapter unit tests alone do
not establish desktop runtime acceptance or Windows support.

One controller exercises the optimized desktop app, normal composer admission,
Custom Responses streaming and MCP tools, canonical settlement, correlated UI
completion, context and cancellation. The `typed` workflow profile excludes
capture; the separate `voice` entry additionally exercises synthetic raw capture
and real wake detection/VAD/Parakeet. Neither profile certifies physical audio,
phone/Wear, Windows or acoustic output.

## Coverage and current status

The opt-in Rust `app-benchmark` feature replaces only hardware input. It feeds the
existing bounded capture worker before downmixing/resampling/framing. CPAL device
negotiation, sample-format conversion, OS microphone permission, acoustic input,
Windows, Android and Wear are **not** established by this software-input run.
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

## Build and offline execution

Build the benchmark-only app using existing pinned/cached native prerequisites:

```sh
VITE_VOXGOLEM_BENCHMARK=1 cargo tauri build --no-bundle \
  --features app-benchmark \
  --config '{"identifier":"dev.gitblame.voxgolem.benchmark"}' -- --locked

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

## Offline native-observer hold diagnostic

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

## Live safety

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
