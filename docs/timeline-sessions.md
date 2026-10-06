# Timeline sessions

The additive `prepare_hls_timeline` entry supports finite clear/AES-128/sample-encrypted
presentation ranges, epoch mapping, explicit gap policies and decodable split
outputs. Legacy prepared/keyed entry points and checkpoint schema v1 retain
their original behavior. See the [release record](release-0.7.0.md).

## Entry point

```rust,no_run
use hls_transmux::{
    crypto::key::KeySession, KeyedInputs, MediaTime, PresentationRange,
    TimelinePrepareOptions, TimelineResult, prepare_hls_timeline,
};

async fn export(inputs: KeyedInputs, keys: KeySession) -> TimelineResult<Vec<u8>> {
    let range = PresentationRange::new(
        MediaTime::new(5, 1).unwrap(), MediaTime::new(12, 1).unwrap(),
    )?;
    let session = prepare_hls_timeline(
        inputs, keys, TimelinePrepareOptions::default().with_range(range),
    ).await?;
    let (bytes, report) = session.into_mp4_bytes().await?;
    let _actual = report.actual_range();
    let _preroll = report.preroll()?;
    Ok(bytes)
}
```

Inputs are finite typed clear/AES-128/sample-encrypted snapshots. External audio replaces
embedded audio. Complete encrypted BYTERANGE resources require the same explicit
`ResourceOptions` policy as keyed sessions. Original sequences, MAP declarations
and key contexts survive range selection and replay.

Requests use public presentation intervals `[start,end)`. Video selection expands
to a verified independent access point and includes the tail GOP. AAC selection
retains complete frames. Requests wholly outside media, containing only a gap,
or lacking a usable access point fail with a typed error. End beyond EOF clips
to available media. CRA/recovery points with unproven dependencies are not treated
as independent entry points merely because the container sets a sync flag.

## Gaps, epochs and outputs

- `GapPolicy::Preserve` is the default. Fragmented output starts new runs across
  decode gaps without extending sample durations over missing media.
- `GapPolicy::Collapse` removes only intervals containing no selected media.
  The same shift applies to DTS, PTS and external interval mapping. A gap in one
  track with media in another is not a removable common gap.
- Classic output rejects remaining internal decode gaps unless
  `TimelineChangePolicy::Split` is explicitly selected. Configuration changes
  likewise require splitting at a decodable boundary or fail.
- `into_mp4_bytes`, `write_to` and `write_to_file` reject Split. Use
  `into_mp4_outputs`, `write_to_outputs` or `write_to_files` instead. Providers
  supply output leases or explicit paths, asynchronously. Writers may be non-Send
  and are flushed, never shut down by the library.
- The next lease is acquired before the old output completes. Already completed
  outputs are retained on failure. Native file publication uses temporary files
  and rename; a writer flush is not a durable checkpoint.

Epoch mappings preserve source times, public times, output origins, track/input
identity, configuration fingerprints and available PDT references. Resets without
sufficient cross-input evidence require explicit `EpochAnchor` values. Rendition
sequence numbers are never used as synchronization clocks. `map_interval` splits
external cue/chapter intervals at mapped boundaries; it does not parse WebVTT or
mux subtitles. Reports include source resource/MAP/key declaration dependencies,
without raw keys or credential-bearing resource URLs.

With `serde`, signed ticks, epochs, indexes and counters use decimal strings.
Deserializing `MediaTime`/`PresentationRange` validates integer precision,
timescales and request bounds. Report serialization is a diagnostic transport,
not a resume format. `source_bytes` currently counts media-resource ciphertext
(or clear input bytes) including rereads, and excludes MAP traffic.

## Execution and memory contract

Finite execution has three stages: a resource/clock scan, a bounded sample cursor
that validates selection and builds the output plan, and verified output replay.
The scan keeps the current and preceding resource's sample metadata to infer TS
tails; it stores only per-resource clock/configuration/coverage summaries after
that. The cursor merges track queues in decode order, releases consumed resources
and regenerates sample timing from verified media. It never retains a sample
index for the entire selected range. Long GOP preroll is replayed from its source,
so even a long GOP does not require buffering all its samples.

`TimelinePlanningLimits` defaults to 65,536 live sample records and 4,096 live
cursor resources. `with_planning_limits` accepts positive limits. A resource or
track-skew window exceeding these limits returns `PlanningBudgetExceeded` during
validation, before acquiring an output. Output replay is paced by writer
backpressure. `peak_planned_samples` and `peak_planned_resources` report these
live-record high-water marks; they are not allocated capacity or RSS.

