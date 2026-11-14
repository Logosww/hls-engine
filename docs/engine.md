# Engine integration and recovery

## Unified API

Use `EngineSession::new(EngineInputs, KeySession, EngineOptions)`. `EngineInputs`
selects the primary input, embedded-audio policy, additional audio and subtitle
tracks. The handle accepts typed snapshots and cues, ends individual inputs, and
controls pause, stop and cancellation. The Engine owns a single scheduler and shared media core; finite and open input use the same implementation.
The executable [Engine example](../examples/engine_demo.rs) demonstrates finite
snapshot admission, independent EOF and bounded byte output.

`into_bytes`, `write_to`, `write_to_file` and output-provider methods remain
available. Borrowed writers are flushed but never closed by the engine. Finite
snapshots/ENDLIST and explicit per-input EOF use the same core as open input.

## Concurrent downloads

Use `EngineOptions::with_download_concurrency(n)` to configure concurrent media
reads for built-in and custom sources, on native and WASM. The default is four.
`n` bounds the combined number of in-flight and ready, unconsumed downloads
across the operation. Set the separate download window byte budget through
`EngineLimits::with_download_buffer_bytes` (default 64 MiB). This accommodates
four downloads at the default 16 MiB per-resource reservation. Set concurrency
to one for serial reads.

```rust
use hls_engine::{EngineLimits, EngineOptions};

let options = EngineOptions::default()
    .with_download_concurrency(4)
    .with_limits(EngineLimits::default().with_download_buffer_bytes(64 * 1024 * 1024));
```

Each download reserves its declared byte-range length or the configured
`ResourceOptions::max_resource_bytes` before starting. The window retains that
reservation until consumption, including completed downloads and errors.
Effective concurrency is reduced when the byte budget fills. With concurrency
above one, the window must hold at least one maximum-size resource. Zero
concurrency or a zero window is invalid. Resource decryption/admission and
sample buffers retain their existing, separate budgets. `ResourceOptions` and
`ReqwestSource::with_concurrency` alone do not configure Engine scheduling.

The Engine peeks only at admitted descriptors and downloads raw resource bytes.
It consumes them in timeline order, resolving keys, decrypting and committing
progress at consumption. A later download failure is reported when that
segment is consumed. MAP reads continue through the existing context-aware
cache. Demanded reads take priority over another input's speculative window.

The writer future owns all speculative futures and buffers. Output backpressure
and pause stop refilling and polling the window. Cancellation, failure and
writer-future drop release it without detached download workers. Sources must
honor the demand-driven session and per-resource byte-cap contract.

Prefetched descriptors remain in the admission queue. Checkpoints retain their
existing queued identities and committed watermarks, with no downloaded bytes
or speculative key state. Recovery validates replay through the existing serial
path before opening the download window. Concurrency and window size may change
when resuming an existing checkpoint.

## Native file finalization

Native `write_to_file` and `write_to_files` accept an optional classic MP4
finalization strategy:

```rust
use hls_engine::{FileFinalizationStrategy, FileOutputOptions, OutputFormat};

let options = FileOutputOptions::default()
    .with_format(OutputFormat::StreamingMp4)
    .with_finalization_strategy(FileFinalizationStrategy::InPlace)
    .with_header_reserve_bytes(1024 * 1024); // Optional; 1 MiB is the default.

// Pass these options to session.write_to_file(path, options).await
// or session.write_to_files(provider, options).await.
```

| Strategy | Finalization | Successful output |
| --- | --- | --- |
| `Copy` (default) | Copy samples into a second temporary file, then publish it | Compact faststart MP4 |
| `InPlace`, reservation fits | Write the complete index into the reserved header area | Faststart MP4 in the same temporary file |
| `InPlace`, reservation too small | Move the media region towards the end of the same file, then write the index at the front | Faststart MP4; no fallback to a tail index |

