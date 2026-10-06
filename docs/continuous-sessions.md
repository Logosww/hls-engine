# Continuous sessions (v0.9)

`ContinuousSession` accepts selected typed Live/EVENT or VOD snapshots while its
caller polls an output future. It starts writing after bounded media probing;
ENDLIST is not required. The SDK owns playlist polling, retry and initial selection.
The core owns reconciliation, decryption, presentation time, backpressure and muxing.

The additive API preserves the finite prepared/timeline APIs, their public struct
literals, error matches, and `with_audio()` replacement semantics. It does not
provide a persistent checkpoint. Complete-resource AES-128, TS AVC/AAC-LC
SAMPLE-AES, and fMP4 AVC/HEVC/AAC-LC cenc/cbcs reuse the v0.8 crypto profiles.
Clear TS/fMP4 support the existing AVC/HEVC/AAC-LC tracks. Track selection is fixed:
a single input, or a primary plus one selected replacement audio input.

## Drive admission and output together

Create `ContinuousInputs::new(ContinuousInput::new(id, source))`, a `KeySession`,
and `ContinuousOptions::default()`, then call `ContinuousSession::new`.
Keep `session.handle()` separately. Run the producer and `session.write_to(&mut
writer)` concurrently on the caller's executor. The runner does not spawn a
background polling thread; native classic file finalization uses a blocking worker.

`handle.accept_snapshot(&id, &snapshot)` validates and admits **all new descriptors
atomically**. `WouldBlock` is retryable. `QueueLimit` means this update itself
exceeds the configured descriptor/metadata limit; refresh with a smaller window
or increase the limit. No prefix is accepted on failure. Prefer
`accept_when_ready` for retries: it subscribes before checking capacity, including
when a snapshot needs more than one free slot. `wait_capacity` only promises an
opportunity to retry, not a reservation. Do not await admission with a full queue
unless the runner is also being polled.

- [Native polling example](../examples/continuous_demo.rs): local clear playlist
  refresh, `try_join!`, typed admission, native fragmented file publication.
- [WASM example](../examples/continuous_wasm.rs): independent JS handle, decimal
  revision strings, non-Send Promise writer, explicit cancellation and bounded
  fixture resources. Production streaming sources should use a demand-driven
  `Source`, as exercised by the SDK harness.
- [Node example test](../scripts/test_continuous_wasm.mjs): delayed WritableStream
  writes, stop, and caller-owned close after the core has flushed.

Run the native example with `cargo run --example continuous_demo -- input.m3u8
recording.fmp4`. Build the JS example with:

```sh
cargo build --target wasm32-unknown-unknown --no-default-features --features serde --example continuous_wasm
wasm-bindgen target/wasm32-unknown-unknown/debug/examples/continuous_wasm.wasm --target web --out-dir target/continuous-example
node scripts/test_continuous_wasm.mjs
```

## Identity and restart

Slots are identified by input, generation, epoch and original media sequence.
Overlapping snapshots align KEY/MAP declarations; snapshot revision alone is not
a new resource identity. Equal retained slots deduplicate. Changed URI (including
authentication query), byte range, key/map, duration, PDT, GAP or epoch yields
`InputRewrite`. A new declaration of the same key URI retains its own key context.
Revision or media-sequence rollback is rejected, not interpreted as a restart.

Call `restart(&id, next_generation)` explicitly, with an increased generation.
It seals the old input. New-generation admission waits until the old accepted
work commits; then the engine applies the epoch/configuration policy. With no
retained overlap, encrypted/MAP input cannot prove declaration continuity and
returns `NeedsReconciliation`. An unannounced sequence hole returns
`MissingSegment`; its duration is unknown, so supply an explicit GAP or restart
with time evidence. Clear contiguous TS needs no KEY/MAP continuity proof.

History has count and metadata byte bounds. Old sequence numbers at or below the
accepted watermark never execute again, including a growing EVENT prefix. Once
history is evicted, the engine cannot fully detect changes to that old prefix.
Caller snapshot size and parse cost are outside the queue budget. This API accepts
typed snapshots only; descriptors are internal execution state.

## Lifecycle

States are preparing, running, paused, draining, finalizing, then one of completed,
failed or cancelled. `stop()` is idempotent: close admission, drain accepted work,
flush/finalize. `end_input(&id)` and ENDLIST close only that input. Empty windows
and unchanged refreshes wait. Overall EOF requires every input to end. The first
normal drain reason wins; before the completion boundary cancellation wins.
After completion, controls cannot change the result.

`with_duration_limit` uses media presentation time and triggers the same drain
protocol. It is not an exact cut: accepted queues and segment granularity can
extend the result. `with_range` instead selects a decodable presentation range,
including required preceding random access samples and an extended end boundary.
An evicted prerequisite is an error. Reports expose requested/actual ranges.

VOD `pause()` requests a consistent processing boundary; await `wait_paused()` to
observe acknowledgement. Resume continues in the same operation, and stop drains
from pause. Open Live/EVENT sessions reject pause. An SDK may stop refreshing a
live playlist, but must handle its sliding-window loss explicitly.

