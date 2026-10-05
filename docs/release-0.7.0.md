# v0.7.0 — finite ranges, epochs and split outputs

Version 0.7.0 adds `prepare_hls_timeline` and a separate timeline capability query.
Requests use half-open presentation ranges; output expands to independently
verified access points and reports preroll/postroll, MAP/key dependencies,
source/public/output clocks and ordered epoch mappings. Clear and AES-128 TS/fMP4,
external audio, explicit anchors/PDT, gap preservation/collapse and decodable
configuration/gap splitting share the same core on native and WASM.

Legacy prepared/keyed APIs, external-audio replacement, writer ownership and
checkpoint schema v1 remain compatible. No existing task is silently migrated.
Timeline reports are diagnostic transport, not checkpoints. Internal raw sample
and Packed AAC hooks validate asynchronous ordering and original byte layout;
they do not advertise SAMPLE-AES, cenc/cbcs or Packed AAC execution. Open inputs,
new persistent recovery, multiple audio renditions and subtitles retain their
later roadmap versions.

## Bounded sample execution

Finite planning keeps only resource-sized sample windows and a compact resource
catalog. Track cursors regenerate timing from hash-verified media, merge decode
order and release consumed resources. Long selections and long GOPs complete
with a fixed sample budget. Catalogs, snapshots, dependency/epoch/gap reports and
classic output indexes are measured separately; total memory is not constant.
Validation finishes before sink acquisition. Consequently first output includes
finite scanning and validation latency. The full allocation/latency evidence is
in [benchmarks.md](benchmarks.md); the API contract is in
[timeline-sessions.md](timeline-sessions.md).

## Verification

Local platform: macOS arm64, 2026-10-05. These checks build and inspect the local
0.7.0 candidate; no registry publication, tag, push or hosted release is implied.

- 39 timeline integration tests cover ranges, B-frame/negative CTS timing,
  TS wrap/reset/ambiguity, EOF tails, clear/AES MAP and key rotation, dual-input
  gaps, split acquisition/writes/flush/finalization failures, cancellation/drop
  across all read stages, lossless integer transport and legacy capabilities.
- 148 independent outputs pass FFprobe packet/payload/timestamp comparison and
  FFmpeg decoding, including Native and optional FFmpeg finalizers, gaps and
  AVC/HEVC configuration changes.
- Native, actual Node WASM and real Chrome compare output hashes and timeline
  reports. Fifteen shared budget cases measure near/far/full/long-GOP execution
  and resource-window rejection. Requested allocator bytes, first-write latency,
  WASM linear memory and JS heap are recorded separately from semantic equality.
- The real downstream SDK's prepared/resume tests and native/WASM adapters are
  checked in an isolated copy. The tracked timeline extension exercises its
  existing host/provider/Promise bridges in nine native/Chrome cases with equal
  report JSON and canonical output hashes. This does not modify or release the
  downstream application.
- Cargo feature combinations, all-target Clippy, WASM compilation, runtime crate
  tests/lint/format, legacy checkpoint fixtures and packaged examples are covered
  by the commands below. Three pre-existing manual/ignored tests remain opt-in.

```sh
cargo test --offline
cargo test --offline --features serde
cargo test --offline --no-default-features
cargo test --offline --no-default-features --features serde
cargo test --offline --all-features
cargo clippy --offline --all-targets --all-features -- -D warnings
cargo check --offline --target wasm32-unknown-unknown --no-default-features --features serde
cargo fmt --all -- --check
python3 scripts/verify_timeline.py --ffmpeg
cargo run --offline --release --example timeline_budget
python3 scripts/verify_runtime.py --browser
python3 scripts/verify_sdk.py /path/to/hls-downloader
python3 scripts/verify_package.py --allow-dirty
```

The [machine-readable evidence](release-0.7.0-evidence.json) records the measured
scope and commands. SDK application rollout remains a downstream release task;
its timeline integration fixture is delivered with this crate.