`Copy` remains the default: it copies samples into a compact, faststart file.
`InPlace` also always produces faststart: `ftyp + moov + [free] + mdat`.
It reserves 1 MiB before the intermediate fMP4. At finalization it writes the
complete index into this space. If space is insufficient, it shifts the media
region towards the end of the same file, copying blocks from end to beginning
with a 1 MiB buffer. It recalculates offsets and any `stco` to `co64` growth before
modifying the file. It never falls back to a tail index or creates a second full
media file. The old initialization and fragment headers remain as unreferenced
bytes inside `mdat`, so the result is not byte-identical to the compact copy output.

Use `with_header_reserve_bytes(n)` to tune the reservation: zero disables it and
forces same-file relocation; nonzero values must be at least eight bytes. Values
1–7 return `InvalidOptions` when the in-place strategy is active. Excess space
remains a `free` box. If the final index leaves only 1–7 bytes, the file is expanded
and relocated as needed to maintain valid box boundaries. The reservation is a
performance hint, not an index size limit. Enough space avoids media copying; insufficient space requires one
full read/write pass over the moved region. Index memory still scales with sample
count. Filesystem allocation and copy-on-write behavior can affect disk usage.
Both `Mp4` and `StreamingMp4` file output support this option. `Copy` and
`FragmentedMp4` ignore the header reservation; `FragmentedMp4` also ignores the
finalization strategy. Split files reserve and finalize independently. The
strategy changes the engine-owned temporary file, not an existing input file or
a previously published destination.

The strategy and reservation methods are Native-only. WASM, borrowed writers and
`into_bytes` keep their interfaces and output layouts. Recoverable output still
uses `RecoveryOptions`, the copy strategy and existing checkpoint schemas.
In-place finalization is not a crash-recovery protocol. Failures and cancellation
before publication do not publish a destination; the private temporary file may
have been partially modified. Publication retains the existing no-clobber rule.

