# Keyed prepared sessions

`prepare_hls_with_keys` runs finite typed playlist snapshots through the same
probe, timeline, track selection, mux and output core as `prepare_hls`. The old
entry points, options, error enums and checkpoint schemas remain available.

```rust,no_run
use hls_transmux::{
    KeyedInput, KeyedInputs, KeyedPrepareOptions, KeyedSessionResult,
    Source, prepare_hls_with_keys,
    crypto::key::KeySession,
    playlist::PlaylistSnapshot,
};
use std::sync::Arc;

async fn export(
    snapshot: PlaylistSnapshot,
    source: Arc<dyn Source>,
    keys: KeySession,
) -> KeyedSessionResult<Vec<u8>> {
    let prepared = prepare_hls_with_keys(
        KeyedInputs::new(KeyedInput::new(snapshot, source)),
        keys,
        KeyedPrepareOptions::default(),
    ).await?;
    let (bytes, report) = prepared.into_mp4_bytes().await?;
    assert_eq!(report.inputs()[0].processed_segments(), report.inputs()[0].total_segments());
    Ok(bytes)
}
```

Parse snapshots with `parse_playlist_snapshot` and stable caller-owned `InputId`s
([typed playlists](typed-playlists.md)); construct the operation-scoped key session
with your provider and clock ([key sessions](key-sessions.md)). All selected
snapshots are validated before source/provider I/O. IDs must be distinct.
Master selection and fetching playlist text belong to the caller.

The execution profile accepts nonempty ENDLIST VOD or unspecified-type media
playlists. It supports clear and AES-128 TS/fMP4, AVC/HEVC/AAC-LC, explicit or
original-sequence IVs, encrypted MAPs, rotation at the same key URI, and METHOD=NONE.
`with_audio` replaces primary embedded audio. Each input advances independently;
sequence numbers are not used to align the two timelines. Open/EVENT playlists,
gaps, discontinuities and mid-input container changes remain rejected by this entry;
use timeline sessions for those policies. The v0.8 [sample profile](sample-encryption.md)
adds TS SAMPLE-AES AVC/AAC-LC and fMP4 cenc/cbcs AVC/HEVC/AAC-LC.
CBC and CTR do not authenticate the key or plaintext.

Encrypted BYTERANGE defaults to rejection. Set
`ResourceOptions::with_encrypted_ranges(EncryptedRangePolicy::CompleteResources)`
through `KeyedPrepareOptions::with_resources` only when each range is a complete,
independently padded AES-128 resource; block alignment alone is insufficient.
For sample encryption the attestation instead requires a complete container resource;
resource-wide padding/block alignment and AES MAP IV rules do not apply.
See [resource validation and limits](aes-resources.md).

`into_mp4_bytes` collects a classic MP4 in memory. `write_to(&mut writer)` produces
fragmented MP4, supports non-Send writers, and flushes without closing the borrowed
writer. Native `write_to_file` supports `Mp4`, `FragmentedMp4` and `StreamingMp4`,
including the optional FFmpeg finalizer. File publication uses the existing
atomic-rename path; a failed streaming operation may retain a non-resumable partial.
No encrypted checkpoint/resume contract is introduced.

Default probe/read concurrency is two segments/two inputs. Resource caps default
to 16 MiB each, 32 MiB pending ciphertext, and two admitted reads. Parallel input
reads automatically serialize when these caps permit only one full-size resource.
Provider admission has its own limits. These are distinct from total memory:
metadata, per-input probe/lookahead demuxed samples, one cached MAP per input,
classic samples/output and optional mfra indexing have separate costs. A stalled
writer starts no additional reads; probe samples are reused during execution.
Custom sources must honor the documented source limits to bound allocation before
returning a buffer. The core never stops an unrelated shared source; isolated
resource sessions are stopped on completion or dropped futures.

MAP reuse compares input identity/generation/epoch and the entire immutable MAP
(location, range, declaration and captured key context). Before reuse it resolves
the current key and checks the selected reference, version, resolution revision
and expiry for AES-128. Sample MAPs retain protection descriptors; keys are resolved
per protected sample through the operation key session. Explicit `prepared.invalidate_keys()`, TTL expiry, cache eviction or
a new MAP declaration can force re-read. MAPs are never shared across operations.

Cancellation races resource reads, provider waits and writer writes/flushes.
Dropping preparation/output releases resource leases and cancels pending provider
work. Already probed clear samples remain accepted if keys later expire or are
invalidated; future resources re-resolve. Successful completion is emitted only
after the output's final flush/publication boundary. The caller owns its sink's
subsequent close/abort operation.

`KeyedSessionEvent` and `KeyedSessionReport` carry stable input IDs/generations,
resource context and separate discovered/downloaded/decrypted/ready/committed
counters. Downloaded bytes are source bytes before decrypt (including CBC padding);
clear passthrough does not count as AES decryption. MAPs have separate counters.
Safe failure categories, standard cause chains, capability queries and runnable
native/WASM adapters are described in [Keyed contracts](keyed-contracts.md).

Validation uses retained independent OpenSSL fixtures, normalized complete output
comparison (only MP4 creation/modification timestamps excluded), native file tests,
actual Node/Chrome WASM with asynchronous providers and a non-Send short writer,
and FFprobe packet/payload/timing plus FFmpeg decode/seek checks. This integration
does not implement live, subtitles or multi-track selection.
Run `python3 scripts/verify_sample_crypto.py --ffmpeg --ffmpeg-finalize` for the sample corpus.

Run `python3 scripts/verify_keyed_decode.py` for the Issue #1 regression gate.
It generates local HEVC/AVC B-frame inputs with FFmpeg, encrypts complete TS/fMP4
resources (including MAPs) with OpenSSL, and compares every decoded video/audio
frame against the independent clear input across bytes, file and writer APIs.
It also checks signed CTS, delayed audio, the longer audio tail and packet timing.
Normal edit-list handling is required; successful decoding and sample counts alone
do not establish that every displayed frame survived.
