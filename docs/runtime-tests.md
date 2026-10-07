# CI and runtime verification

The repository's `tests/runtime/` crate binds the production parser, crypto,
Engine and compatibility APIs to a test-only WASM module. Native Rust, actual
Node WASM and real Chrome compare output hashes, reports and lossless integer
transport. The host bridge covers key Promises, synchronous reentry, rejection,
late settlement, cancellation and writable backpressure.

Run from the repository root:

```sh
cargo test --locked --manifest-path tests/runtime/Cargo.toml --target-dir target --lib
cargo fmt --manifest-path tests/runtime/Cargo.toml -- --check
cargo clippy --locked --manifest-path tests/runtime/Cargo.toml --target-dir target --all-targets -- -D warnings
python3 scripts/verify_runtime.py --browser
python3 scripts/verify_engine_recovery.py
python3 scripts/verify_package.py
```

Install `wasm32-unknown-unknown`, Node and wasm-bindgen CLI 0.2.100 first. Browser
checks need Chrome/Chromium on macOS/Linux; `HLS_TEST_CHROME` selects the executable.
Independent media checks also require FFmpeg and FFprobe. FFmpeg-backed Cargo
builds need the FFmpeg 9 development libraries and pkg-config.

The CI feature matrix covers the default source, serde, no-default-feature and
experimental GCM combinations on Linux, macOS and Windows. macOS additionally
runs the FFmpeg backend and independent packet, decoded-frame and subtitle checks.
The recovery corpus interrupts every checkpoint, compares recovered files with
complete execution and checks split-output publication. Test-only I/O hooks also
inject file errors and real process exits; they are absent from production builds.

Package verification runs `cargo publish --dry-run`, inspects the archive,
validates documentation links, and builds and tests extracted native/WASM
examples. Scripts remain in the Git checkout and operate on the extracted crate.
Development reports, CI configuration and the nested runtime crate are excluded
from the package. Schema-1 compatibility is checked against artifacts generated
from the preserved baseline commit, rather than recreated by the current encoder.

Logs, measurements and JSON evidence live under `target/` and are uploaded by CI.
The publish workflow depends on the complete CI workflow for the release tag,
then verifies package name/version, tag identity and a clean checkout before
publishing.