Choose `InPlace` when avoiding a second full media file matters. It is not a
promise of faster finalization when the reservation is insufficient. In the
[local benchmark](benchmarks.md#local-result-2026-11-14), 8 GiB inputs showed no
stable speed advantage: median finalization was 13.72 seconds for Copy and
14.01 seconds for InPlace with the default 1 MiB reservation. These are local
synthetic I/O measurements, not production-player validation. Keep Copy as the
default until representative production workloads justify a change.

The repository exporter accepts the same opt-in strategy:

```sh
cargo run --locked --example engine_export -- input.m3u8 output.mp4 --in-place
# Force the insufficient-reservation path while still producing faststart:
cargo run --locked --example engine_export -- input.m3u8 output.mp4 --in-place --header-reserve-bytes 0
```

## Native file recovery

`write_recoverable_to_file(path, RecoveryOptions)` writes a persistent
`path.hls-partial` sibling and emits `EngineCheckpoint` through a separate,
fallible callback. The default is fragmented MP4 and Flush. Use
`with_output_format(OutputFormat::StreamingMp4)` for Native classic finalization
and `with_durability(CheckpointDurability::SyncAll)` for file synchronization.

Persist callbacks atomically. Flush does not guarantee survival of power loss;
SyncAll does not persist the caller's checkpoint or directory entries. Keep the
partial and any checkpoint-referenced finalization artifact until the completed
checkpoint has been durably persisted. The engine deliberately retains those
files so a crash between publication and checkpoint persistence can recover.

To restore, construct fresh sources/key providers and call
`EngineSession::restore(inputs, keys, options, checkpoint)`. Re-submit snapshots
covering queued/lookahead resources and replay accepted subtitle cues before
running the recoverable file writer. This replay is bounded by the configured
admission/resource/sample limits. Stable resource locations identify the same
resources; put renewed transport credentials in the source/provider rather than
rewriting resource identities. Encrypted replay requires a stable provider key
version. A version mismatch is `ResumeConflict`, including identical key bytes
with a different provider version.

The engine verifies replay identities and sample hashes, validates the complete
committed file prefix and fragment count, then truncates an uncommitted tail.
Corruption/conflict does not modify the old file. Finalizing and completed
checkpoints require no source or cue replay. Final publication never overwrites
an unrelated destination. Repeated recovery at the publication boundary is
idempotent.
Once a Completed checkpoint is durable, its intermediate files may be removed.
Completed recovery verifies the final file alone. A failed completion callback
returns an error carrying the already-published output; it does not expose a
premature Completed state. Cancellation after publication cannot retract that file.

Schema 2 has a checksummed binary archive (`to_bytes`/`from_bytes`). Optional serde
uses `{schema_version: 2, archive: <lowercase hex>}`, preserving all integer widths.
The archive is versioned metadata, not an encrypted container: it holds codec
configuration, track metadata, clocks, watermarks, packet/cue/key-version hashes
and file identities. It contains no raw keys, resource URLs, credentials or
packet/cue payloads. Its checksum detects accidental corruption, not adversarial
modification. Schema 2 is frozen for 1.x. Changes to the binary layout or replay
semantics require a new schema version and an explicit compatibility/migration
path; an existing fixture must not be regenerated to hide incompatibility.
`tests/fixtures/checkpoint/engine-schema2.bin` pins the original binary envelope
and its lossless serde representation. Media-only operations still emit schema 2.
Fixed subtitle files opt into schema 3, which preserves the original envelope
fields and adds a joint file ledger and receipt state; both versions are readable.
See [joint subtitle recovery](subtitle-sidecars.md#joint-native-fixed-file-recovery)
for adapter obligations and the explicit no-in-place-migration boundary.
Schema 1 is unsupported. Existing Engine schema 2/3 checkpoints remain readable in Strict mode.

Public configuration structures keep private fields and builders, with read-only
accessors for options, budgets and recovery settings. New report/error/event
variants remain non-exhaustive where extension is supported. Public `Engine*`
types include event callbacks, input progress, mappings, output reports, media
reports and peaks so callers can name the complete stable API through the root API. GCM remains an explicitly enabled experimental draft-22 profile;
its status does not weaken the stable clear/AES/sample-encryption contracts.

Split policies use the supplied path for output 0, followed by
`<path>.part-000001.mp4`, `<path>.part-000002.mp4`, and so on. Each child has its own
`.hls-partial` sibling. Checkpoints carry an ordered size/hash ledger of completed
children. Recovery verifies that ledger before touching the active partial.
`completed_output(index)` exposes a child's size/hash; `output_index()` identifies
the active child. The ledger shares the configured metadata budget and therefore
has a finite capacity even when report history is truncated.

A checkpoint with `bytes_written() == 0` records acquisition intent before file
creation. Persist it as usual; it can recover a missing file or a partially written
header. `is_sealed()` records an intermediate child's publication boundary and is
separate from whole-session `is_finalizing()`/`is_completed()`. The callback may
run several times at one media position. `durability()` reports the guarantee of
that checkpoint; the caller may explicitly select a different guarantee when
continuing. A successfully published child remains available if a later child fails.

Default missing-data policy returns `ReplayRequired` without modifying the file.
For open inputs, explicit Skip/Split accepts eviction only when the new manifest's
media sequence proves the old resource has left the window in the same generation.
It records a gap and requires a random-access video boundary before continuing.
An omitted snapshot, an interior hole or a resource rewrite does not count as
window eviction. Collapse follows the declared shared gap interval; incompatible
per-input collapse intervals return `TimelineAmbiguous` before output acquisition.

Native file capability queries accept supported recovery combinations and list
caller checkpoint persistence, identity replay and stable key versions as explicit
requirements. Borrowed writers, collected bytes and WASM filesystem recovery
remain rejected by the recovery query.

Normal rolling manifests may evict committed segments while retaining required
lookahead. Restored progress includes newly admitted segments and retained revision,
generation, epoch and timestamp-wrap state; identity is per resource, not a digest
of the entire manifest. Resource/MAP digests and key versions are checked before
output truncation. Missing subtitle replay also fails before changing the file.

## Experimental GCM

Enable Cargo feature `experimental-gcm` and
`EngineOptions::with_experimental_gcm(true)`. It implements only the fixed HLS
draft-22 whole-resource layout: 32-byte key, 16-byte leading IV and 16-byte trailing
authentication tag. KEY must not include IV. The provider returns
`AvailableKey::aes256_gcm(SecretKey::aes256(bytes)?)`.

Authentication precedes demux, clear-resource observation and output. Complete
encrypted byte ranges require explicit attestation; arbitrary ciphertext slices
and I-frame playlists remain rejected. GCM never becomes a stable-standard claim
merely because the crate has a stable major version.

`EngineError::cause()` preserves resource, sample and media causes. Context accessors
expose input, generation, epoch, source track and sample when available; authentication
failure has its own kind. Capability decisions distinguish `container_supported()`
from `playback_supported()` and retain explicit player rejection reasons. Recovery
queries distinguish the native file persistence contract from ordinary writer output.

## Migration

See [1.1 migration](migration-1.1.md) for removed entrypoints and checkpoint rules.

## Recording costs

Fragmented recording uses bounded active resource/sample queues. Classic MP4
finalization builds a sample index whose size grows with the recording. Split
checkpoints retain a completed-output size/hash ledger within the metadata limit;
exhaustion returns `BudgetExceeded` and preserves completed children.
See [memory bounds](benchmarks.md) and [CI checks](runtime-tests.md) for measurement
commands and artifact locations.

Fresh output acquisition uses exclusive creation, including split children: a file
created by another writer after intent persistence is preserved and produces an
output error. Reopening an existing partial is reserved for explicit recovery of
the same output checkpoint.

## Optional AAC overlap repair

```rust
use hls_engine::{AacTimestampPolicy, EngineOptions};
use std::time::Duration;
let options = EngineOptions::default().with_aac_timestamp_policy(
    AacTimestampPolicy::RepairOverlaps { max_shift: Duration::from_millis(50) }
);
```

Strict is the default. The caller must provide a nonzero, representable limit;
50 ms above is an example, not a recommended universal threshold. Repair applies
only to selected MPEG-TS AAC-LC tracks, after decryption and timestamp unwrapping.
It keeps sample contents and order, extends overlapping clocks to the preceding
sample end and preserves forward gaps. It never drops frames or synthesizes audio.
Each corrected frame's shift from its original timestamp must fit the limit,
including accumulated corrections. Excess returns `AacTimestampRepairLimitExceeded`;
`aac_timestamp_error()` exposes the slot, original/expected times and limit.

When Strict rejects a recognized MPEG-TS AAC-LC overlap, it returns
`EngineErrorKind::AacTimestampOverlap`. `aac_overlap_error()` exposes an
`AacTimestampOverlapError` containing the input/segment slot, unwrapped source
time (`original`), required next-frame time (`expected`) and their difference
(`overlap`). This includes equal/backward timestamps and increasing timestamps
that still overlap the preceding frame. Existing rounding tolerance is unchanged.
Other timeline problems remain `TimelineAmbiguous`; fMP4 and Packed AAC errors
are not classified as repairable TS overlaps. The diagnostic describes the local
failure, not a guarantee that a complete retry will fit a repair limit. A caller
may explicitly create a new repair session; neither automatic fallback nor
cross-policy checkpoint restore is implied.

Exact sample clocks avoid accumulating 44.1 kHz rounding error. State continues
across segments and resets at discontinuity, generation/configuration changes and
missing segments. Video, fMP4 and Packed AAC retain their existing validation.
Excluded audio does not participate. A valid output timeline does not establish
that a damaged source's original audiovisual synchronization was recovered.

`EngineReport::media().aac_repairs()` returns bounded per-input counters, maximum
shift and first/last positions. At most one `AacTimestampRepaired` event is emitted
per repaired segment. Reports describe processed samples, including lookahead;
commit events separately identify durable output. Strict JSON omits `aac_repairs`.

Repair sessions write schema 4 checkpoints, including sidecar sessions. Strategy,
limit and algorithm version are bound to configuration identity. Restore requires
identical options and preserves pending repaired samples without applying repair
a second time. Strict retains the frozen schema 2/3 encoding and identity. A
schema 2/3 checkpoint cannot be resumed with repair enabled.
