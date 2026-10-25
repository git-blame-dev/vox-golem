# Android / Wear conversation replication integration

This package transports completed immutable operations only. The portable Rust conversation
store owns bundled SQLite, canonical validation and incremental commits; snapshots, retired ledgers, drafts, running requests,
provider calls and credential envelopes are never conversation payloads.

## Native paired-app setup

[AssistantPeerSetup](AssistantPeerSetup.kt) automatically enrolls the natively paired
phone/Watch apps for conversation sharing. Matching package IDs and actual
signing certificates establish Wear Data Layer isolation; public IDs are bookkeeping,
not tokens or an additional authorization ceremony. No app invitation, selection,
confirmation checkbox or manual enrollment is required.

- The metadata-only Hello is version 3 and uses native replication peer identity,
  independent of provider access or credential storage. Setup requires one compatible
  opposite-form-factor counterpart with identities distinct from the local app.
- Capability discovery uses `FILTER_ALL`, not just `FILTER_REACHABLE`: an offline second
  device must not cause the remaining reachable device to be selected arbitrarily.
  Missing Hello waits; incompatible metadata requires an update; multiple candidates,
  SDK discovery errors and storage failures have distinct diagnostic states.
- Native replication persistence owns the counterpart binding and replica identities.
  No credential association, hub, generation or copy-permission state gates setup.
  Routes re-query SDK identity and current reachability; stored node hints and
  incoming JSON never authorize a sender. Reconnect preserves peer identity and
  conversation retry/ACK state when the binding is unchanged.
- The replication database migration clears legacy setup metadata, including pending
  invitations. It does not erase conversation history, drafts, credentials, backups or
  upstream access sources. Reassociation replaces bookkeeping, not personal content.
- History epoch 2 resets conversation retries, not current trusted bindings or
  setup metadata. Its one-time conversation-database reset drops only owned history tables,
  preserving the replica UUID/counter and leaving credential/settings/capture storage alone.
  Legacy history bodies are discarded, never migrated or rehashed.

## Application and lifecycle hooks

- [AssistantApplication](AssistantApplication.kt) owns native peer setup and calls
  `AssistantReplication.start` before framework services need it. It does not configure
  credential bindings, call `AssistantRuntime.get`, create a provider engine or start generation.
  A custom shell Application must preserve those shared owners and startup hooks.
- Startup, foreground resume, SDK identity/capability/peer events, coalesced reconcile
  hints, connected session channels and bounded jobs drive setup and reconciliation. Call
  `AssistantReplication.reconcileNow(context)` on resume and after settled local mutations.
  Identity events are hints: capability and exact host-scoped identity are queried again
  rather than trusting event metadata.
- Setup and reconciliation remain native lifecycle work, not credential-settings polling.
  No sync callback imports access or starts provider/microphone work. Marshal native
  presentation callbacks to the UI thread.
- Use `AssistantRuntime::refreshIfStarted` as `onCommitted` to refresh an existing engine
  after imports. The headless listener's Store handle uses the same SQLite database and
  Rust SQLite owner, not another Kotlin database/cache authority. Ordinary admission,
  draft, completion and projection calls do not round-trip a full snapshot through JNI.

## Build and source ownership

The common [manifest](../../../../../../AndroidManifest.xml) owns the Application,
GMS listener (`DATA_CHANGED` for `/voxgolem/assistant/v1/`, `MESSAGE_RECEIVED` for
`/voxgolem/chatgpt-access/v1/` and `/voxgolem/assistant/v2/reconcile-hint`, `CHANNEL_EVENT`
for `/voxgolem/assistant/v2/phone-relay/` and `/voxgolem/assistant/v2/session/`, and
`CAPABILITY_CHANGED`) and persisted replication jobs.
`RECEIVE_BOOT_COMPLETED` supports persisted JobScheduler work; no custom boot receiver or
invented `PEER_CONNECTED` manifest action is needed. Credential `/transfer` and `/ack`
Message handlers are removed; credentials never enter the conversation queue,
database, setup metadata or conversation replication sessions.

