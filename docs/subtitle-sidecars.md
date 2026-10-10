# Subtitle resources and acknowledged sidecars

The 1.0.2 patch adds explicit WebVTT resource preparation, bounded acknowledged
subtitle delivery on Native/WASM, and joint native recovery for fixed subtitle
files and media. Ordinary Writable/`SubtitleSink` implementations remain
ineligible for recoverable output. SDK adapters own WebVTT serialization and
publication; Engine validates and coordinates the durable prefixes.

## Resource preparation

Use `ResourceRequest::webvtt_media(snapshot, index)` or
`ResourceRequest::webvtt_map(snapshot, index)` for immutable VOD, Live or EVENT
snapshots. The SDK still reconciles its subtitle snapshots and owns their input,
generation and revision identities. Requests retain the complete descriptor,
original media sequence, epoch, range and frozen MAP/key declaration context.
The existing `media` and `map` constructors retain their strict finite media
profile and container validation.

`ResourceSession::read` returns `ClearContainer::WebVtt` or `WebVttHeader`, clear
bytes and selected key reference/version/IV evidence. Clear and AES-128 resources
use the existing operation budgets, key provider, TTL/invalidation, cancellation,
zeroizing buffers and complete-resource range policy. Sample encryption and GCM
are not WebVTT profiles. The SDK parses UTF-8 and WebVTT **after** preparation;
`read` intentionally accepts bytes that may later fail subtitle syntax validation.
CBC is unauthenticated: successful padding does not establish a correct key/IV.

For an Engine operation, prefer `handle.read_subtitle_resource(source, request)`
to share its resource and key budgets with media. The request's input ID must be
a selected subtitle track. This does not admit cues automatically. The SDK parses
the returned bytes and maps its X-TIMESTAMP-MAP into the bound media source clock.

`cue.with_resource(&clear)` binds a digest of the complete descriptor, plaintext,
selected key identity and stable provider version to that cue. Call it in the
same order for MAP then segment when both contribute to parsing. Encrypted
bindings require `KeyVersion::Provider`; operation-local revision numbers are not
stable recovery evidence. The digest participates in queued-cue replay identity;
no raw key, URL or cue body is added to the checkpoint. A changed resource/version
or missing fresh preparation fails queued replay with `ReplayRequired` before
media prefix truncation. Deserializing a cue restores its digest but does not
constitute fresh resource preparation: reconstruct it from freshly read resources
before replay, using the restoring Engine handle so preparation shares the new
operation. Cloned cues from an earlier in-memory operation also fail this check.
Completed/offline restores require no pending-cue replay.

## Sink contract

Attach `session.with_subtitle_sink(Arc<dyn SubtitleSink>)` before admitting cues.
Implement these two asynchronous methods (the future alias works on both targets):

```rust,no_run
use hls_engine::{SubtitleCommit, SubtitleSink, SubtitleSinkFuture};
struct Sidecar;
impl SubtitleSink for Sidecar {
    fn commit<'a>(&'a self, batch: &'a SubtitleCommit) -> SubtitleSinkFuture<'a> {
        Box::pin(async move {
            for cue in batch.cues() {
                // Use cue.disposition() to decide whether to write a cue.
                // Serialize cue.cue().payload()/identifier()/settings() and
                // cue.start()/end(), selecting track_id()/output_index().
                // Await each caller-owned Writable's write here.
                let _ = cue;
            }
            Ok(())
        })
    }
    fn finish(&self) -> SubtitleSinkFuture<'_> {
        Box::pin(async {
            // Await all required Writable closes here.
            Ok(())
        })
    }
}
```

A successful `commit` acknowledges the **entire** batch. Calls are serialized:
only one batch is in flight. Delivery follows successful media `write_all` and
`flush`; it is independent of the optional, lossy event callback and final report
history. Sink failures return `SubtitleOutput` with a redacted diagnostic and an
explicitly accessible raw cause. The operation cannot report success before
`finish` resolves. A later media finalization failure can still fail the operation.
Neither ordinary media flush nor a sink acknowledgement implies crash durability.

