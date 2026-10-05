# v0.4.0 / v0.4.1 validation

Measured 2026-09-30 on macOS 27.0.1, arm64, Rust 1.98.1, release profile.
Each RSS measurement runs in a fresh test process, using `/usr/bin/time -l`.
The scanner reads init/moof metadata, skips mdat payload, and retains sample
indexes. The muxer writes faststart tables, then copies payload in 1 MiB blocks.

## Reproduce

```sh
python3 scripts/benchmark_finalize.py
python3 scripts/verify_media.py
cargo test --offline --lib sparse_large_mp4_ffprobe_seek -- --ignored --nocapture
```

The media script requires FFmpeg/FFprobe and the default Source feature. The
benchmark uses `--no-default-features` and requires Python plus `/usr/bin/time`;
on Linux it reads `time -v` statistics. The benchmark checks that fixed-count
payload growth adds less than 16 MiB RSS and that index growth remains below
512 bytes per additional sample, allowing for allocator/platform noise.

`python3 scripts/benchmark_finalize.py --large` additionally copies 5 GiB of
synthetic media. This can require over 10 GiB of temporary logical disk space.
These manual cases stay out of ordinary CI; CI runs the small independent media
validation. The >4 GiB structural test creates a sparse file, so physical disk
use depends on the filesystem's sparse-file support.

## RSS and file I/O

Inputs contain synthetic zero payload, with one sample per fragment. The setup
uses seeks rather than payload buffers. This isolates finalize memory and I/O;
it does not model codec decoding, real network performance, or download memory.
Total finalize time below includes scanning, layout construction, and copying.
Throughput covers the layout/copy phase; writes are flushed, without `SyncAll`.
Filesystem cache and concurrent activity affect timing.

| Samples | Payload | Peak RSS | Scan | Total finalize | Copy MiB/s | Logical temp disk | Allocated temp disk¹ |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 128 | 32 MiB | 3.03 MiB | 0.0012 s | 0.0197 s | 1731 | 64.02 MiB | 64.03 MiB |
| 128 | 256 MiB | 3.77 MiB | 0.0006 s | 0.1755 s | 1464 | 512.02 MiB | 512.02 MiB |
| 128 | 1024 MiB | 3.72 MiB | 0.0006 s | 1.2665 s | 809 | 2048.02 MiB | 2056.02 MiB |
| 1,024 | 4 MiB | 3.05 MiB | 0.0054 s | 0.0106 s | 774 | 8.14 MiB | 8.14 MiB |
| 16,384 | 64 MiB | 6.06 MiB | 0.0740 s | 0.1561 s | 779 | 130.19 MiB | 130.20 MiB |
| 131,072 | 512 MiB | 20.06 MiB | 0.6852 s | 1.7111 s | 499 | 1041.52 MiB | 1050.43 MiB |

¹ Sum of input and output `stat` block counts × 512 at completion. Sparse
allocation reporting varies by filesystem; logical input/output sizes are
reported separately in the script's JSON results.

Fixed sample count increased payload 32× without proportional RSS growth.
Increasing sample count increased RSS, as expected for metadata retained in
memory. This supports payload-independent finalize buffering, not constant
memory for arbitrarily long videos. Download memory still includes the current
segment, demux/mux buffers, and configured prefetch slots; a configurable single-resource limit is available in v0.4.1; a shared byte budget remains future work. The bytes API and batch `Mp4` retain their memory-output
semantics.

## Media and large-file correctness

- Generated six seconds of continuous H.264 B-frame video at 30 fps and AAC at
  48 kHz, split into three TS segments and separately into three externally generated fMP4
  segments. Both input modes passed. FFprobe matched all 180 video samples and
  283 audio samples between the fragmented intermediate and Native output:
  track time bases, DTS, PTS, duration, size, and SHA-256 payload hashes.
- FFmpeg decoded the complete Native output and decoded after a seek to three
  seconds. The output's top-level boxes are `ftyp`, `moov`, `mdat`.
- The manual sparse test wrote a 4,404,038,000-byte classic MP4. FFprobe sought
  to a sample at byte 4,373,629,296, confirming readable 64-bit offsets beyond
  `u32::MAX`. This uses synthetic payload and validates structure/seeking, not
  large-file decoding. Small real-media decoding is tested separately above.
- Metadata tests cover `stco/co64`, both mdat header widths, duration header and
  edit-list versions, layout growth that promotes a second track to co64,
  timeline arithmetic overflow, corrupt boxes/ranges/sequences, and cancellation
  between copy blocks. Guarded reads fail if scanning touches media payload.
