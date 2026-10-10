# Engine integration and recovery

## Unified API

Use `EngineSession::new(EngineInputs, KeySession, EngineOptions)`. `EngineInputs`
selects the primary input, embedded-audio policy, additional audio and subtitle
tracks. The handle accepts typed snapshots and cues, ends individual inputs, and
controls pause, stop and cancellation. The implementation uses the existing
multi-track scheduler rather than a second media pipeline. Public `Engine*`
names currently re-export that implementation.
The executable [Engine example](../examples/engine_demo.rs) demonstrates finite
snapshot admission, independent EOF and bounded byte output.

`into_bytes`, `write_to`, `write_to_file` and output-provider methods remain
available. Borrowed writers are flushed but never closed by the engine. Finite
snapshots/ENDLIST and explicit per-input EOF use the same core as open input.

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
Schema-v1 tasks continue through `legacy`.

Public configuration structures keep private fields and builders, with read-only
accessors for options, budgets and recovery settings. New report/error/event
variants remain non-exhaustive where extension is supported. Root `Engine*`
aliases include event callbacks, input progress, mappings, output reports, media
reports and peaks so callers can name the complete stable API without importing
legacy names. GCM remains an explicitly enabled experimental draft-22 profile;
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
requirements. Borrowed writers, collected bytes, WASM filesystem recovery and the
FFmpeg multi-track file backend remain rejected by the recovery query.

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
merely because the crate reaches 1.0.

`EngineError::cause()` preserves resource, sample and media causes. Context accessors
expose input, generation, epoch, source track and sample when available; authentication
failure has its own kind. Capability decisions distinguish `container_supported()`
from `playback_supported()` and retain explicit player rejection reasons. Recovery
queries distinguish the native file persistence contract from ordinary writer output.

## Compatibility

Change the Cargo dependency to `hls-engine`; move historical Rust imports from
`hls_transmux::...` to `hls_engine::legacy::...`. Adopt the root `Engine*` interface
for new sessions. Source and key-provider implementations remain reusable.
The legacy `with_audio()` replacement behavior remains unchanged. New
`EngineInputs::with_audio()` adds a selected audio input.

Keep schema-v1 records on `legacy` file APIs. There is no implicit v1-to-v2
checkpoint conversion, and a failed prepared partial is not a checkpoint. Historical digest domains remain unchanged.

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
