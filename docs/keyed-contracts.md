# Keyed session diagnostics, progress and capability queries

These additive APIs describe the finite keyed prepared entry (clear/AES-128 since v0.6,
[finite sample encryption](sample-encryption.md) since v0.8). Legacy `Error`, `SessionPhase`, source
bounds, options literals and checkpoint schemas retain their existing contracts.

## Query a combination

```rust
use hls_transmux::capabilities::*;
let query = KeyedCapabilityQuery::new(
    KeyedInputCapability::new(
        KeyedContainer::TransportStream,
        KeyedEncryption::Aes128,
        vec![KeyedCodec::Avc, KeyedCodec::AacLc],
    ),
    KeyedOutput::FragmentedWriter,
);
let decision = query_keyed_capability(&query);
assert!(decision.supported());
assert!(decision.requirements().contains(&CapabilityRequirement::ProviderResolution));
let refused = query_keyed_capability(&query.with_resume(true));
assert!(!refused.supported());
assert_eq!(refused.rejections()[0].dimension(), CapabilityDimension::Resume);
```

A query performs no I/O. `supported()` means the declared combination has an
implementation in the **current target/feature build**. Requirements still apply:
finite snapshot validation, actual container/codec validation, compatible timing
and configurations, provider availability/authorization, and complete encrypted
range boundaries where requested. It does not certify bytes, a KEYFORMAT adapter,
a provider, a decoder/player, authenticity, or arbitrary content carrying the same
codec name. `KeySession::validate_resource` checks configured KEYFORMAT/version
selection and IV agreement without fetching or admitting a request.

Queries distinguish container, encryption method, sample protection scheme,
codec, key source, source mode, output, range, resume, track selection, subtitles,
experimental requests and timeline changes. Every unsupported dimension is
returned with an input role when applicable. TS/fMP4, AVC/HEVC/AAC-LC, clear/AES-128
and mixed clear/AES resources are supported within the finite profile. Replacement
external audio may use a different container/encryption mode. Generic multi-track,
subtitles, Packed AAC, GCM, live/EVENT, rewritten snapshots,
gaps/discontinuities and presentation ranges remain rejected. The experimental
flag cannot bypass those checks.

`ClearByteRanges` declares that ranges apply only to clear resources; other
resources in that input may be encrypted whole resources. `CompleteEncryptedResources`
requires the caller's independent-boundary attestation in resource options.
Arbitrary encrypted subranges and I-frame ranges are rejected.

MP4 bytes and borrowed fragmented writers are portable. File outputs require a
native target; FFmpeg streaming finalize additionally requires `ffmpeg-finalize`.
Built-in HTTP key resolution and CDM are not provided: an external provider may
implement HTTP transport/authorization itself. SDKs retain source transport and
writer close/abort ownership.

`prepared.capability_query(output)` derives input profiles from the immutable
snapshots and selected, probed tracks. `report.capability_query()` records those
profiles and the actual successful output mode. It is a description of this
operation, not permission to reuse keys or accept a rewritten snapshot. Snapshot
rewrite detection belongs to the P1 resource comparison API; this finite entry
has no update/resume method and never accepts a changed snapshot mid-operation.

## Inspect failures without leaking raw messages

`KeyedSessionError::kind()` retains the broad P4 categories; `failure()` distinguishes
unsupported combinations, provider unavailable/failure, invalid key/IV/encryption
metadata, expired keys, decrypt failure, clear media validation, read, budget,
output and cancellation. Malformed playlist syntax/IVs remain typed
`PlaylistError`s from the parser, before a keyed session exists. A provider cannot
construct a malformed AES key: `SecretKey::new` validates its length; an adapter
can represent an invalid host response with `ProviderFailureKind::InvalidResponse`.

`resource_context()` carries the original input/generation/epoch/sequence, resource
kind/range, method and immutable candidate key references. Resource failures expose
a selected reference/version/resolution revision when resolution reached that
point. Unknown context stays absent; global writer/finalizer failures do not acquire
an arbitrary input, and a resource-level failure does not invent a track/sample.
Manifest-known unsupported keys/ranges are checked across **both complete snapshots
before any resource or provider I/O**, including declarations after the probe window.

