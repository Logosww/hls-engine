# v0.8.0 — finite sample decryption

The existing keyed and timeline APIs accept TS SAMPLE-AES AVC/AAC-LC and fMP4
cenc/cbcs AVC/HEVC/AAC-LC without an opt-in flag. Decryption precedes codec checks,
keyframe detection and NAL normalization. Output reconstructs clear sample entries
and preserves codec payloads and timing. Public resource-only APIs continue to
mean clear/AES-128; legacy checkpoints, writer ownership and audio replacement
semantics are unchanged.

See [the supported protection profile](sample-encryption.md) for box versions,
flags, offsets, IV rules and exclusions. `AvailableKey::sample_aes` and
`sample_aes_ctr` use the existing operation key session, including KID binding,
TTL, invalidation, request coalescing and cancellation. `SampleError` adds typed
resource/track/sample/key context with redacted default formatting.

## Verification

Local candidate, macOS arm64, 2026-10-06. This record does not imply a registry
publication, tag, commit or SDK product rollout.

- Independent Shaka Packager v3.9.3 media covers AVC/HEVC/AAC cenc/cbcs and AVC/AAC
  TS. OpenSSL supplies 1/2-byte NAL-prefix, multitrun CTR fixtures and whole-resource
  CBC rotation fixtures. All 98 retained files and original source hashes are
  recorded in the fixture manifest. NIST SP 800-38A CBC/CTR vectors validate the
  cipher implementation independently of fixture generation.
- Twenty-one shared native/Node WASM/real Chrome cases compare complete normalized
  output hashes, timestamps and reports. Normalization changes only MP4
  creation/modification timestamps. FFprobe independently parses 40 clear outputs;
  FFmpeg decodes all 40 with fatal decoding errors enabled, including both native
  and FFmpeg finalizers and the existing output formats.
- Ten sample integration tests cover auxiliary-only/inline-only metadata, KID
  overrides, budgets before provider calls, mixed schemes with external audio,
  METHOD=NONE/AES-128/sample rotation, clear samples without key requests, ranges,
  epoch resets, MAP changes, configuration splits and cancellation. A same-URI
  key replaced after TTL expiry with unchanged ciphertext produces ResourceChanged.
- Parser vectors cover versions, bounds, counts, offsets, conflicting auxiliary
  descriptions, track/fragment seig namespaces, IVs and clear overrides. Algorithm
  vectors cover CTR continuation across subsamples, CBC pattern/reset/trailer,
  AVC short NALs, the 48/49-byte boundary and encryption-layer emulation prevention.
- The real hls-downloader checkout is copied into an isolated harness. Its original
  prepared/resume tests and native/WASM adapters compile against this crate.
  Sixteen timeline/sample cases produce equal native/Chrome reports and output
  hashes using the real SourceHost/provider/Promise bridge plus a tracked sample
  method/KID extension. Three additional Chrome cases cancel cenc, cbcs and TS
  key waits; late Promise completions write no output and cause no unhandled
  rejection. The SDK revision and upstream source digest are in the evidence.
- Full all-feature and no-default/serde regressions, Clippy, formatting and WASM
  compilation pass. Three pre-existing manual/ignored tests remain opt-in.
  Publication dry-run and extracted-package verification pass, including packaged
  examples, sample/timeline tests and an actual Node WASM adapter.

## Memory and limits

Default per-resource raw sample count is 65,536. Container sample/group parsing
also has a 65,536-entry defensive ceiling. Resource and waiting-byte budgets are
checked before retained raw sample copies and across key waits. Collect and
Replay release their buffers per resource; finite catalogs and MP4 output indexes
still scale with input length.

The profile separately records peak raw and replay payload capacities per resource:
70,734 / 91,019 / 71,134 bytes for AVC cenc / HEVC cbcs / TS AVC, respectively, in
each phase. These are retained payload capacities, excluding metadata and temporary
copies. TS removal can shorten lengths without shrinking vector capacity.
The counting allocator independently records combined allocations including those
costs, not total process RSS. Native peak increments are 304,176 / 386,698 / 302,582
bytes, with zero retained increment. Native release first writes were 6.93 / 4.56 /
8.55 ms. Node/Chrome debug WASM measurements are retained separately and are not
comparable benchmarks. The runtime records WASM linear memory before/after and JS
heap snapshots, not per-operation JS peaks. See the
[machine-readable evidence](release-0.8.0-evidence.json) for individual measurements.

Collapsing presentation holes across reordered pictures can create non-monotonic
DTS. Such timelines remain rejected for both clear and encrypted inputs; preserving
the holes supports the tested AVC-to-HEVC configuration split. CBC/CTR have no
authentication: incorrect keys are not guaranteed to be detected. This is a bounded
container profile, not arbitrary CENC/DRM support. Live/EVENT, Packed AAC, multiple
tracks, subtitles, new recovery and GCM remain outside this release.

## Reproduction

```sh
cargo test --offline --all-features
cargo test --offline --no-default-features --features serde
cargo clippy --offline --all-targets --all-features -- -D warnings
cargo fmt --all --check
cargo check --offline --no-default-features --features serde --target wasm32-unknown-unknown
python3 scripts/verify_sample_crypto.py --ffmpeg --ffmpeg-finalize
cargo run --offline --release --example sample_budget
python3 scripts/verify_runtime.py --browser
python3 scripts/verify_sdk.py /path/to/hls-downloader
python3 scripts/verify_package.py --allow-dirty
```

AES 0.8.4, CBC 0.1.2 and CTR 0.9.2 are pinned in the cipher 0.4 family with
zeroization support. Dependency/source review and WASM verification are not a
cryptographic audit or a blanket guarantee that the complete dependency graph
has no advisories. Fixture generation commands, tool versions, license and SHA-256
values live in `tests/fixtures/sample_crypto/manifest.json` in the package.
