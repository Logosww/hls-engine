# v0.6.0 release verification

Local acceptance completed on 2026-10-05 on macOS for package version 0.6.0.
The results below were recorded before the release commit and publication.
CI runs these checks separately on Linux/macOS/Windows; the tables here describe
the local verification evidence.

## Delivered scope

Finite typed playlists, lossless u64 metadata transport, operation-scoped key
providers, bounded AES-128 resource decryption, encrypted MAP handling and keyed
prepared output through the existing media core. TS/fMP4, AVC/HEVC/AAC-LC, clear/AES
mixtures, single inputs and replacement external audio are covered. Diagnostics,
stage counters and declarative capability queries are separate public contracts.

Legacy APIs and schema v1 checkpoints remain compatible. Live/EVENT, presentation
ranges, sample encryption, arbitrary encrypted byte slices, general multi-track,
subtitles and keyed resume remain unsupported. CBC is unauthenticated; passing
padding/container checks does not prove authenticity. Queries describe supported
combinations, not arbitrary media validity or player compatibility.

## Acceptance results

| Check | Local result |
| --- | --- |
| Default / serde | 191 / 195 passed; 3 ignored in each run |
| No default / no default + serde | 160 / 164 passed; 3 ignored in each run |
| All features, including FFmpeg | 196 passed; 3 ignored |
| fmt, Clippy all-features and minimal+serde, rustdoc | Passed |
| Clear media corpus | 14 inputs, 42 successful outputs; independent packets, decode and seek |
| Legacy dual input | 48 outputs; packets, payloads, DTS/PTS and both finalize backends |
| AES fixtures | 51 files, 8 cases, 20 independent OpenSSL decryptions, 8 FFmpeg decodes |
| Keyed integration | 64 independently inspected/decoded outputs; 96 portable combinations in the shared harness |
| Native SDK example | 3 committed segments; decode and seek passed |
| P0–P5 actual Node/WASM | Native semantic agreement; P1 additionally checks lossless archives and JS round trips |
| P0 and P2–P5 real Chrome | Native/Node/browser agreement; cancellation, Promise rejection and late-result cases passed |
| Legacy WASM session | Mixed inputs, bytes/writer, timeline and cancellation passed |
| Publication dry-run | Package verification passed; upload aborted by dry-run |
| Extracted package | Required guides and native/WASM examples present; both examples compile |
| Extracted WASM execution | 2 successful key requests, 275048 MP4 bytes, 17 callback events, 3 provider failure cases, 0 unhandled rejections |

Rust test totals include doctests. The three ignored items are the manual RSS
benchmark, the >4 GiB sparse-file seek test and an illustrative archive doctest;
they are not counted as passes. Node and real-browser results are recorded
separately in [machine-readable evidence](release-0.6.0-evidence.json). Prototype
experiments for future versions do not expand the delivered scope above.

## Reproduce

Run from the repository root:

```sh
cargo test --locked
cargo test --locked --features serde
cargo test --locked --no-default-features
cargo test --locked --no-default-features --features serde
cargo test --locked --all-features
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo clippy --locked --all-targets --no-default-features --features serde -- -D warnings
cargo doc --locked --no-deps --all-features
python3 scripts/verify_media.py
python3 scripts/verify_multi_input.py --ffmpeg
python3 scripts/verify_crypto.py --openssl --ffmpeg
python3 scripts/verify_keyed.py --ffmpeg
python3 scripts/verify_keyed_examples.py
python3 scripts/verify_package.py
```

FFmpeg checks require FFmpeg 9 development libraries and executables. Package
verification needs registry access, wasm32-unknown-unknown, Node and wasm-bindgen
CLI 0.2.100. Use `--allow-dirty` on `verify_package.py` for an uncommitted checkout.
The script performs a publication dry-run, explicitly creates a fresh `.crate`,
checks its contents and executes the example from an extracted copy. A stale
archive left by a prior dry-run is never accepted as the current artifact.

The phase results above are the original acceptance record. The prototypes were
retired on 2026-10-05 after consolidating the production regressions into
`tests/runtime/`. M0 decisions and phase evidence are archived under
`docs/planning/`; current rerun commands are in [runtime tests](runtime-tests.md).
The repository's CI now runs that suite in native Rust, Node/WASM and real Chrome.

## Documentation and SDK handoff

Public guides live in [docs](README.md) and are included in the crate. Historical
requirements and plans live in `docs/planning/` and are excluded from the package.
The WASM adapter is the root example `examples/keyed_wasm.rs`; the runtime suite
imports that exact source, and the package check also executes its own generated
bindings. Native usage is `cargo run --example keyed_demo -- input.m3u8 output.mp4`.

Downstream SDK integration may adopt the finite non-resumable AES-128 path.
Legacy clear resume remains available; the downstream AES resume acceptance
condition still depends on v1.0 and is not closed by this release.
