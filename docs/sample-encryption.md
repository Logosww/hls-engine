# Sample encryption

Implemented in v0.8.0; see the [release evidence](release-0.8.0.md) for verified
combinations. Existing keyed and timeline entry points select the
sample path automatically. Legacy and public resource-only APIs keep their
previous clear/AES-128 contracts.

Version 0.9 connects these same protection profiles to selected Live/EVENT through
[continuous sessions](continuous-sessions.md); finite entry points retain their
finite-input semantics.

| Container | KEY method | Scheme | Codec | IV |
| --- | --- | --- | --- | --- |
| TS | SAMPLE-AES | Apple sample CBC | AVC, AAC-LC | KEY IV or original media sequence |
| fMP4 | SAMPLE-AES | cbcs | AVC, HEVC, AAC-LC | 16-byte sample or constant IV |
| fMP4 | SAMPLE-AES-CTR | cenc | AVC, HEVC, AAC-LC | 8/16-byte sample IV |

`PlaylistSnapshot::validate_finite_sample_vod` and `validate_timeline_sample_vod`
provide sample-profile preflight. The original resource-only validators retain
their clear/AES-128 meaning.

Keys are 16 bytes. Use `AvailableKey::sample_aes` / `sample_aes_ctr`, binding
`request.resource().kid()` when present. Provider transport and retry remain
caller responsibilities. CBC/CTR are unauthenticated and cannot identify every
incorrect key. The library does not implement license exchange or CDM access.

## Container profile

- Protected sample entries: encv/enca with unique sinf/frma/schm/schi/tenc;
  schm v0, flags 0, scheme version 0x10000; tenc v0/v1, flags 0.
- senc v0, flags 0 or subsample flag 2; algorithm override flag is unsupported.
- saiz v0 and saio v0/v1, flags 0/1; auxiliary type and parameter must match.
  saio supports one offset for a fragment's samples or one per trun. Offsets use
  the same explicit tfhd base or default-base-is-moof as sample data. External
  resources and implicit cross-traf bases are unsupported.
- sgpd(seig) v0/v1/v2 and sbgp(seig) v0/v1, flags 0; grouping parameter 0.
  Track and fragment descriptions are separate namespaces. Index 0 uses the
  applicable default; indexes above 0x10000 select fragment descriptions.
- Counts, sizes, offsets, IVs and exact subsample coverage are checked. Duplicate
  descriptions, missing auxiliary location pairs and contradictory senc/saiz/saio
  fail closed. Unknown schemes/versions/flags return unsupported.

Protection is resolved before codec inspection, keyframe detection and NAL
normalization. TS encryption-layer emulation prevention is removed separately
from the original clear NAL byte layer. Mux reconstructs clear sample entries.

## Limits and diagnostics

`ResourceOptions::with_sample_limit` caps samples retained for one resource
(default 65,536); container sample/group tables also have a 65,536-entry
defensive ceiling. Resource/key limits retain their existing defaults. Resource
and sample buffers, finite catalogs, output indexes and host memory are distinct
costs; these limits are not a total RSS guarantee. Waiting for a key is cancellable.
`ResourceStats` and timeline report accessors expose separate peak raw/replay
payload capacities per resource; they exclude metadata, temporary copies and
allocator overhead. These host-memory observations are outside the stable timeline
JSON wire representation.

`SampleError` is available through keyed/timeline error accessors. It retains
resource identity, a sample/track when known, scheme, selected key reference and
version, and explicit underlying error access. Default formatting is redacted.
A parser failure before sample identification can have no sample/track context.

Live/EVENT, TS HEVC SAMPLE-AES, TS SAMPLE-AES-CTR, Packed AAC, GCM and persistent
sample-session recovery are excluded. Complete resource ranges require the
existing caller attestation; arbitrary ciphertext slices and I-frame ranges
remain unsupported.

References: [Apple sample encryption](https://developer.apple.com/library/archive/documentation/AudioVideo/Conceptual/HLS_Sample_Encryption/Encryption/Encryption.html),
[W3C ISO common encryption](https://www.w3.org/TR/eme-stream-mp4/).