Cancellation competes with read/key/sink acquisition/write/flush and finalization;
CPU loops check cancellation cooperatively. Dropping the runner cancels its
operation sources. Late requests cannot publish events or commit media. The core
flushes borrowed writers but never calls shutdown, close or abort. The SDK must
await its own sink close before public completion, and handle close rejection.

## Time, gaps and splitting

Independent input sequence numbers do not synchronize tracks. Media clocks,
PDT or explicit `ContinuousAnchor`s establish a common timeline. TS wrap is
unwrapped within an epoch; resets create a new mapping. Committed mappings are
immutable. Ambiguous dual-input epoch alignment fails. Initial offsets, signed
CTS, and the longer input's independent tail are preserved.

Missing media fails by default. `with_missing_segments(Skip)` accepts declared
GAP slots, emits gap events and applies `GapPolicy::Preserve` (default) or Collapse.
Dual-input collapse requires matching common gaps; unsupported overlapping media
or ambiguous gap timing fails. An HTTP/key/demux failure is not silently converted
to a GAP. `Split` waits for a decodable boundary; configuration changes default to
failure and require `TimelineChangePolicy::Split` to start a new output. Track
count/roles remain fixed. TS tail duration uses next-sample lookahead or valid
same-epoch evidence; otherwise `TailDurationPolicy` determines failure/explicit
fallback. AAC retains its codec sample duration.

`write_to_outputs` and native `write_to_files` acquire a new sink after detecting
the boundary, finish the old output, then write the new header/media. A single
writer, single path or memory collector rejects split options. Completed child
outputs are retained on errors up to the history limit; subscribe to ordered
Output events to retain the full list.

## Budgets and output ownership

`ContinuousLimits` configures queued descriptors and metadata, retained history,
sample count/bytes, probe depth and dual-input skew. Defaults are 128 queued
items, 4 MiB descriptor metadata, 128 history entries, 65,536 samples, 64 MiB sample
payload, two probe segments and 30 seconds skew. Active processing and lookahead
are bounded separately from the admission queue. Existing `ResourceOptions` and
`KeySessionOptions` still bound encoded resources and key work. These are separate
budgets, not a total RSS guarantee: decrypt/demux/fragment construction may have
bounded copies of the current resource, and caller snapshots/callback storage
remain caller costs. Slow writer flushes stop media progress and admission.

Dual input requires `with_waiter(Arc<dyn ContinuousWait>, timeout)`; the host
provides a cancellable wait future (e.g. a JS Promise). The default timeout is 30
seconds. A lagging input times out while another has outstanding work; idle open
inputs are allowed to wait. The WASM core does not require a Rust timer.

| Output | Contract and retained cost |
| --- | --- |
| `write_to` / `write_to_outputs` | fMP4 fragments; no recording-length mfra accumulation; caller owns close/abort |
| `into_bytes(capacity, format)` | Explicit nonzero capacity; fMP4 or classic Mp4; overflow fails. Collector bytes plus classic demux/mux copies and sample index scale with the recording |
| `write_to_file(s)` | Same-directory temporary fMP4; FragmentedMp4 publication or classic Native/optional FFmpeg finalization; fixed payload copy buffer plus recording-length classic sample index |

Native publication is no-clobber: an existing destination is an error. A successful
hard-link publication is the final completion boundary; temporary paths are cleaned
up on failure/cancellation. It does not offer the legacy partial-file checkpoint or
power-loss durability guarantee. Complete prior split files remain available.
File output requires a filesystem supporting same-directory hard links.

## Reports and capability queries

Each input reports unknown `total`, discovered/accepted/downloaded/decrypted/
committed counts and its committed slot. Discovered counts reflect successfully
admitted unique descriptors; a rejected atomic update changes no counters. GAPs
commit without download/decryption. Fragment bytes and flush must succeed before
committed counters/mappings advance. Reports include bytes, actual duration,
gaps, recent output/mapping history, truncation and queue/sample peaks. Output
reports separate `collected_bytes` and `classic_index_samples`; index allocator
size and additional mux copies are platform dependent.

Mapping events carry generation/epoch/track, configuration identity, optional PDT,
source/presentation/output origins. `map_time` applies the affine source mapping.
Events are delivered in order; the core only retains bounded recent history plus
cumulative counters. A callback must keep its own bounded queue or persist records
if it needs every mapping. Neither events nor serde reports are checkpoints.
Serde writes wide integer fields as decimal strings, including values above 2^53.

Use `capabilities::query_continuous_capability(ContinuousCapabilityQuery)` with
input, output, memory capacity and host waiter constraints. Admission and media
validation remain runtime requirements; this is not an unconditional `live=true`.
No LL-HLS partial segments, multi-audio, subtitles, Packed AAC, GCM or new durable
resume are added. See the [v0.9 verification record](release-0.9.0.md).