The immutable finite resource catalog, playlist snapshots, dependency reports,
epoch/gap mappings and native classic-finalization indexes have separate costs.
`indexed_resources` exposes the catalog size. Catalog cost grows with scanned
resources; reports grow with resource/epoch/gap count. Each resource's payload and
demux work is subject to the separate byte limit. Classic bytes additionally
retains selected payload and final bytes. No API promises constant total memory.

The finite scan and validation complete before first output: first-write latency
therefore grows with the scanned selection. This permits complete provider
requests and failure before sink acquisition, including ambiguous clocks and
unrepresentable gaps. Near ranges stop once their tail access point is known;
far ranges still scan the prefix for clock and access-point evidence. See
[measured reads, allocation and latency](benchmarks.md). Open-input latency and
incremental playlist acceptance belong to v0.9.

The finite combination matrix now additionally covers encrypted ranges across
three clock-reset epochs with distinct MAP/media keys and MAP redeclarations;
all four primary/audio gap combinations with Preserve/Collapse and clear/AES-128
resources; one-frame TS EOF with required evidence or an explicit duration;
partial media writes and failed flushes in the second split output; and
cancellation at three checkpoints during native finalization of that output.
The last cases verify completed reports, preservation of existing destinations,
and temporary-file cleanup.

The v0.8 sample path resolves protection and awaits keys before codec checks,
keyframe detection and NAL normalization. Replay identity includes scheme, KID,
IV, pattern and subsamples alongside original layout/timing. fMP4 and AAC samples
retain their lengths and declared clear bytes. TS AVC may shrink only when removing
encryption-layer emulation prevention; the clear NAL mapping is checked.
See [sample encryption](sample-encryption.md) for the supported container metadata.
Timeline rereads verify both resource bytes and decrypted packet digests, so a
changed key cannot silently alter an accepted output. Packed AAC stays internal.

## Verification entry points

- `cargo test --test timeline_session --features serde`
- `python3 scripts/verify_timeline.py` — independent generated B-frame/negative-CTS
  corpus through bytes, native finalization and fMP4 writer, clear and AES-128,
  for full export and GOP-expanded ranges. Add `--ffmpeg` for the optional finalizer.
- `cargo run --example timeline_budget` — deterministic near/far read counts and
  planning-state peaks, full-range and long-GOP success under a fixed budget,
  allocation totals and first-write latency, plus oversized-window rejection. The same
  cases execute in the runtime suite; see [measurements](benchmarks.md).
- `python3 scripts/verify_sdk.py /path/to/hls-downloader` — downstream native/browser timeline
  integration verification using the real SDK host in an isolated copy.
- `python3 scripts/verify_runtime.py --browser` — native, actual Node WASM and real
  Chrome compare timeline output hashes, reports and lossless time transport.

The fixed key in `examples/timeline_export.rs` is exclusively a synthetic fixture
key, not an application key-management implementation.

Local verification and exact commands are recorded in
[release-0.7.0.md](release-0.7.0.md). Tests cover TS wrap and half-period rejection,
reset-after-wrap anchors, TS gap tails, same-config MAP redeclaration, ambiguous
dual-input resets and explicit anchors, PDT calendar/rounding, cancellation/drop
in all three read stages, keys and output leases, blocked writes/flushes and
native finalization. Publication checks execute the timeline tests and examples
from an extracted crate. No registry publication is performed by those checks.

## Downstream SDK integration

`verify_sdk.py` copies the SDK adapters and fixtures, points the disposable
manifests to this crate version, and first checks legacy prepared/resume and
native/WASM adapter compatibility. It then installs the tracked integration
extension from `tests/support/sdk` into that copy. The extension uses the SDK's
actual `SourceHost`, key provider, callback leases and Promise I/O bridge.

Nine cases compare native and real browser report JSON and canonical MP4 hashes:
clear/AES ranges, clear/AES dual input, clock reset, gap collapse, gap splitting,
preserved fragmented gaps and codec-change splitting. Output Promise completion
is awaited. `target/sdk-compat/evidence.json` records the SDK revision and upstream
source digest. The SDK checkout and locks are unchanged; the extension is an
upstream integration fixture, and SDK product rollout is a separate downstream
release activity.
