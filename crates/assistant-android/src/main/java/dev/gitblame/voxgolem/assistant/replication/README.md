# Android / Wear conversation replication integration

This package transports completed immutable operations only. Rust validates records and
reconciles committed SQLite snapshots; snapshots, retired ledgers, drafts, running requests,
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

## Application and lifecycle hooks

- [AssistantApplication](AssistantApplication.kt) owns native peer setup and calls
  `AssistantReplication.start` before framework services need it. It does not configure
  credential bindings, call `AssistantRuntime.get`, create a provider engine or start generation.
  A custom shell Application must preserve those shared owners and startup hooks.
- Startup, foreground resume, SDK identity/capability/peer events and bounded jobs drive
  setup and reconciliation. Call `AssistantReplication.reconcileNow(context)` on resume
  and after settled local mutations. Identity events are hints: capability and exact
  host-scoped identity are queried again rather than trusting event metadata.
- Setup and reconciliation remain native lifecycle work, not credential-settings polling.
  No sync callback imports access or starts provider/microphone work. Marshal native
  presentation callbacks to the UI thread.
- Use `AssistantRuntime::refreshIfStarted` as `onCommitted` to refresh an existing engine
  after imports. The headless listener's Store handle uses the same SQLite database and
  pure JNI domain gateway, not another in-memory conversation authority.

## Build and source ownership

The common [manifest](../../../../../../AndroidManifest.xml) owns the Application,
DataItem/capability listener and persisted replication jobs.
`RECEIVE_BOOT_COMPLETED` supports persisted JobScheduler work; no custom boot receiver or
invented `PEER_CONNECTED` manifest action is needed. Credential `/transfer` and `/ack`
Message handlers are removed; credentials never enter the conversation queue,
database, DataItems, Assets or setup metadata.

The shared library uses the already pinned Wearable 19.0.0 APIs. Each shell's effective
`android_wear_capabilities` resource array must contain `voxgolem_assistant_v1` while
preserving its capture/input entries. Shell/common Gradle integration owns ABI selection,
JNI packaging and shrink rules. Source types and the merged manifest are authoritative;
do not maintain a second wire schema or copied registration block here.

## Wire, durability and bounds

- One public metadata-only `/voxgolem/assistant/v1/identity` Hello carries v3 setup identity.
  The conversation namespace remains v1; identity version 3 does not change record
  wire versions. Old `/pair/request` and `/pair/ack` invitation slots no longer
  enroll a peer. No credential envelope or user content is staged in identity metadata.
- Eight fixed slots per enrolled peer live at
  `/voxgolem/assistant/v1/<sender-replica>/<target-replica>/<kind>`: `hint`,
  `inventory_request`, `inventory`, `record_request`, `records`, `ack`, `ack_confirmed`,
  `restart`. Envelope sender/target/session/token and SDK URI/source must agree.
- A revision-pinned inventory walk commits **all pages' tombstones first**, then walks body
  pages at that same sender revision. Revision changes restart the finite session.
- Offers originate from Store publications but filter the current quarantine ledger even
  when an old outbox index still has a payload. Rust inventory omits quarantined bodies;
  explicit body-serving and staged-item cleanup independently exclude them. Exact receipts
  for committed quarantined evidence remain valid, but that evidence cannot form context.
- Newly discovered missing ancestors are requested after imports as well as on inventory
  pages. A session attempts at most 128 distinct unknown ancestors, preventing unavailable
  or poisoned dependencies from creating a hot request loop.
- Assets carry records; the control page carries exact ID/body/conversation/tombstone and
  byte declarations. Import recomputes canonical body hashes through Rust. Store commit and
  durable receipt retrieval precede ACK; the peer commits exact ACK state before confirming.
  ACK confirmation includes reciprocal exact receipts derived from the confirming Store's
  committed snapshot; this acknowledges relayed publications without guessing that a record
  or SDK submission proves persistence, and avoids ACK ping-pong. Session metadata is durable
  before publication. Duplicate/reordered/lost frames resume
  from that metadata and committed Store outboxes, not sequence watermarks.
- Controls are at most 32 KiB/128 items. A body is at most 2 MiB; at most eight Assets and
  4 MiB aggregate are read per response. JSON rejects duplicate keys, bad UTF-8 and depth
  over 32. Polling uses exact SDK slots, not an unbounded DataItem prefix buffer.
- There are at most eight enrolled peers. Enrollment is blocked when deferred revocation
  cleanup would accumulate unbounded old slots. Revoked-peer cleanup metadata persists
  until the SDK delete succeeds.
- Automatic setup retains one counterpart binding. The protocol's eight-peer bound is a
  transport ceiling, not support for automatic multi-device setup. Replacing
  a counterpart queues old conversation-slot cleanup without deleting local history.
- SDK Tasks wait at most five seconds; a work unit has a 50-second cooperative budget.
  Asset descriptors are poll/read bounded and released on every path; late Task resources
  are released and late publications are conservatively deleted. Workers idle out, the
  queue is capped at 16, and no custom background daemon runs.
- Persisted retry jobs use 30-second to 30-minute capped exponential backoff plus jitter.
  A separate 30-minute periodic job, startup/resume and capability reachability triggers
  reconcile. OS deferral is possible; thirty minutes disconnected is not a TTL or a
  promised delivery deadline. Physical offline/reconnect acceptance remains necessary.

Target fields are routing metadata, **not selective encryption** among installations with
the same package/signature. Cleanup is best effort and cannot immediately erase an Asset
already obtained by an offline peer. No operation triggers remote generation or capture.

## Provider access and verification boundary

Conversation synchronization does not transfer credentials and does not depend on usable
provider access. Each app reads its own existing externally injected app-private access
file, without import, login, renewal or refresh implementation. Normal chat reports
unavailable/expired/rejected access and retains drafts for deliberate retry. Authorized
development file injection and its private tooling stay outside the repository; proper
device authentication is deferred, not currently available.

Use the maintained verification targets in the [shared library guide](../../../../../../../../README.md#verification).
Synthetic SDK/controller and SQLite/Rust JNI tests establish policy, ordering and recovery,
not real Google Play services reachability. Matching signed phone/Watch artifacts, durable
conversation ACKs, usable provider access, ART/JNI behavior and physical offline/reconnect acceptance
remain separate gates. This guide records no successful physical acceptance.