Each `CommittedSubtitleCue` includes an numeric receipt (preserved for pending cues across fixed-file recovery), subtitle
input/track, source cue (generation/epoch/body/identifier/settings and optional
resource identity), output index, disposition and actual output-local start/end.
Receipts are allocated in submission order, including rejected late cues;
`SubtitleAcceptance::first_receipt()` identifies the beginning of the submitted
batch. Equal WebVTT identifiers, bodies and overlapping cues are never deduplicated.
All portions of one cue retain its receipt. A cue crossing commit boundaries may
be emitted as consecutive clipped portions. Times are quantized to the same
90 kHz clock as embedded wvtt; the SDK converts them to WebVTT timestamps using
its chosen serialization precision. Rejected intervals are diagnostic and must
not be emitted as subtitles.

`SubtitleFrontier` identifies the input, track, generation, epoch and output and
its sealed output-local end. No subsequently accepted cue can change the earlier
interval. Empty subtitle windows also advance frontiers. Range clipping, split
output origins, media shifts and gap policies use the shared embedded-wvtt path.
Final independent subtitle tails are delivered before `finish`. Entirely excluded
range cues receive `RejectedRange`. Media EOF or stop drains accepted work;
unknown cue epochs fail with `MissingSubtitleMapping`. The duration limit stops
media admission using the existing policy; already accepted subtitle tails still
drain (range end, when set, clips those tails).

`SubtitleTrack::with_embedded(false)` keeps the same mapping and sidecar delivery
without adding wvtt to media. Such tracks require a subtitle sink. Other subtitle
tracks may remain embedded in the same operation. Plain-text/standard-setting
validation remains the existing subtitle profile.

## Bounds and cancellation

Cue admission, generated samples and pending deliveries have explicit count/byte
limits. A pending batch uses at most `EngineLimits::samples()` cue records and
`sample_bytes()` accounted bytes, independent of bounded report history; a batch
that cannot fit fails with `BudgetExceeded`. It does not grow a hidden delivery
queue. The one in-flight batch is released on acknowledgement. Caller-retained
clones and memory allocated by the SDK/Writable are outside Engine accounting.

While a sink is pending, media consumption pauses and new admission returns
`WouldBlock`; use separate producer/executor tasks and `accept_cues_when_ready`.
A sink must complete from its own write progress, not await new Engine admission
from that same blocked operation. `stop()` waits for the pending acknowledgement
and drains; `cancel()` interrupts it, drops the pending future and prevents later
acknowledgements from resuming the operation. Cancellation cannot undo already
written external bytes. On write/close failure or cancellation, cleanup of the
caller-owned sidecar is the SDK's responsibility; no success-close is fabricated.

## Joint native fixed-file recovery

Implement `RecoverableSubtitleSink: SubtitleSink` and attach it with
`session.with_recoverable_subtitle_sink(Arc::new(adapter))`, then use
`write_recoverable_to_file`. This adapter is native-only; the acknowledged
Writable contract above remains shared with WASM.

- `destinations()` declares exactly one `SubtitleDestination::new(input, path)`
  for every selected subtitle input, including embedded tracks. It must not open
  files. Roots are canonicalized, sorted by input ID and bound to the checkpoint.
  Duplicate inputs, overlapping media/sidecar namespaces and changed roots fail.
- `format_identity()` is a stable, nonempty SDK serialization/version identifier.
  Change it when formatting, timestamp precision or sidecar semantics change;
  restoring with a different identity fails before any output mutation.
- The SDK lazily opens `destination.partial_path(output)` for append inside
  `commit`. Engine creates the empty partial only after validation. Write the
  UTF-8 `WEBVTT` header when its length is zero, retain identifiers/settings and
  serialize only written/clipped portions. Empty windows must still initialize
  the file. Flush before acknowledging; `finish` awaits every required close.
- Output 0 uses the supplied final path; subsequent outputs append
  `.part-000001.vtt`, etc. Partials append `.hls-partial`. `publish(output)` closes
  any remaining writer for that child and publishes all selected tracks without
  overwriting existing files. A hard link from partial to final is suitable.
  Retain the partial until a durable Completed checkpoint; do not rename it away.
  The adapter must be idempotent at a validated publication boundary. Engine
  verifies all published sizes/hashes before reporting media publication success.

