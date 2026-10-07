# Memory and recording costs

Fragmented Engine output is bounded by configured admission, resource, sample,
probe, history and metadata limits. It does not keep a whole-recording sample
index. A slow output writer stops new reads and applies backpressure to every
selected input. `into_bytes` additionally retains the complete output up to its
explicit capacity.

Native classic MP4 finalization reads disk staging and constructs the index
needed for seeking. Its index grows with sample count, including subtitle
samples. Source resources, codec configuration and mux copies also consume
memory; a queue counter alone is not a total process-memory measurement.

Schema-2 checkpoints retain the active replay window and completed-child
identities. Their size is bounded by the metadata limit. Split ledgers grow with
the number of completed outputs; reaching that limit returns `BudgetExceeded`
and preserves completed children. Recovery verifies the committed prefix with a
fixed read buffer rather than loading the entire output.

## Reproduce measurements

Run from the repository root with release optimization:

```sh
cargo run --locked --release --features serde --example engine_budget
HLS_ENGINE_BUDGET_GCM=1 cargo run --locked --release --features serde,experimental-gcm --example engine_budget
cargo run --locked --release --features serde --example multitrack_budget
cargo run --locked --release --features serde --example continuous_budget
cargo run --locked --release --example timeline_budget
```

The Engine harness measures 64/512/4096 segments with one or three selected
inputs, interruption and restoration, checkpoint size, first resumed checkpoint
latency, active allocations and classic index growth. The GCM variant rotates
provider key versions across epochs using independent synthetic fixtures.

The runtime harness repeats long multi-input recordings in native Rust, Node
WASM and Chrome. Rust allocator peaks, WASM linear memory and JavaScript heap
measure different layers and must not be added or compared as equivalent values.
Read the per-run JSON under `target/` for actual measurements on the current host.
CI uploads these artifacts; elapsed time and allocator overhead are platform dependent.

Legacy timeline planning retains bounded live sample records and per-resource
summaries. It can replay a long GOP from its source without buffering all of its
samples. Planning may read several resources before its first output write; this
is distinct from bounded continuous recording.
