# v0.9.0 — open input and continuous sessions

Candidate implementation based on v0.8.0 / `8d6c54f`. Local acceptance is on
macOS arm64, 2026-10-06. Registry publication and SDK product rollout are separate.

`ContinuousSession` adds atomic typed-snapshot admission, rolling reconciliation,
independent input EOF/restart, stop/drain, cancellation and VOD pause. Its runner
writes before ENDLIST, reuses operation-scoped key/resource sessions, and supports
selected clear/AES-128/TS SAMPLE-AES/fMP4 cenc/cbcs input. A primary may have one
selected replacement audio input. Output is continuous fMP4, explicitly bounded
memory fMP4/classic, or native temporary fMP4 with Native/optional FFmpeg classic
finalization. Ordered events carry incremental mappings and split outputs;
reports retain bounded recent history and cumulative statistics.

See [the API guide](continuous-sessions.md), [Native example](../examples/continuous_demo.rs)
and [WASM example](../examples/continuous_wasm.rs). The finite APIs, public struct
literals, legacy errors, audio replacement semantics and schema-v1 resume remain
unchanged. No persistent checkpoint is added for continuous operations.

## Verification scope

The [machine evidence](release-0.9.0-evidence.json) records exact commands, fixture
hashes, runtime profiles and SDK revisions. Gate results there are authoritative;
only completed runs are marked successful.

- Continuous integration tests exercise open encrypted profiles before ENDLIST,
  atomic admission, duplicate/rewrite/revision/generation handling, retained
  KEY/MAP overlap, explicit restart, empty windows, growing EVENT, queue limits,
  rolling append and independent audio EOF/tail. They cover ranges, duration drain,
  epochs, GAP preserve/collapse/split and configuration split; ambiguity fails.
- Lifecycle tests cover pending key/sink acquisition, slow writer/capacity waits,
  VOD pause acknowledgement, stop/cancel, final flush cancellation, failed fragment
  flush, Drop, no-clobber native publication and Native/FFmpeg finalization.
  A reentrant host waiter verifies no state mutex is held across its callback.
- Thirty-four shared Native/Node WASM/Chrome outputs cover the 17 retained clear
  and sample-encrypted profiles in fragmented/classic memory output. Normalization
  changes only MP4 creation/modification timestamps. Each run uses an open playlist,
  emits media before ENDLIST, then stops and drains. Generation exceeds 2^53.
- Independent FFprobe/FFmpeg verification compares 52 outputs with clear inputs:
  34 sample/clear memory outputs, 12 AES-128 rotation/METHOD=NONE memory outputs,
  and 6 native classic outputs across Native/FFmpeg backends. Decoding uses fatal
  errors, preserves video frame cadence, and trims AAC decoder padding to the
  declared final packet duration. This normalization is in the verification script.
- The real SDK checkout is copied to an isolated harness. Its SourceHost, key
  provider, Promise callbacks and demand writer run eight continuous combinations
  with initial windows, overlapping refreshes, stop/drain and native file output.
  Native/browser results must match. The wrapper owns WritableStream close and
  verifies rejection prevents public completion. Three continuous key-wait cancels
  verify late completions produce no writes; read/key/write Promise failures are
  injected separately. The real SDK checkout remains unchanged.
- The release gate runs default, serde, no-default, no-default+serde, all-features,
  WASM compilation/execution, root/runtime formatting and Clippy. Existing legacy
  compatibility and resume tests are part of the feature matrix. Extracted package
  tests build Native and WASM examples and execute the WASM example in Node.

## Memory evidence and limits

The deterministic long-run probe records 8, 64 and 256 repeated AAC resources,
each in a new explicit epoch, with two queue slots and four history entries. A
short-writing non-Send sink deliberately yields. It measures allocator peaks and
retained bytes, first-write latency, core queue/sample peaks, WASM memory pages,
and JS heap for each scale. It excludes caller-owned input snapshots and output
collection. Native allocator peak is 124,647 bytes above baseline at all three
scales in the recorded run; sample payload is 24,279 bytes, sample count 94, queued
descriptors one and retained mappings four. Total allocations grow with work.

WASM pages are high-water memory and need not shrink. JS heap includes runtime,
fixture and Promise allocations and is GC-dependent. These measurements establish
bounded core state under a controlled workload; they are accelerated short tests,
not multi-hour or production-source benchmarks. Classic output still builds a
recording-length sample index; memory output retains recording-length bytes and
conversion copies. `collected_bytes` and `classic_index_samples` distinguish these
costs from the bounded live pipeline.

## Explicit boundaries

No LL-HLS partial segments, automatic rendition changes, multiple audio tracks,
subtitles, Packed AAC, GCM, new persistent checkpoint or package rename. Existing
v0.8 sample-protection restrictions continue. Full EVENT snapshots may themselves
grow outside the core's admission budget. Evicted identity history cannot fully
detect old rewrites, but never re-executes an accepted sequence.

Encrypted/MAP windows without retained overlap require explicit restart;
unannounced missing slots cannot infer a duration. GAP skip is explicit and does
not hide network/key/demux failures. Cross-input reset or gap collapse needs common
time evidence. Probe/first-output bounds are counts and bytes, not a latency SLA
for a stalled source. Duration limits drain accepted work and may overshoot.

Native files publish without replacing existing destinations, using same-directory
hard links. Failed/cancelled temporary outputs are removed; earlier completed
split outputs remain. Borrowed writers are flushed, never closed/aborted by the
core. The SDK's public completion must follow successful sink close. Events are
not recovery checkpoints; this minor offers operation-local pause/restart only.
