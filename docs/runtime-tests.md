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

The development timeline suite compares clear/AES-128 range output hashes,
mapping reports and lossless integer transport. It also runs the same 12
near/far/full-range/long-GOP budget cases on Native, Node WASM and Chrome;
[planning-state measurements](benchmarks.md) distinguish retained records from
total memory and quantify reads before the first output write.

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

Downstream compatibility can be checked with
`python3 scripts/verify_sdk.py /path/to/hls-downloader`. It copies the SDK's Rust
adapters and fixtures into `target/sdk-compat`, patches Cargo to this crate, runs
the SDK's native prepared/resume tests, and checks the native workspace and
browser WASM adapter. It also installs the tracked timeline integration extension
into the isolated SDK copy and compares nine native/real-Chrome timeline cases
through the SDK host and Promise bridges. The SDK checkout stays unchanged.

Timeline runtime runs include 15 deterministic cursor-budget cases plus actual
allocation, first-write, WASM-page and JS-heap measurements. Timing/allocation
measurements live outside the cross-runtime semantic equality report.