The standard `Error::source()` chain is safe to format and inspect/downcast:
`KeyedSessionError -> ResourceError -> KeyError -> ProviderFailure`, when present.
It stops before untrusted provider/transport messages. Core media/output failures
instead expose a safe typed `KeyedFailure` source. Use `raw_cause()` explicitly for
underlying legacy/source errors, or `ProviderFailure::raw_cause()` to inspect the
original typed provider cause. These explicit raw values may contain credentials.
Debug/Display omit key bytes, provider messages and opaque IDs; resource URLs
remove username, password, query and fragment. Key versions are opaque in Debug.
Progress/events/reports intentionally do not implement blanket serialization;
serialize selected safe fields, as the WASM example does. Playlist archives are
explicit transport data and may contain source URLs/credentials.

## Count each boundary separately

Each input reports `discovered_segments()` (finite total), plus separate
`media()` and `maps()` resource counters:

| Counter | Boundary and units |
| --- | --- |
| downloaded resources/bytes | A bounded complete source read returned; bytes are before decryption, including CBC padding. Read failures do not claim partial body byte counts. |
| decrypted resources/bytes | AES-CBC/PKCS7 succeeded; plaintext bytes exclude padding. Clear passthrough resources do not increment these counters. |
| ready resources/clear bytes | Clear container validation succeeded. Includes decrypted and clear passthrough resources; demux/config validation can still fail later. |
| MAP cache reuses | A full context/current-key-validated MAP cache hit; does not double-count download, decrypt or ready bytes. |
| committed segments | The shared output core accepted that segment's samples (collector), or completed its fragment write/flush (writer). This is not a durable checkpoint. |

The input convenience accessors count **media only**. In particular,
`downloaded_bytes()` now uses the final v0.6 ciphertext/source-byte definition;
the unreleased P4 clear-byte definition is available as `media().clear_bytes()`.
`processed_segments()` is an alias for the committed boundary. Output bytes are
reported separately and include headers/indexes after their write/flush.

A slow provider can therefore show downloaded=1, decrypted=0, ready=0, committed=0.
Malformed decrypted media can show decrypted=1 but ready=0. Probe/lookahead can show
ready ahead of committed. MAP metadata and bytes do not advance media counts.
Events carry resource context at read/decrypt/validate/cache boundaries and original
slots at commit boundaries. Counter overflow fails rather than wrapping.

Callbacks run outside internal locks. Native callbacks require Send+Sync; WASM
callbacks may own JS values. Cancellation/drop prevents further operation callbacks,
and failed writes/final flushes never emit Completed. Completed marks the library's
flush/publication boundary; the SDK must still finish its owned writer close.

## Runnable examples

- Native: `cargo run --example keyed_demo -- input.m3u8 output.mp4`. The example reads local key files (at most 17 bytes to reject wrong lengths), uses the native file finalizer and prints safe byte/commit counts. A native default-source build is required.
- WASM: [keyed WASM example](keyed-wasm.md), built with `cargo build --no-default-features --target wasm32-unknown-unknown --example keyed_wasm` from the repository or unpacked crate. The host supplies resource Uint8Arrays, Promise key resolution and a JavaScript progress callback. Original u64 sequence/revision and byte counters cross JS as decimal strings. The example collects MP4 bytes; it is not a streaming HTTP adapter.

Tests cover declarative supported/refused dimensions, runtime preflight refusal,
provider/raw-cause separation, secret scans, intermediate counters, native output,
and actual Node/Chrome execution of the JS example including rejected/unavailable/
wrong-length key responses. The P4 output/cancellation/compatibility tests remain
active. See the [release verification record](release-0.6.0.md); publication remains
a separate operation.

Sample failures expose `SampleError` through `sample_error()`, with non-exhaustive
kind, resource/track/sample identity, scheme, key reference/version and typed causes.
The sample profile capability query reflects the container/codec/method matrix;
protected container metadata is validated during demux before sample submission.