Engine ordering is media write/flush → SDK subtitle write acknowledgement →
optional file `sync_all` → joint checkpoint callback. Sealing/finalizing checkpoints
are persisted before SDK `publish(output)`, followed by media publication and a
second callback. A checkpoint callback must atomically persist the supplied
archive before returning success. `SyncAll` syncs all committed file prefixes;
SDK/checkpoint persistence owns directory sync and the durability of publication
names. Neither mode promises atomic publication across files. A crash can leave
one final visible while another remains a partial; restoring the saved checkpoint
validates and completes this transition idempotently.

Recovery validates replay (including fresh subtitle resources/provider versions),
all fixed destinations, prior published children, media fragment structure and
**every** committed sidecar prefix before truncating **any** uncommitted tail.
Native Unix also rejects cross-track/output hard-link aliases; symlink sidecar
files are rejected. Keep exclusive ownership of all output paths throughout the
operation on every platform: concurrent external replacement/writes are unsupported.
Admission is blocked while a checkpoint drains late dispositions and persists,
so its receipt state cannot advance past an unacknowledged batch.

Queued receipt numbers, source hashes and sealed timing watermarks are persisted;
replay of pending cues preserves their delivered receipt numbers and remaining
intervals. Re-submit the pending cues/resources before running restoration.
`first_receipt()` during replay describes that submission; the restored delivery
uses its saved receipt. Fully sealed replay is discarded. New accepted cues
receive unused receipts. No cue text, raw key or per-recording list of committed
cue bodies is retained in the checkpoint. The file ledger grows per output/track,
shares the metadata budget and fails explicitly when that budget is exhausted.

Joint checkpoints use schema 3 (`HLSECP03`), extending the frozen schema-2 fields
with a sidecar configuration digest/file ledger and versioned receipt state.
`schema_version()` reports the actual archive; `ENGINE_CHECKPOINT_SCHEMA_VERSION`
is the newest supported version. Media-only sessions continue to emit schema 2,
and the original schema-2 fixture decodes/re-encodes unchanged. There is no in-place
migration that adds sidecars to an existing media-only checkpoint: finish that
operation or start a fresh operation. Schema-3 restore requires the same fixed
adapter; an arbitrary sink or missing adapter is rejected.
`has_subtitle_files()` and `subtitle_file_prefixes()` expose the durable file
ledger without revealing paths, resource URLs, keys or cue bodies.

A Completed restore needs only the final media and subtitle files. It does not
read sources, resolve keys, admit cues, invoke sink write/close/publication methods
or rewrite any output.
All partials may then be deleted by the SDK. Damaged/missing final files fail
validation. A failed acknowledgement, checkpoint callback or publication never
reports operation success; after partial publication, recovery uses the last
persisted sealing/finalizing archive.

## Verification and release boundary

`tests/subtitle_recovery.rs` uses a UTF-8 WebVTT SDK adapter, duplicate identifiers,
overlaps, tails and two tracks. It compares uninterrupted and resumed sidecar bytes
and media content across real SIGKILL windows in media-to-sidecar handoff, partial
sidecar writes, close, checkpoint replacement and publication. It covers embedded
and sidecar-only output, fMP4/classic MP4, corrupt prefixes, missing replay,
changed key versions, destination conflicts and offline Completed restores.
`recovery_faults.rs` additionally injects I/O failures and abrupt process exits
around writes, sync, truncation and publication across configuration splits.
The original media-only/schema-2 recovery tests remain part of the gate.

These contracts and tests make the native recovery implementation reviewable.
The downstream M4 delivery gate additionally requires a published registry release;
a local version bump or successful package build does not establish publication.

The shared `tests/support/subtitle_contract.rs` runs on native and actual WASM;
its CBC fixtures come from OpenSSL, and its small independent ISO-BMFF reader
compares sidecar intervals to emitted tfdt/trun/wvtt samples. The JS runtime bridge
exercises asynchronous Writable writes/close, failures, concurrent operations and
late acknowledgements after cancellation.
