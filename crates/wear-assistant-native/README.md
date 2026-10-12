# Shared mobile Assistant JNI bridge

Despite its historical crate name, `voxgolem-wear-assistant-native` now supplies
`libwear_assistant_native.so` to **both phone and Wear** through
[assistant-android](../assistant-android/README.md). It binds the portable Rust
conversation domain and the existing Custom Responses client; it is no longer an
unwired Wear-only feasibility stub. The phone's Tauri bridge and native Wear
Activity both use the shared `AssistantRuntime`/engine/store.

This crate has no Android UI, Tauri shell, database, ASR/TTS, credential-import UI
or parallel provider implementation. Core owns immutable operation/snapshot rules;
Kotlin owns app-private SQLite transactions, platform IO and native lifecycle.
Source integration is not proof of physical-device or complete-feature acceptance.

## Two separate native entry points

| Android owner | Native calls | Boundary |
| --- | --- | --- |
| [AssistantDomainNative](../assistant-android/src/main/java/dev/gitblame/voxgolem/assistant/AssistantDomain.kt) | `apply(request: String): String` | Pure bounded core evaluation; no provider execution, credentials or cached history |
| [AssistantProviderNative](../assistant-android/src/main/java/dev/gitblame/voxgolem/assistant/runtime/AssistantProviderNative.kt) | `respond(requestId, authPath, requestJson, callback)` and `cancel(requestId)` | Worker-only Custom request and cancellation; does not commit history |

[lib.rs](src/lib.rs) exports the shared classes' JNI symbols and the retained
[WearAssistantNative](../../apps/wear/app/src/main/java/dev/gitblame/voxgolem/android/wear/assistant/WearAssistantNative.kt)
compatibility entry points. Both domain entry points call the same `apply_jni`;
both provider entry points call the same `respond_jni`/`cancel_jni`. Production
history/provider ownership is in the shared library, not a second Wear engine.
Load with `System.loadLibrary("wear_assistant_native")`. Keep native object/method
names and callback methods through R8 using the shared
[consumer rules](../assistant-android/consumer-rules.pro) and Wear's
[compatibility rules](../../apps/wear/app/assistant-proguard-rules.pro).

### Headless domain and trusted snapshots

[domain.rs](src/domain.rs) defines the wire gateway; the canonical types are in
[durable_conversation.rs](../core/src/durable_conversation.rs) and
[conversation_sync.rs](../core/src/conversation_sync.rs). Supported evaluations
cover records, views, revision-pinned plans/context, continuation choices,
inventory/reconciliation and exact receipts. Read those declarations rather than
maintaining another JSON schema or method descriptor catalogue here.

Retained record keys are recomputed as SHA-256 of compact, typed `Operation` JSON,
not the peer's raw JSON or outer record. Restore validates the snapshot and checks
its normalized export, including retired and quarantine ledgers. Older v1 inputs
without quarantine default to an empty ledger; canonical exports include it.
Retired bodies have been purged, so their keys are accepted only from the trusted
committed SQLite ledger, never a peer-supplied snapshot. Quarantined bodies are
still digest-checked and cannot form provider context.

The gateway caps input/output at 16 MiB, raw operations at 2 MiB and snapshots
(including post-import snapshots) at 4 MiB. It returns a plan/projection or bounded
error, not a database commit. [AssistantStore](../assistant-android/src/main/java/dev/gitblame/voxgolem/assistant/AssistantStore.kt)
checks the base revision and atomically commits snapshot, purge/outbox work and
receipt state before publication/ACK. A revision conflict recomputes the plan
without rerunning inference. Peers import immutable records, not snapshots or
retired ledgers. Branch selection and exact per-peer ACKs remain explicit.

### Provider worker and cancellation

[request.rs](src/request.rs) and [outcome.rs](src/outcome.rs) define bounded provider
requests/results. Trusted native code supplies core-selected ancestry and the
app-private credential path; the wrapper has no Android Context to enforce that
path's sandbox ownership. Callers cannot override authentication bytes or the
endpoint in request JSON. The existing [Custom client](../platform/src/custom_openai.rs)
owns model/provider behavior.

