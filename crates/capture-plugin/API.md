# Capture integration contract (v1)

Source package: `dev.gitblame.voxgolem.capture`. Phone uses `:tauri-plugin-capture`;
phone and Wear depend on the ordinary `:capture-android` library. SDK speech is shared
through `:voice-android`; the capture plugin does not depend on Assistant/JNI. Desktop has no
native watch-capture runtime. Wear keeps its native shell, not the phone Tauri plugin.

## Authoritative sources

- [Shared enums, records, manifests, protocol and transcript reducer](../capture-android/src/main/java/dev/gitblame/voxgolem/capture/CaptureContract.kt)
- [Canonical SQLite repository and persistence JSON](../capture-android/src/main/java/dev/gitblame/voxgolem/capture/CaptureRepository.kt)
- [PCM recording writer](../capture-android/src/main/java/dev/gitblame/voxgolem/capture/PcmRecordingWriter.kt), [WAV reader](../capture-android/src/main/java/dev/gitblame/voxgolem/capture/WavPcmReader.kt)
- [Phone commands and Activity callbacks](android/src/main/java/dev/gitblame/voxgolem/capture/CapturePlugin.kt)
- [Public projection allowlist](android/src/main/java/dev/gitblame/voxgolem/capture/CaptureProjection.kt)
- [Frontend bridge parser](../../frontend/app/src/lib/captures.ts)

These source declarations, not copied signatures or enum catalogues in this document,
define the API. UTC timestamps use ISO-8601; `finishedLocalDate` is `yyyy-MM-dd`.
Public capture-history responses are not canonical persistence JSON: they include
display duration, nullable transcript/audio/error fields and enum names, but omit
private spool paths, audio hashes, recognition session IDs and export recovery data.
No schema, database version, preference key or data migration changes accompany the
phone ownership extraction.

## Phone ownership

- [PhoneCaptureCoordinator](android/src/main/java/dev/gitblame/voxgolem/capture/PhoneCaptureCoordinator.kt)
  remains the single live/replay/probe admission, session, timeout and SDK-retirement
  owner. Recognition leases and failed-teardown retries are retained; no replacement
  SDK session is admitted while failed retirement still owns the resource. SDK
  teardown does not run under session/lease admission monitors.
- [PhoneCaptureDelivery](android/src/main/java/dev/gitblame/voxgolem/capture/PhoneCaptureDelivery.kt)
  owns wire encoding, transcript revision coalescing, separate live-message and
   receipt executors, and oversized exact-node receipt channels. Publishing follows coordinator
  persistence. Empty or unsendable hypotheses do not suppress a deliverable preview;
  final messages are not dropped by partial coalescing.
- [PhoneCaptureArchive](android/src/main/java/dev/gitblame/voxgolem/capture/PhoneCaptureArchive.kt)
  owns archive deduplication and the serial archive/export lane, using canonical
  repository rows rather than another record cache. Verified read-back is required
  before `PHONE_ARCHIVED`; export requires `SAVED`, nonempty `FINAL`, verified audio.
- [CaptureChannelReceiver](android/src/main/java/dev/gitblame/voxgolem/capture/CaptureChannelReceiver.kt)
  remains application-owned. Transient GMS listener destruction does not end its
  readers; real runtime shutdown does. Live EOF/archive reconciliation stays with
  the coordinator because it is coupled to recognition-session ownership.

## Requirements and onboarding

[NativePhoneRequirements](android/src/main/java/dev/gitblame/voxgolem/capture/PhoneCaptureRequirements.kt)
is the phone-only permission/alias/error catalogue. The plugin's annotations and
visible permission request, coordinator admission and foreground service checks
share it. `RECORD_AUDIO` is required for supplied watch audio, `BLUETOOTH_CONNECT`
from API31 and `POST_NOTIFICATIONS` from API33. Requests require an explicit action
in a resumed Activity; prior consent permits a visible-resume service restart.
Service admission still rechecks consent and permissions and fails closed.

