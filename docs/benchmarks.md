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

## Native finalization with insufficient header reservation

Run the manual dense-file benchmark (allow space for two large media files):

```sh
python3 scripts/benchmark_finalization.py
# Shorter run:
python3 scripts/benchmark_finalization.py --sizes-mib 256,2048 --repeats 3
```

The default run compares Copy and InPlace with an insufficient 8 KiB reservation
at 256 MiB, 2 GiB and 8 GiB. The 8 GiB case also tests the default 1 MiB
reservation, which is insufficient for its sample index. All InPlace measurements
assert that media relocation actually occurred. Each strategy gets a newly
written, physically allocated input; no sparse media, reflinks or source clones
are used. The operation order rotates across three repetitions.

This measures the real internal scanner and finalizers in release mode with
16 KiB synthetic samples and 256 samples per fragment. The payload is not valid
AAC; this is an I/O/index benchmark, not a decoder benchmark or production capture.
Output faststart layout and beginning/middle/end sample payloads are checked
outside the timed region. Codec and player-facing correctness are covered by
`scripts/verify_media.py` separately.

Input generation and input `sync_all` are measured separately and excluded from
`finalize_seconds`. Finalization includes scanning, indexing, copying/relocation
and output flush, matching the ordinary file writer's synchronization policy.
`finalize_sync_seconds` additionally includes a measured output `sync_all` call.
`pipeline_seconds` adds synthetic input preparation; it does not measure HLS
network transfer, decryption, demuxing or publication. The OS cache is not purged;
freshly written input is naturally cached to the extent available memory permits.
Do not run compilation or other I/O benchmarks concurrently with measurements.

### Local result: 2026-11-14

Apple M3, 8 GiB RAM, macOS 27.2.1, local APFS, Rust 1.100.0, release build.
Numbers below are seconds, medians of three runs at 256 MiB and six runs at
2/8 GiB. Sizes describe media payload; actual files include container metadata.
Both reservations were insufficient in every reported InPlace run.

| Payload | InPlace reservation | Copy finalization | InPlace finalization | Time change | Copy including sync_all | InPlace including sync_all |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 256 MiB | 8 KiB | 0.115 | 0.026 | -77.5% | 0.124 | 0.104 |
| 2 GiB | 8 KiB | 1.947 | 1.298 | -33.3% | 2.082 | 2.051 |
| 8 GiB | 8 KiB | 13.716 | 13.926 | +1.5% | 13.804 | 14.480 |
| 8 GiB | 1 MiB | 13.716 | 14.007 | +2.1% | 13.804 | 14.282 |

At 8 GiB, Copy ranged from 10.82 to 23.34 seconds and InPlace with the default
1 MiB reservation ranged from 10.79 to 17.07 seconds. These ranges exceed the
median difference: there is no demonstrated stable large-file speed advantage.
Including synthetic input preparation, the respective 8 GiB pipeline medians
were 19.21 and 20.93 seconds; preparation variability makes this a secondary
metric, not an isolated finalizer comparison. The 2 GiB apparent advantage at
flush nearly disappears once output synchronization is included.

The copy path seeks/reads/writes individual samples (16 KiB here), while the
relocating path uses 1 MiB blocks. That can explain some small-file CPU/I/O-call
savings, but this benchmark does not isolate cache, filesystem copy-on-write,
writeback or thermal effects. Do not generalize it to every storage device or
sample distribution. Keep InPlace optional: avoiding a second full file is the
clear design benefit; insufficient reservation still incurs full media movement.

[Raw measurements and environment](https://github.com/Logosww/hls-engine/blob/v1.3.0/docs/benchmarks/native-finalization-2026-11-14.json)
retain both batches. The initial 256 MiB measurements are marked pilot-only
because a brief Clippy compilation overlapped that early group; its table uses
the separate sequential confirmation run. All 2 GiB and 8 GiB observations are
included, with no outlier trimming. No compilation overlapped the confirmation
batch. All measured outputs passed the faststart and sample-probe checks.