- `tests/fixtures/v030_prefix.fmp4` and the two v030 checkpoint JSON files were
  produced by the released v0.3.0 source at commit `8cde2e3`. The serde test
  resumes downloading and independently retries Native finalize from these
  artifacts without changing schema v1.

Native finalize preserves the timing/configuration stored in its partial file.
v0.4.1 adds multi-trun parsing, NAL-prefix conversion and native timing preservation. Discontinuities remain unsupported. Individual sample sizes
remain 32-bit, as required by sample tables; individual in-memory fragments
must also fit their signed trun data offsets and the platform address space.

All required feature combinations, all-features with FFmpeg 9, wasm check,
fmt and Clippy were verified locally. Cross-platform runs are delegated to the
existing Linux/macOS/Windows CI matrix. Publication is not part of this change.

## v0.4.1 prefetch measurements and budget design

Run `python3 scripts/benchmark_prefetch.py` with local TCP listening allowed.
Fresh release processes serve 32 synthetic resources on loopback, with a per-resource
limit equal to the payload size. Concurrency 1 retains one serial body; concurrency 4
admits the current 12-slot prefetch window. Wait for server writes and then 50 ms.

| Resource bytes | Concurrency | Admitted payload estimate | Peak RSS bytes |
| ---: | ---: | ---: | ---: |
| 1,048,576 | 1 | 1,048,576 | 10,223,616 |
| 1,048,576 | 4 | 12,582,912 | 29,458,432 |
| 8,388,608 | 1 | 8,388,608 | 19,644,416 |
| 8,388,608 | 4 | 100,663,296 | 58,589,184 |

These are macOS arm64 observations, not hard RSS bounds. The payload estimate uses
completed server writes rather than an atomic snapshot of ready slots. Socket/body
reads may still be in flight, and synthetic repeated bytes can benefit from OS memory
compression. Peak RSS includes the server, runtime and transport. The consumer,
init cache, demux buffers and one TS lookahead segment add memory beyond ready slots.
With finite cap L, each response body is at most L and ready-slot bodies are bounded
by approximately 3 * concurrency * L. Defaults do not impose a byte bound.

A subsequent shared byte budget should reserve credits before admitting each fetch,
including init, consumer-created requests and ready/in-flight prefetch bodies. Known
lengths reserve upfront; unknown lengths acquire additional credits before retaining
each chunk. Eviction, consumption, failure and cancellation release credits via RAII.
The consumer's next resource gets priority and reserved headroom, so workers cannot
fill the budget and deadlock it. Resources exceeding the configured budget fail before
waiting; unknown-length growth must fail or spill, rather than hold partial credits
while all workers wait for more. Slot and byte permits jointly bound scheduling, and
cancellation wakes permit waits. Explicit ownership/transfer avoids charging a body
twice while it moves from a slot into processing. This design is not an implemented
runtime option in 0.4.1.

The expanded media verifier covers nine inputs: regular and VFR TS; regular, negative
CTS, VFR, delayed audio, one/two-byte multi-run AVC fMP4; HEVC fMP4. Each compares the
input directly against fragmented, Native and batch output (counts, rational timing
within one destination tick, normalized NAL/AAC payload), followed by FFmpeg decoding
and seeking. Released v0.4.0 checkpoint fixtures complement the v0.3.0 fixtures.

## v0.5.0 prepared-session bounds

New sessions disable autonomous Reqwest prefetch. The scheduler issues at most
`min(max_in_flight_reads, input_count)` reads, default two in total. Each input
holds at most a current and one lookahead segment during normal execution;
preparation retains up to `max_probe_segments_per_input` segments (default two).
A blocked writer prevents new reads. Demuxed samples, normalized NAL copies and
one assembled fragment add a constant number of segment-sized buffers. This is
a segment-count bound, not a strict byte bound or a total process-memory claim.

Playlist metadata grows with segment count. Classic bytes output retains the
samples and final result. Native file finalize keeps per-sample metadata plus
a 1 MiB payload copy buffer. `write_mfra=false` (new default only) retains no
per-fragment random-access entries; enabling it adds O(fragment count) entries.
Custom Sources own their read-time allocations and must respect demand-driven
session settings, stop/drop semantics and configured resource caps.

`tests/prepared_session.rs` compares short and long synthetic operations with
both mfra settings, records downloaded-minus-processed segment windows and
maximum pending write size, and uses controlled Source futures to measure active
reads and verify cancellation/drop cleanup. It does not substitute RSS for
these lifecycle and queue assertions.