The service is a visible `connectedDevice` foreground service, not a microphone
service. Host manifest permissions and service type remain required. The supplied
audio adapter needs API33 and a provider accepting external PCM. New Notes capture
the shared installation language/network-fallback policy at Start and carry it to
live and replay recognition; missing legacy policy defaults to dedicated local-only
admission. Disabled fallback requires the dedicated on-device route. Permitted
fallback can select an ordinary provider with provider-controlled networking;
offline preference is not an execution guarantee. Android permits unsupported
external-input providers to open their own microphone, so the app supplying Watch
PCM does not establish provider microphone isolation. Permission denial or a
background FGS start denial is not bypassed. Real input support, isolation and
accuracy require device acceptance, not a READY callback, service-start request or
host test.

`NativeProjectionStatus` tracks consent, foreground readiness, runtime failure and
missing permission separately. Public `onboardingState` is `setup-required`,
`permission-required`, `starting`, `enabled`, `failed` or `unsupported`;
`onboardingError` is nullable. `starting` means consent/permissions permit startup
but foreground readiness is not confirmed; it is neither success nor failure.
`enabled` requires readiness with no current failure or missing permission.
`setupRequired` remains true until enabled. Folder readiness is independent.
Frontend retains legacy snapshots without onboarding fields using `setupRequired`
and the existing `error` fallback. The Rust bridge still forwards snapshots and
unwraps native `{captures: [...]}` list responses into the frontend array.

## Wire and durable export guarantees

Protocol stays `voxgolem/capture/v1`: start/ready/partial/final/stop/receipt/status
messages, ordered bounded PCM frames with explicit EOF, and exact archive length
and hash validation. Receipts echo the archive hash. Transcript messages fit the
32KiB message cap; larger final receipts use a ChannelClient channel opened to the
exact originating/selected watch node at `/voxgolem/capture/v1/receipt/<captureId>`.
The channel carries a big-endian 32-bit byte length followed by the unchanged v1
UTF-8 JSON receipt and EOF. Length is limited to 256KiB. Once a lifetime reservation
is obtained, logical work is revoked after 15 seconds, including queued/acquiring IO;
a timed-out receive cannot commit a receipt. Native retirement is separately confirmed.
Wear rechecks the selected phone node before acquisition and before committing.
After consuming a valid receipt, Wear writes a one-byte `1` acknowledgement on
the reverse stream and closes that stream. The phone reads that byte before
closing the native channel. Failed synchronous/asynchronous closes retain their
precise handles and admission charge until a successful close or matching callback,
including late SDK stream acquisitions. Each recovery round makes at most three
native/stream close attempts, off the deadline scheduler. Phone admits at most ten
outstanding receipt resources. Wear admits at most ten receipt readers and manages
up to 64 reserved cleanup lifetimes including rejected incoming channels. This quota
does not bound every SDK-delivered callback handle: a handle waiting for a reservation
is outside that managed pool, and its 15-second lifetime has not begun. Saturation can
therefore leave dispatch waiting indefinitely for confirmed retirement; there is no
new application queue for these handles and no claim of a total SDK-handle bound.
Exhausted cleanup capacity backpressures dispatch rather than dropping a failed handle; explicit
capture transport retry requests another bounded recovery round. Native failure can
therefore deny/delay subsequent receipt work until retirement succeeds.
Wear also installs a process-owned SDK close observer, independent of the listener
dispatch queue, so callback confirmation can release cleanup quota during dispatch
backpressure. A GMS-delivered handle awaiting quota remains held by its callback;
no stream or transcript processing is admitted for it.
Phone tracks early close confirmations only during pending open attempts, filtering
by expected node/path and retaining at most 16 exact SDK channel identities per open.
Adoption reconciles the exact handle, never a capture path alone. Unrelated callbacks
are ignored, excess same-route candidates are not cached, and completed attempts
discard their candidates so a later replacement cannot inherit an old confirmation.
An archive hash and capture ID correlate content; neither is peer authentication.
Transport confidentiality relies on Google's paired-node ChannelClient routing,
not a recipient UUID in a globally replicated payload. No credentials are sent.
The phone atomically creates a new capture and an immutable phone-local origin
association from the actual GMS source node. The additive `phone_capture_origins`
table lives in the private capture database; it is never encoded in canonical
records or the protocol. STATUS/retry, live input, duplicate archive/header replay,
and all transcript-bearing deliveries check that association. Hash equality cannot
authorize another watch or rebind ownership. Capture IDs/hashes remain non-secret.
Live PCM also requires the capture/session/source-node tuple and the exact registered
input owner. SDK mutations retain the recognizer's exact events owner, so reused
session IDs cannot redirect stale input, finish or cancellation to a replacement.
Rejected readers are stopped with a fixed `live-input-ownership-rejected` READY status.
An authorized peer with no admitted SDK preview can still retain its local recording
and transfer the finalized archive. The reviewed collision path requires knowledge
or collision of a live session UUID (or an admission race); no discovery route was
established, and UUID secrecy is not used as the authorization control.
Denied requests receive only an additive v1 `AUTHORIZATION_FAILED` receipt with the
echoed capture ID and fixed `capture-origin-not-authorized` code: no hash, transcript,
session, destination or claimed archive acknowledgement. Older receivers ignore the
unknown stage. Updated Wear shows an actionable origin-confirmation error without
promoting transfer/recognition state or deleting retained text/audio.