The shared library uses the already pinned Wearable 19.0.0 APIs. Each shell's effective
`android_wear_capabilities` resource array must contain `voxgolem_assistant_v1` while
preserving its capture/input entries. Shell/common Gradle integration owns ABI selection,
JNI packaging and shrink rules. Source types and the merged manifest are authoritative;
do not maintain a second wire schema or copied registration block here.

## Transport split, session ownership, framing and canonical SQLite authority

### Transport split

- `/voxgolem/assistant/v1/identity` `DataItem` (`<= 2 KiB`, no assets) remains strictly for
  non-enrolling v3 `Hello` discovery. Old `/pair/request` and `/pair/ack` invitation slots
  and legacy `/voxgolem/assistant/v1/<sender>/<target>/<kind>` or
  `/voxgolem/assistant/v2/<sender>/<target>/<kind>` `DataItem` conversation mailbox paths are
  rejected and inert; no conversation payloads or `Asset` attachments use `DataClient`.
- `/voxgolem/assistant/v2/reconcile-hint` `MessageClient` messages (`<= 256` bytes) carry
  v3 coalesced reconcile hints (latest payload per `sourceNodeId`, capped at `MAX_PEERS = 8`,
  with at most one queued drain runnable on the replication executor).
- `/voxgolem/assistant/v2/session/<uuid>` `ChannelClient` streams carry connected
  bidirectional v2 replication sessions.
- `/voxgolem/chatgpt-access/v1/` (`ChatGptAccessSync`) and
  `/voxgolem/assistant/v2/phone-relay/` (`PhoneRelay`) remain separate and unchanged.

### Deterministic session ownership

- In `ReplicationController.reconcile`, Phone (`role == AssistantPeerRole.PHONE`) opens
  canonical `ChannelClient` sessions when the peer is reachable and paired, both during
  `reconcile()` and upon validating an incoming `/voxgolem/assistant/v2/reconcile-hint` in
  `ReplicationController.onHint`.
- In `ReplicationController.reconcile`, Watch (`role == AssistantPeerRole.WATCH`) sends
  coalesced `/voxgolem/assistant/v2/reconcile-hint` messages when Phone is paired and accepts
  incoming `ChannelClient` sessions opened by Phone via `ReplicationController.accept`.
- Within a connected session, the initiator pulls from the peer first and then serves local
  inventory/records/ACKs (`pullFromPeer` -> `serveToPeer`), while the responder serves first
  and then pulls (`serveToPeer` -> `pullFromPeer`), completing bidirectional catch-up over
  one channel.

### Connected session framing and bounds

- Session streams use 4-byte big-endian length-prefixed UTF-8 JSON control frames
  (`2..=32 KiB`, `MAX_CONTROL_BYTES = 32 KiB`, `ReplicationWire.MAX_ITEMS = 128`,
  `MAX_SESSION_PAGES = 4096`, `ReplicationWire.MAX_ITEMS = 128` `ancestorAttempts` cap in
  `ReplicationController.appendMissingAncestors`). JSON decoding rejects duplicate keys,
  malformed UTF-8 and nesting depth over 32.
- Control frames (`version = 2`) validate `sender`, `target`, `session`, `token`, and
  `kind` (`inventory_request`, `inventory`, `record_request`, `records`, `body_chunk`,
  `batch_end`, `ack`, `ack_confirmed`, `restart`, `done`) against the authenticated route.
- A revision-pinned inventory walk requests and commits **all pages' tombstones first**
  (`phase = "tombstones"`), then walks record pages (`phase = "records"`) at that same
  sender revision. Any sender revision change emits `restart` and restarts the walk from
  tombstones.
