# Runtime regression tests

The repository's `tests/runtime/` crate binds the production parser, key sessions,
resource decryption, keyed prepared core and SDK adapter to one test-only WASM
module. It compares native Rust results with actual Node/WASM and real Chrome.
The root integration tests continue to cover the public APIs directly.

The runtime suite covers lossless playlist archives and JS round trips, provider
coalescing/expiry/invalidation, synchronous reentry, cancellation/abort, late
Promise success/rejection, resource budgets, 96 clear/AES prepared combinations,
stage counters, capability queries and the exact `examples/keyed_wasm.rs` adapter.
The adapter includes rejected, unavailable and wrong-length key responses.

Run from the repository root:

```sh
cargo test --locked --manifest-path tests/runtime/Cargo.toml --target-dir target --lib
cargo fmt --manifest-path tests/runtime/Cargo.toml -- --check
cargo clippy --locked --manifest-path tests/runtime/Cargo.toml --target-dir target --all-targets -- -D warnings
python3 scripts/verify_runtime.py --browser
```

Install the wasm32-unknown-unknown target and wasm-bindgen CLI 0.2.100 first. The
runner requires Node; `--browser` additionally requires Chrome/Chromium on
macOS/Linux. Set `HLS_TEST_CHROME` to select another executable. Node and browser
checks write separate reports under `target/runtime/`; CI uploads both.

This suite runs from the repository and is excluded from the published crate.
For the published WASM adapter, [package verification](release-0.6.0.md) builds
and executes `examples/keyed_wasm.rs` from an extracted crate archive.

Independent output inspection uses the root `keyed_export` example through
`python3 scripts/verify_keyed.py --ffmpeg`. It creates real native outputs for
FFprobe packet/payload/timing comparisons and FFmpeg decode/seek checks.

The earlier phase prototypes were retired on 2026-10-05. Their M0 decisions and
phase evidence are archived under `docs/planning/`; the original v0.6 runtime
results remain in [release evidence](release-0.6.0-evidence.json). Experiments for
future sample encryption, open input or subtitles do not expand the delivered
v0.6 scope.
