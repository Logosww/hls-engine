# AES-128 resources (v0.6)

`crypto::resource` prepares complete clear or AES-128 HLS resources using typed
playlists and operation-owned key sessions. The additive
[`prepare_hls_with_keys`](keyed-sessions.md) entry integrates this resource path
with the shared prepared media pipeline. Legacy entry points remain unchanged.

```rust,no_run
use std::sync::Arc;
use hls_transmux::{Source, playlist::PlaylistSnapshot,
    crypto::{key::KeySession, resource::*}};

async fn read_first(source: Arc<dyn Source>, keys: KeySession,
    snapshot: &PlaylistSnapshot) -> ResourceResult<ClearResource> {
    let resources = ResourceSession::new(keys, ResourceOptions::default())?;
    let request = ResourceRequest::media(snapshot, 0)?;
    resources.read(source, request).await
}
```

A request is derived from a P1 snapshot that passes `validate_finite_vod()`;
open/EVENT/empty/I-frame/sample-encrypted/GCM/gap/discontinuity inputs are rejected
before source/key work. `ResourceRequest::map` uses the MAP's frozen declaration
and encryption context. Constructors check the finite snapshot; later prepared
integration can perform this preflight when building its finite execution plan.

## Ordering, IV and validation

The path is **reserve capacity → reserve a key waiter → read bounded ciphertext
→ resolve key → decrypt the complete resource → validate the clear container**.
A waiter is admitted before reading, but its provider is polled only after the
body arrives. Clear resources bypass key resolution. A key expiring during the
read or resolution returns a typed key error; the core does not silently retry or
try other keys after bad padding or invalid media.

AES-128-CBC uses PKCS7 and decrypts the owned buffer in place. The explicit IV
wins; otherwise a media resource uses the original u64 sequence, left-padded with
zeroes to 128 big-endian bits (`sequence_iv`). An encrypted MAP requires an explicit
IV. Keys are exactly 16 bytes via `SecretKey`; ciphertext must be nonempty and a
multiple of 16. Length, padding, source/key, range, budget and media validation
failures are distinct. Errors retain resource/input/epoch/sequence context and
safe diagnostics. Original source/media causes require explicit `raw_cause()`.

TS validation checks every 188-byte packet's framing, error/scrambling bits,
adaptation bounds and presence of a PAT packet. fMP4 validation checks complete
box framing (including trailing bytes and 64-bit sizes), init ftyp/moov/mvex and
existing init codec/config parsing, or complete moof/mdat pairs and required
fragment headers. Sample-protection/group metadata is rejected in this profile.
These are container checks, not full media demux or codec/packet validation.
Matching MAP, sample offsets, codec payloads, timestamps and mux compatibility are
validated by the shared media core in the P4 keyed prepared entry.

CBC provides no authentication. Wrong keys normally fail padding or structure;
a wrong IV may preserve padding, and some corruptions can pass both checks. Do not
expose detailed decrypt errors as a remote decryption oracle or interpret success
as proof of authenticity. The public API does not expose raw unaudited decryptor
callbacks or a padding-only decryption endpoint.

## Complete-resource ranges

Encrypted BYTERANGE defaults to `EncryptedRangePolicy::Reject`. A caller with
provenance that each segment/MAP range is an **independently encrypted and padded
complete resource** may explicitly choose `CompleteResources`. An offset need not
be block-aligned (a complete encrypted resource can begin anywhere in a bundle),
but its ciphertext length must be. Exact range length is checked after reading.
Block alignment, a playlist tag or successful PKCS7 padding is not proof of an
encryption boundary; the core cannot recover such provenance from CBC bytes.
Arbitrary CBC subranges and I-frame range reconstruction are not supported.

## Admission, source ownership and memory

Defaults: **16 MiB per resource, 32 MiB reserved bytes, 2 active resources**.
`with_limits` requires positive limits, a representable per-resource limit and a
byte pool large enough for one maximum-size resource. Admission reserves a known
range length or the entire per-resource cap before any read starts. Reservations
cover reading, waiting for a key, decryption and validation. Full budgets return
`BudgetExceeded` without queuing more work; callers wait for active work to finish
before retrying. P2 key limits remain independently enforced.

Each read requests an isolated, demand-driven `Source` session with a byte cap;
only isolated sessions receive `stop_session` on completion/drop. Built-in HTTP
checks announced length and incremental body growth. Built-in file reads reject
oversized files before loading and seek/read only requested ranges, also bounding
file growth after the metadata check. `MemorySource` shares immutable backing
maps between sessions and checks/slices before copying. Its stored corpus remains
caller-owned memory. Custom `Source` implementations must honor
`SourceSessionOptions::max_resource_bytes` **during** reading and must not use a
URI-only stale cache for mutable encrypted resources. The returned Vec is checked
again, but this cannot undo allocations already made inside a noncompliant source.

Cancellation races both read and key waits, cancels the operation-owned KeySession,
drops its source lease and releases capacity. Dropping one read only releases that
read's waiter; other coalesced waiters continue. Late Promise results cannot return
clear bytes or reinsert keys after operation cancellation. Synchronous decryption
and validation check cancellation before/after their bounded work; they do not
promise to yield within an individual AES call on the browser event loop.

`ClearResource` owns a zeroizing buffer, exposes bytes only by explicit access and
omits bytes from Debug/serialization. Capacity is released when `read` completes;
caller-retained clear resources, source/host allocations, demux copies, output
buffers and classic MP4 indexes are outside these admission counters. This is not
a total RSS cap. Public `read` does not cache resources. The P4 adapter retains
one MAP per input and checks its complete descriptor and current key resolution
before reuse. The result retains its immutable descriptor, selected key
reference/version, resolution revision and IV. Caller caches must also include
operation scope and full descriptor/version; never cache clear bytes by URI alone.

## Dependencies and evidence

P3 uses pinned RustCrypto `aes 0.8.4` / `cbc 0.1.2` with cipher 0.4 and zeroize.
This retains the P0 tested combination, rather than asserting it is the latest.
Both crates use MIT OR Apache-2.0 licensing. AES and CBC are delegated to these
libraries; no AES round function is implemented here. Owned secret/plain buffers
and enabled library key schedules are wiped on drop, without a guarantee for JS,
host, register or provider copies.

Reference review: [AES manifest](https://raw.githubusercontent.com/RustCrypto/block-ciphers/aes-v0.8.4/aes/Cargo.toml),
[CBC API and authentication limits](https://docs.rs/cbc/0.1.2/cbc/),
[RustCrypto block ciphers](https://github.com/RustCrypto/block-ciphers),
[RFC 8216](https://www.rfc-editor.org/rfc/rfc8216.html) and
[RustSec advisory database](https://github.com/RustSec/advisory-db).
The scoped advisory inventory and runtime verification are recorded in
[Archived dependency review and verification](release-0.6.0-evidence.json); they are not a cryptographic security audit.

`tests/aes128_resource.rs` exercises independent retained media, rotation/NONE,
MAP/sequence IVs, ordinary/range resources, malformed length/padding/container,
wrong key/IV, cancellation/drop, concurrent coalescing, caps, file range seeking
and redaction. A NIST CBC vector and independently padded OpenSSL vector are unit
tests. [Fixture provenance](../tests/fixtures/crypto/README.md) records public keys,
commands, hashes and clear-source origins. [Runtime regression](runtime-tests.md)
compares native, actual Node/WASM and real Chrome using asynchronous key Promises.