- Prepared batch streaming enforces `MAX_BATCH_RECORDS = 8`, `MAX_RECORD_BYTES = 2 MiB`,
  `MAX_BATCH_BYTES = 4 MiB`, and `MAX_CHUNK_BYTES = 64 KiB`:
  - The sender writes a `records` control frame declaring `transfer` UUID, ordered
    `{ "entry", "bytes" }` transfer descriptors (`record_begin` metadata), and `unavailable`
    IDs accounting for every requested entry, followed by `body_chunk` frames and raw UTF-8
    byte chunks (`<= 64 KiB` each), and finishes with an explicit `batch_end` frame
    verifying total record count and byte length.
  - The receiver validates transfer declarations against requested entries and phase, and
    reassembles chunks in bounded memory (`ReplicationWire.readRecordBatch`)
    verifying exact byte counts, UTF-8 JSON structure, and descriptor equality before
    passing the batch to native import.

### Native Store and canonical SQLite authority

- Native [`Store::replication`](../../../../../../../../../conversation-store/src/replication.rs)
  prepares bounded body batches (`Request::Bodies`) in a short read transaction and releases
  the store read lock before socket writes; `ReplicationController` re-verifies `revision`
  before streaming the batch over the channel.
- `Request::Import` validates batch size, byte budgets, and descriptor equality, then
  executes `import_received` to recompute canonical body hashes, commit accepted/duplicate/
  suppressed/quarantined records, and return `{ "outcome": ..., "receipts": [...] }`
  (flattened `CommitOutcome` fields plus durable `receipts`) atomically from the same
  single SQLite write transaction.
- Offers originate from Store publications while filtering the quarantine ledger; quarantined
  records remain valid exact receipt targets when committed, but are never advertised or
  served as bodies and cannot form prompt context.
- After importing a batch or reconciling known inventory entries, the receiver sends `ack`
  with exact committed `Receipt`s (`recipient_replica`, `operation`, `body_key`) and waits
  for `ack_confirmed` carrying reciprocal exact receipts derived from the peer's committed
  snapshot before recording peer acknowledgements (`store.acknowledge`).
- Canonical SQLite receipts and outboxes in `assistant.db` are the sole convergence
  authority across disconnects. `ReplicationDatabase` (`assistant-replication.db`, schema
  v5) stores only `peers(replica)`, `setup`, `native_bindings`, and `retry` — there are no
  durable `peers.session` columns or `cleanup_peers` tables.
- Canonical history including retired/quarantine ledgers is capped at 4 MiB encoded;
  drafts at 4 MiB aggregate, each draft at 32 KiB; each outgoing/receipt outbox at 8 MiB
  and 65,536 rows; peer receipts at 65,536 rows. SQLite has a 64 MiB page budget and a
  16 MiB retained WAL budget with automatic checkpoints. Explicit truncating checkpoints
  are idle maintenance, not admission/stream/cancellation work.
- Automatic setup retains one counterpart binding (`MAX_PEERS = 8` is a protocol ceiling,
  not automatic multi-device setup). SDK Tasks wait at most 5 seconds inside a 50-second
  cooperative operation budget; late `Channel` or stream results arriving after timeout are
  closed immediately. The single-thread replication executor idles out after 30 seconds with
  a queue cap of 16, backed by persisted 30-second to 30-minute exponential-backoff retry
  jobs and a 30-minute periodic job.

Target fields are routing metadata, **not selective encryption** among installations with
the same package/signature. No replication operation triggers remote generation or capture.

## Provider access and verification boundary

Conversation synchronization does not transfer credentials and does not depend on usable
provider access. Android's device-code sign-in and paired access-only sharing use a
separate transient credential channel; refresh tokens stay on the phone. Copied or imported
credential files are not read. Normal chat reports unavailable/expired/rejected access
and retains drafts for deliberate retry. See the shared library's provider-access guidance.

Use the maintained verification targets in the [shared library guide](../../../../../../../../README.md#verification).
Synthetic SDK/controller and SQLite/Rust JNI tests establish policy, ordering and recovery,
not real Google Play services reachability. Matching signed phone/Watch artifacts, durable
conversation ACKs, usable provider access, ART/JNI behavior and physical offline/reconnect acceptance
remain separate gates. This guide records no successful physical acceptance.