Existing captures without an association remain local-only: no STATUS/header replay
can claim them, and remote transcript delivery is withheld. Their local history,
audio, explicit finalization, recognition and Org export remain available. No rows
or audio are deleted or rewritten to backfill origin guesses. Legacy captures and
changed SDK node identities after reinstall require a future explicit local origin
confirmation/rebind feature; no such UI or automatic rebind is introduced here.
An unbound orphaned audio file also cannot be claimed by START followed by a matching
header replay. New archive admission requires the verified full incoming body, not
just a header/hash match against a pre-existing file.

Small v1 message receipts and stored record schemas remain compatible. The former
persistent oversized DataItem route is intentionally no longer emitted or consumed:
updated phone and Wear builds are required together for oversized completions.
Existing global DataItems created by older installations are not retroactively
made private or deleted by this change. Ordinary status retry can request a fresh
exact-node receipt only for an authorized bound origin. Canonical records and the
database version remain unchanged; the private origin table is added lazily without
backfilling older rows. No audio capability or credential transfer is introduced.

Projected and peer-visible diagnostic codes use the shared fixed allowlist, not
exception messages, URI/path values, or exception class names. Unknown historical
codes become a fixed failure code at public boundaries; private recording and SAF
URI fields retain their existing explicit local-use contracts. Detailed exception
logging is not added. Matching duplicate interrupted archives retain the phone's
explicit `SAVED`/`user-finalized` decision; conflicting content remains rejected.

[SafNotesStore](android/src/main/java/dev/gitblame/voxgolem/capture/SafNotesStore.kt)
owns destination preference, one persistable read/write grant acquisition and
verified WAV archival. Plugin result handling validates offered read/write flags
before selection. SAF uses document URIs, never guessed external filesystem paths.

[OrgExportCoordinator and its private journal database](android/src/main/java/dev/gitblame/voxgolem/capture/OrgExportCoordinator.kt)
move together unchanged. `capture_org_journal.db` version1, row keys, image paths,
pre/post hashes, exact `ai-inbox.org` name handling, process serialization and
read-back safeguards are preserved. Ambiguity remains `NEEDS_REVIEW`, not a claim
of atomic provider replacement or cross-process editor locking. Legacy completed
writes are acknowledged only after matching capture/destination/version/phase,
provider tree membership, owned recovery images, lengths, entry and current
postimage hash; acknowledgment is metadata-only. Unwritten preimages and unsafe
or unknown outcomes remain held. Already-exported corrections do not rewrite or
duplicate the exported entry; interrupted audio requires explicit finalization.

## Verification boundary

The phone library retains core-library desugaring for Java time at minSdk24.
This extraction adds no dependency or toolchain change. Existing phone coordinator,
channel, adapter and SAF provider tests remain the regression owners; shared-domain
tests run in `:capture-android`, not a copied phone implementation. Final integrated
phone/shared tests, Android lint and app builds are primary-coordinated gates after
all source lanes finish. API36 provider/FGS behavior and physical watch/phone
acceptance remain separate device gates; no new installation is authorized here.
