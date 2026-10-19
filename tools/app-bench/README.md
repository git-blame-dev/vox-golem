# Unified application benchmark

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

Each report distinguishes `workflow_completed`, individual `checks`,
`all_checks_passed` and `qualification_eligible`. Canonical answer/source integrity,
model/effort, context, safe admission/cancellation and resource retirement remain
required. A failed displayed-source-list check remains visible without hiding the
completed provider/canonical workflow. Active source accumulation is now fixed;
canonical source metadata/cold hydration is still separate, unimplemented scope.
Desktop history lost
across process relaunch is likewise reported, not repaired by a second store.
Preferences and absence of regeneration on relaunch are independently checked.

The voice path currently encounters renderer/frame-backlog failures after real
wake/VAD/ASR. It is deliberately deferred, not certified by typed measurements.
There is no transcript injection, wake bypass or threshold retuning.

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
Live credentials are not copied into fixture/report artifacts; ordinary authority
and revision fences must remain intact.

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