`respond` blocks its caller and invokes `ProviderDeltaCallback.onReady()` and
`onDelta(String)` synchronously on that attached worker thread. Callbacks must
return promptly and marshal UI work themselves. `onReady` runs after cancellation
registration but before credential/network IO; cancellation can therefore be
replayed if Stop raced worker startup. The explicit request ID must match the
request projection. Callback failure stops the request and yields a bounded error.
If JVM result allocation fails, the native result may be null/pending an exception;
an allocated JSON response is not guaranteed under OOM.

[AssistantRequestWorker](../assistant-android/src/main/java/dev/gitblame/voxgolem/assistant/runtime/AssistantRequestWorker.kt)
admits one request through retirement, suppresses late callbacks and bounds
aggregate delta text. The engine owns context and durable completion. Provisional
deltas and a provider `complete` result are not saved/synced history: successful
delivery gating, domain validation and SQLite commit are still required.

`cancel` is a no-op for unknown IDs; active IDs remain reserved until retirement.
It drops the provider future but cannot interrupt a blocking callback or synchronous
credential read. IDs must not be reused. The bridge reads externally provisioned
app-private access files; it does not import, copy, refresh or manage access.
Unavailable, expired or rejected access leaves the draft for deliberate retry.
Conversation transport is separate GMS DataItem/Asset replication and may relay
encrypted traffic through Google servers; it is not a cloud-free guarantee.

## Packaging and checks

[assistant-android/build.gradle.kts](../assistant-android/build.gradle.kts) owns JNI
build/staging, not copied shell scripts. Phone settings select `arm64-v8a`
(`aarch64-linux-android`); Wear settings select `armeabi-v7a`
(`armv7-linux-androideabi`). The library uses the NDK API24 compiler baseline;
the Wear shell itself has minSdk30. Debug uses debug Rust; release and the
optimized Wear performance variant use release Rust. Native outputs/staging live
under each Gradle root's `build/assistant-android/`, not `app/build/rust-native`.
JNI Cargo tasks use cached dependencies with `--offline --locked`.

Run from the source checkout on Linux:

```bash
cargo fmt -p voxgolem-wear-assistant-native --check
cargo clippy --offline --locked -p voxgolem-wear-assistant-native --all-targets -- -D warnings
cargo test --offline --locked -p voxgolem-wear-assistant-native
make android-assistant-test
make wear-test
make android-build
make wear-build
make wear-performance
```

Mobile targets require JDK21 (`JAVA_HOME` and its `bin` on `PATH`), SDK36
(`ANDROID_HOME`), NDK28.2.13676358 (`NDK_HOME`) and the corresponding Rust target.
Phone targets additionally require Tauri CLI2.11.1. Each Gradle shell supplies
`assistantNativeAbis`; do not configure the library without that property.
Shared-library and Wear JNI unit tests automatically depend on
`buildAssistantNativeJvm` and load the host `.so`. `assistantNativeLibrary` can
select the shared tests' library path; it does not remove the native build dependency.
The library's host build needs no Android cross-compiler, but Gradle still needs
the shell's SDK/configuration. Pure Kotlin/JSON tests are not a substitute for JNI tests.

Tests use synthetic data and no real provider login. Linux checks do not establish
ARM32/ARM64 ART load, optimized JNI callbacks/Stop, networking, speech-provider
availability or two-device offline/reconnect acceptance. The phone plugin now enables
shared local speech admission on load, and its
[single-turn voice shortcut](../assistant-plugin/README.md#new-voice-chat-shortcut-implemented-source)
is implemented in source, not a deferred stub. Installed recognition support,
launcher confirmation and physical callbacks remain unverified; the final mobile
artifact for the latest source is still pending. Older installed/archived capture
APKs are not this source's acceptance evidence. The distinct watch-to-phone
supplied-audio lane has production runtime constructors but retains a default-false
provider/device calibration gate, without an advertised activated UI flow.
Bundled watch-local transcription remains future backlog.
