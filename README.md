# hls-transmux

A lightweight Rust HLS → MP4 transmuxer. Reads HLS playlists (local files or
HTTP/HTTPS), demuxes the underlying MPEG-TS or fMP4/CMAF segments, and remuxes
them into a single MP4 — **no decoding, no encoding, no transcoding**.

All core HLS / TS / ISOBMFF logic is self-contained; only a few basic async and
HTTP dependencies are required.

Documentation: [integration guides](docs/README.md).

Version 0.8.0 adds finite sample decryption through the existing keyed/timeline APIs:
TS SAMPLE-AES (AVC/AAC-LC), and fMP4 cenc/cbcs (AVC/HEVC/AAC-LC), producing clear MP4.
See [sample encryption](docs/sample-encryption.md) and [release verification](docs/release-0.8.0.md).

## Features

**Input**

- HLS media playlists and master playlists (explicit variant index selection)
- Local file paths and HTTP/HTTPS sources (async API)
- Segment formats: MPEG-TS and fMP4 / CMAF (`#EXT-X-MAP`)
- `#EXT-X-BYTERANGE` (segments and init segments)

**Codecs**

- Video: H.264 / AVC, H.265 / HEVC
- Audio: AAC-LC

**Output** ([`OutputFormat`])

| Variant         | Layout                                       | Pipeline                              | Peak memory | Playable if interrupted |
| --------------- | -------------------------------------------- | ------------------------------------- | ----------- | ----------------------- |
| `Mp4` (default) | `ftyp` + `moov` + `mdat`                     | batch (demux all to memory, then mux) | high        | no                      |
| `FragmentedMp4` | `ftyp` + `moov` + `moof` + `mdat` per segment | streaming (write per segment)         | low         | yes (fMP4)              |
| `StreamingMp4`  | `ftyp` + `moov` + `mdat`                     | streaming fMP4 → defrag               | sample index + bounded payload buffers | yes (temp file is fMP4) |

`StreamingMp4` produces the same layout as `Mp4`, but uses a streaming fMP4
pipeline (writes a temporary fMP4 file) plus end-of-stream defrag. Native finalize
scans sample metadata and copies payload with a fixed 1 MiB buffer. Memory consists
of segment/prefetch buffers, sample indexes, and the copy buffer; it still grows
with sample count. The temp file `<output>.partial.<ext>` is a valid,
playable fMP4; you can play the downloaded portion after interruption.

## v0.6.2 fragmented timeline fix

Prepared clear/AES-128 sessions with delayed external audio or video now report
the same presentation end in fragmented and classic MP4. Track-local fragment
clocks and exact movie edits preserve the shared timeline without counting the
initial offset twice. Native finalization and optional random-access indexes
retain the same packet timestamps. See [the verification record](docs/release-0.6.2.md).

## v0.6.1 B-frame presentation fix

Classic MP4 edit lists now select the complete media-local presentation interval,
preserving the HEVC/AVC B-frame tail in bytes and native file output. Signed CTS,
initial audio/video offsets and audio tails are retained. The regression gate
compares every decoded frame against independently generated clear inputs for
clear/AES-128 TS/fMP4 across all three keyed output APIs.
See [the verification record](docs/release-0.6.1.md).

## v0.6.0 typed playlists and keyed prepared sessions

Version 0.6.0 extends the existing `hls-transmux` crate. The package name, Rust
import name (`hls_transmux`) and this README remain the entry points for the crate.
The new APIs are available alongside the legacy clear APIs.

The additive `playlist` API parses immutable metadata and supports lossless serde
archives. `crypto::key` provides bounded asynchronous provider resolution,
version/expiry-aware caching and cancellation. The resource layer adds bounded
complete-resource AES-128 decryption and container checks. `prepare_hls_with_keys`
uses the shared prepared media/output core and version-aware MAP reuse. See
[typed playlists](docs/typed-playlists.md), [key sessions](docs/key-sessions.md),
[AES resources](docs/aes-resources.md), [keyed prepared sessions](docs/keyed-sessions.md),
and [diagnostics, progress, capability queries and examples](docs/keyed-contracts.md).
Package version 0.6.0 extends the existing crate; release acceptance is documented
in [the verification record](docs/release-0.6.0.md).
Legacy clear APIs and schema v1 checkpoints remain compatible; AES resume,
live/EVENT and sample encryption were outside v0.6; finite sample encryption is added in v0.8.

| Input and workflow | Entry point | Guide |
| --- | --- | --- |
| Clear HLS, including legacy single-input resume | `transmux_hls_to_mp4_async` and writer/bytes variants | Quick start and resume sections below |
| Selected clear VOD, with optional replacement audio | `prepare_hls` | [Prepared sessions](docs/prepared-sessions.md) |
| Selected finite clear/AES-128/sample-encrypted snapshots with an external key provider | `prepare_hls_with_keys` | [Keyed sessions](docs/keyed-sessions.md) |

## v0.5.0 prepared sessions and pure tracks

`prepare_hls(HlsInputs::new(primary).with_audio(audio), PrepareOptions::default())`
accepts selected VOD media playlists. Inspect `session.info().timeline()` before
output, then consume the session with `into_mp4_bytes()`, `write_to(&mut writer)`
or native `write_to_file(path, FileOutputOptions::default())`.

- TS/fMP4 in either combination, AVC/HEVC video and one selected AAC-LC track.
  External audio replaces embedded audio; no language/master selection is performed.
- Independent video-only and audio-only inputs also work through legacy APIs.
- Shared exact decode origin, retained track offsets, TS wrap handling and signed CTS.
- Demand-driven reads: at most two concurrent reads by default, current plus one
  lookahead segment per input, one pending output fragment. No autonomous prefetch
  while a prepared session is idle or a writer is blocked. Limits depend on segment
  size; bytes output and native finalize sample indexes still grow with duration.
- New events and structured `SessionError` context have no checkpoint. Legacy APIs,
  exhaustive error matches and checkpoint schema v1 are unchanged. Prepared sessions
  do not support resume; new sessions default to `write_mfra=false`.
- The same fixture contracts execute in native Rust and actual WASM/Node.

See [prepared sessions](docs/prepared-sessions.md) for API examples, timestamp
mapping, resource bounds and lifecycle rules. Run `python3 scripts/verify_multi_input.py
--ffmpeg` for independent selected-payload/timestamp checks across output modes.

### v0.5.0 双路合流与纯轨输出

新增 prepared session：先有界探测并取得精确时间映射，再选择 MP4 bytes、fMP4
writer 或 native 文件输出。支持 TS/fMP4 混合输入；外置 AAC 替换主路内嵌音频，
保留音画偏移和各轨完整尾部。单独纯视频、纯音频也支持所有现有输出格式。
选轨策略和 WebVTT 解析仍由 SDK 负责。

新进度不带 checkpoint，错误提供输入角色和分片上下文；旧 API 与 schema v1
保持兼容。新接口不支持续传，默认关闭 mfra；原有接口默认值不变。媒体缓冲按
并发数与分片数限制，不是严格字节预算。详见[接入文档](docs/prepared-sessions.md)。

## v0.4.2 streaming index memory fix

Setting `write_mfra=false` now skips per-fragment random-access index retention
both during streaming and checkpoint recovery. This benefits browser WASM and
native callers. Default indexed output, public APIs and checkpoint schema v1
remain compatible; playlist metadata and media buffers still require memory.

## v0.4.1 media and HTTP fixes

The legacy `on_progress` callback now publishes committed checkpoints only.
Batch `Mp4` has no checkpoint; use runtime phase events for batch progress.

Existing entry points, public struct literals and checkpoint schema v1 remain
compatible. Released v0.3/v0.4 artifacts are covered by recovery tests.

- fMP4 supports multiple `trun` boxes, explicit/moof-relative offsets and run
  continuation. AVC/HEVC NAL widths 1, 2 and 4 are normalized to four bytes.
  Stable `avc3/hev1` initialization is accepted; matching in-band parameter sets
  move to the `avc1/hvc1` sample entry and are removed from samples. Configuration
  changes fail before the affected fragment is written.
- Input fMP4 timescales, sample durations and simple unit-rate edit-list offsets
  are retained. TS timestamps unwrap across the 33-bit boundary before sorting.
  All output modes use a common decode origin and retain signed composition
  offsets and audio/video start differences. Reports include every track and the
  final sample's duration; track durations cover the maximum decode/presentation
  end in their own timescale.
- TS video waits for the next segment to determine the previous final frame's
  duration. At EOF it uses the latest interval, falling back to 3000/90000 seconds
  when none is available. At most one segment awaits commit; an error preserves
  earlier committed fragments. This can delay the first write until the second
  segment arrives. fMP4 with known durations does not require this lookahead.
- Metadata and unknown HLS tags are ignored; unsupported media semantics still
  fail. Implicit ranges require the immediately preceding range on the same URI.
  Initialization caching includes resolved location and range. HTTP ranges check
  status, exact interval, total-size consistency and actual body length.

Configure HTTP safeguards without changing `TransmuxOptions`:

```rust,no_run
use hls_transmux::{HttpRequestPolicy, ReqwestSource};
use std::time::Duration;
let mut policy = HttpRequestPolicy::default();
policy.request_timeout = Some(Duration::from_secs(30)); // headers and entire body
policy.max_retries = 2; // additional attempts; transient transport/status errors
policy.max_resource_bytes = Some(64 * 1024 * 1024); // each HTTP response
let source = ReqwestSource::with_concurrency(4).with_request_policy(policy);
```

Defaults preserve the supplied client's timeout, add no retries and impose no
size cap. Exponential backoff is capped by `backoff_max`; cancellation interrupts
reads and backoff. Policies cover playlists, initialization and media, including
prefetch. Diagnostic URLs omit credentials, query strings and fragments.
A resource cap is not a total memory budget; measurements and the proposed byte
budget are in [benchmarks.md](docs/benchmarks.md).

For finer progress, use `TransmuxRuntimeOptions::on_event` with
`transmux_hls_to_mp4_async_with_runtime`, `transmux_hls_to_writer_async_with_runtime`,
`transmux_hls_to_mp4_bytes_with_runtime` or native-only
`finalize_partial_mp4_async_with_runtime`. Each adds a final runtime argument.
Events distinguish `Downloading`, `Processing`, `Finalizing`, `Completed`.
Completion follows successful sink flush/file commit and is never emitted on
failure. Existing checkpoint callbacks retain their commit ordering.
`downloaded_bytes` counts successful media reads in this invocation, including
TS lookahead and recovery verification; it excludes init, failed retry traffic
and bytes downloaded by earlier invocations. Finalize-only recovery reports zero.

Unsupported boundaries: dependent implicit cross-traf offsets, multiple sample
entries/parameter-set configurations, complex edit lists, decode gaps requiring
additional edits/runs, encryption, discontinuities, automatic alternate-audio selection and live.

The retained [media corpus](tests/fixtures/media/README.md) covers 14 continuous
FFmpeg inputs, including HEVC, B frames, VFR and pure-track support boundaries.
Run `cargo test --test media_corpus` for fixture regressions and
`python3 scripts/verify_media.py` for independent FFprobe/FFmpeg verification.

Pure-track output is supported in v0.5.0. Legacy partial files retain their
existing media/timescale interpretation; recovery does not rewrite history.

## Installation

```toml
[dependencies]
hls-transmux = "0.7"
```

The `default-source` feature is enabled by default (built-in reqwest-backed HTTP
client). To drop reqwest entirely and supply your own HTTP reader:

```toml
[dependencies]
hls-transmux = { version = "0.7", default-features = false }
```

Optionally enable `ffmpeg-finalize` to remux via ffmpeg (through `ffmpeg-next`)
during `StreamingMp4` finalization instead of the built-in defrag path. Requires
FFmpeg 9 shared libraries and pkg-config on the system:

```toml
[dependencies]
hls-transmux = { version = "0.7", features = ["ffmpeg-finalize"] }
```

Optionally enable `serde` to derive `Serialize`/`Deserialize` for
`TransmuxResumeState`, so apps can persist resume checkpoints directly:

```toml
[dependencies]
hls-transmux = { version = "0.7", features = ["serde"] }
```

## Custom Source

This crate focuses on transmuxing only. Resource reads (playlist text + segment
bytes) are abstracted through the [`Source`] trait. [`ReqwestSource`] is the
built-in default; callers can plug in their own implementation:

```rust
use std::path::PathBuf;
use std::sync::Arc;
use hls_transmux::{
    ByteRange, HlsInput, OutputFormat, Source, SourceLocation,
    TextResource, TransmuxOptions, VariantSelection, transmux_hls_to_mp4_async,
};

#[derive(Debug)]
struct MySource;

impl Source for MySource {
    fn read_text<'a>(
        &'a self,
        location: &'a SourceLocation,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = hls_transmux::Result<TextResource>> + Send + 'a>> {
        Box::pin(async move {
            todo!()
        })
    }

    fn read_bytes<'a>(
        &'a self,
        location: &'a SourceLocation,
        range: Option<&'a ByteRange>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = hls_transmux::Result<Vec<u8>>> + Send + 'a>> {
        Box::pin(async move {
            todo!()
        })
    }
}

# async fn run() -> hls_transmux::Result<()> {
let report = transmux_hls_to_mp4_async(
    HlsInput::custom(
        Arc::new(MySource),
        SourceLocation::File(PathBuf::from("playlist.m3u8")),
    ),
    "output.mp4",
    TransmuxOptions::default(),
).await?;
# Ok(())
# }
```

## Concurrent downloads

`ReqwestSource` downloads segments serially by default. Use
[`ReqwestSource::with_concurrency`] (opt-in) for bounded concurrent prefetch —
the built-in HTTP client fetches up to `concurrency` segments ahead while the
transmuxer consumes them in order:

```rust
use std::sync::Arc;
use hls_transmux::{
    HlsInput, OutputFormat, ReqwestSource, SourceLocation,
    TransmuxOptions, VariantSelection, transmux_hls_to_mp4_async,
};

# async fn run() -> hls_transmux::Result<()> {
let source = Arc::new(ReqwestSource::with_concurrency(8));
let location = SourceLocation::Url(
    url::Url::parse("https://example.com/media.m3u8").unwrap()
);
let report = transmux_hls_to_mp4_async(
    HlsInput::custom(source, location),
    "output.fmp4",
    TransmuxOptions {
        output_format: OutputFormat::FragmentedMp4,
        ..Default::default()
    },
).await?;
# Ok(())
# }
```

**Activation conditions:**

- `concurrency > 1`
- Input is an HTTP/HTTPS URL (local files are fast enough sequentially; no prefetch)
- `read_text` returns a media playlist (master playlists are not prefetched — variant not yet chosen)

**Transparency:** the transmuxer still calls `read_bytes(url)` in `segments[i]`
order. Prefetch is invisible to transmux logic — bytes may already be in the slot
cache or may wait for the fetch. `concurrency = 1` uses the original serial path
with zero overhead.

`HlsInput::Url` / `HlsInput::Path` are unchanged (still use `ReqwestSource::new()`,
serial). For concurrency, pass `ReqwestSource::with_concurrency(n)` explicitly via
`HlsInput::custom`.

### Custom request headers (auth / cookies / CDN signatures)

For protected resources (`Authorization: Bearer <token>`, `Cookie`, custom CDN
signature headers), use [`ReqwestSource::with_headers`] or
[`ReqwestSource::with_concurrency_and_headers`] with a `reqwest::header::HeaderMap`.
Headers are attached to **all** outbound HTTP requests (playlist `GET` and
segment `GET`, including Range requests) on both serial and concurrent paths.

```rust
use std::sync::Arc;
use hls_transmux::{
    HlsInput, ReqwestSource, SourceLocation, TransmuxOptions, transmux_hls_to_mp4_async,
};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};

# async fn run() -> hls_transmux::Result<()> {
let mut headers = HeaderMap::new();
headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer secret"));
let source = Arc::new(ReqwestSource::with_concurrency_and_headers(4, headers));
let location = SourceLocation::Url(
    url::Url::parse("https://example.com/media.m3u8").unwrap()
);
let _ = transmux_hls_to_mp4_async(
    HlsInput::custom(source, location),
    "output.fmp4",
    TransmuxOptions::default(),
).await?;
# Ok(())
# }
```

Use the `headers()` accessor to read the configured `HeaderMap`. For a custom
`reqwest::Client` plus headers, build the client with
`reqwest::ClientBuilder::default_headers(headers)` and pass it to
[`ReqwestSource::with_client`] / [`ReqwestSource::with_client_and_concurrency`].

## Progress / cancel / resume

`TransmuxOptions` exposes three optional hooks, all `None` by default (same
behavior as before for existing callers):

- `on_progress`: per-segment progress callback
- `cancel`: cooperative cancellation token
- `resume`: resume checkpoint

### Progress callback

After each streaming segment is committed (demux + write + flush), the crate synchronously invokes
`on_progress` with current progress and a resume snapshot:

```rust
use std::sync::{Arc, Mutex};
use hls_transmux::{
    HlsInput, OutputFormat, TransmuxOptions, TransmuxProgress,
    transmux_hls_to_mp4_async,
};

# async fn run() -> hls_transmux::Result<()> {
let events: Arc<Mutex<Vec<TransmuxProgress>>> = Arc::new(Mutex::new(Vec::new()));
let events_cb = events.clone();

let report = transmux_hls_to_mp4_async(
    HlsInput::Path("playlist.m3u8".into()),
    "output.fmp4",
    TransmuxOptions {
        output_format: OutputFormat::FragmentedMp4,
        on_progress: Some(Arc::new(move |p: TransmuxProgress| {
            events_cb.lock().unwrap().push(p);
        })),
        ..Default::default()
    },
)
.await?;
# Ok(())
# }
```

`TransmuxProgress` fields:

| Field                   | Type                  | Description                                              |
| ----------------------- | --------------------- | -------------------------------------------------------- |
| `total_segments`        | `usize`               | Total segments in playlist                               |
| `completed_segments`    | `usize`               | Segments completed so far                                |
| `downloaded_bytes`      | `u64`                 | Cumulative segment bytes downloaded (excludes init)      |
| `bytes_written`         | `u64`                 | Bytes committed to output; batch uses runtime events    |
| `current_segment_index` | `usize`               | Index of the segment just completed                      |
| `resume`                | `TransmuxResumeState` | Current resume snapshot; persist on every callback       |

### Cooperative cancellation

`cancel` is checked at the start of each segment iteration; cancellation returns
`Error::Cancelled`. On the `StreamingMp4` path, `.partial.mp4` is kept (contains
written fragments as playable fMP4) and can be used for resume.

```rust
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::future::Future;
use std::pin::Pin;
use hls_transmux::{CancelToken, Error, HlsInput, OutputFormat, TransmuxOptions, transmux_hls_to_mp4_async};

#[derive(Debug, Default)]
struct MyCancelToken(Arc<AtomicBool>);

impl MyCancelToken {
    fn trigger(&self) { self.0.store(true, Ordering::SeqCst); }
}

impl CancelToken for MyCancelToken {
    fn is_cancelled(&self) -> bool { self.0.load(Ordering::SeqCst) }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}

# async fn run() -> hls_transmux::Result<()> {
let token = Arc::new(MyCancelToken::default());
let opts = TransmuxOptions {
    output_format: OutputFormat::StreamingMp4,
    cancel: Some(token.clone()),
    ..Default::default()
};
let result = transmux_hls_to_mp4_async(
    HlsInput::Path("playlist.m3u8".into()),
    "output.mp4",
    opts,
).await;
assert!(matches!(result, Err(Error::Cancelled)));
# Ok(())
# }
```

`CancelToken` is a zero-dependency trait; wrap `tokio_util::sync::CancellationToken`
or any cancellation primitive on the app side.

### Resume

`resume` skips `segments[..completed_segments]` and opens the existing output
file in append mode. The app persists `TransmuxResumeState` on each
`on_progress` callback and passes it back after cancel or crash.

```rust
use hls_transmux::{
    HlsInput, OutputFormat, TransmuxOptions, TransmuxResumeState,
    transmux_hls_to_mp4_async,
};

# async fn run() -> hls_transmux::Result<()> {
let saved: TransmuxResumeState = load_from_db()?;

let report = transmux_hls_to_mp4_async(
    HlsInput::Path("playlist.m3u8".into()),
    "output.fmp4",
    TransmuxOptions {
        output_format: OutputFormat::FragmentedMp4,
        resume: Some(saved),
        ..Default::default()
    },
)
.await?;
# Ok(())
# }
# fn load_from_db() -> hls_transmux::Result<TransmuxResumeState> { unimplemented!() }
```

`TransmuxResumeState` fields:

| Field                 | Type    | Description                                                         |
| --------------------- | ------- | ------------------------------------------------------------------- |
| `completed_segments`  | `usize` | Segments done; resume skips `segments[..completed_segments]`        |
| `bytes_written`       | `u64`   | Current output file size; append continues from this offset         |
| `next_sequence`       | `u32`   | Next fragment `mfhd` sequence number                                |
| `global_base_dts_90k` | `u64`   | First-packet DTS (90 kHz); baseline for zeroing all sample timelines |
| `schema_version` | `u32` | Required wire schema version (currently 1) |
| `stage` | `TransmuxStage` | Downloading, Finalizing, Completed |
| `total_segments` | `usize` | Bound manifest segment count |
| `input_digest` / `init_digest` | `[u8; 32]` | SHA-256 identity of resolved manifest / codec configuration |
| `output_format` / `write_mfra` | enum / `bool` | Bound output configuration |
| `duration_ms` | `u64` | Duration accumulated at the committed boundary |

**Constraints:**

- Resume only on `StreamingMp4` / `FragmentedMp4`; `Mp4` + `resume` returns
  `Error::InvalidInput`
- On download resume, the crate re-demuxes `segments[0]` to verify codec config
  and the original timestamp base before modifying the file
- On resume completion, the crate scans existing `.partial.mp4` moof boxes to
  rebuild historical `tfra` entries and emit a full `mfra` box (output bytes match
  a fresh run; only wall-clock timestamps may differ)

### v0.3 reliability and finalize recovery

Each call creates an isolated built-in Source session. Cancellation races all
playlist/init/media reads and writer waits; dropping the entry future stops its
prefetch workers and consumer fetches. Custom sources with background work can
implement `Source::create_session` and `stop_session`; default implementations preserve existing
custom sources. A cancelled non-seekable sink may contain partial output and
cannot be resumed. File calls settle outstanding file writes before returning
from ordinary cancellation/error; native CPU finalization checks cancellation
between parsing/copy operations and waits for its worker to stop.

File checkpoints are emitted after a complete fragment and `flush`. Set
`checkpoint_durability: CheckpointDurability::SyncAll` to also sync the file before
the callback. Persist the checkpoint atomically yourself (write a sibling temp,
sync as required, replace, and sync its parent directory where supported).
Default flush supports process-crash recovery; power-loss persistence also depends
on checkpoint persistence, directory metadata and filesystem guarantees. SyncAll
is rejected for generic writer sinks, whose filesystem guarantees are unknown.

v0.2 checkpoints and unknown schemas are explicitly rejected; start a new v0.3
job. Changed manifest locations (including changed signed URL parameters), byte
ranges, codec initialization or output configuration reject download recovery.
Short files and invalid fragment boundaries/sequences fail without mutation;
validated extra bytes are truncated to the checkpoint before appending.

StreamingMp4 keeps `<stem>.partial.<ext>` on network/finalize errors and cancel.
Retain it together with the last checkpoint. A fresh call without `resume`
starts over and replaces an existing partial file. Delete abandoned partial files
explicitly with your application's filesystem cleanup. Successful finalization
writes an independent sibling `.hls-transmux-finalize-*.mp4`, closes/syncs it as
configured, then atomically renames it over the target. Failure leaves the old
target intact; platforms refusing replacement return an error without deleting
that target. Successful completion removes the partial on a best-effort basis.

The final segment emits `Finalizing` before the trailing index is written, so
interruption while writing that index is recoverable. Pass that checkpoint to
`finalize_partial_mp4_async(partial, output, checkpoint, options)` with
`output_format: OutputFormat::StreamingMp4`, or use the existing file entry with
`options.resume`. Both Finalizing paths use local validated media only, ignoring
the supplied HLS input. You may select a different finalize backend on retry.
`Completed` is emitted only after the target replacement; it is a terminal state
and cannot be resumed. Fragmented output also emits a terminal Completed event
in addition to its per-segment callbacks. Batch callbacks are informational and
do not provide resumable checkpoints.

Since v0.4, Native finalize and resume validation skip media payload during scans.
Classic MP4 supports `co64`, extended-size `mdat`, and 64-bit duration headers.
The bytes API and batch `Mp4` remain in-memory. Schema v1 checkpoints from v0.3
remain compatible. See [benchmarks.md](docs/benchmarks.md) for measurements and limits.
Finalization uses cooperative CPU cancellation;
filesystem commit and FFmpeg header/trailer operations finish before returning.

### `serde` feature

Enable `serde` to derive `Serialize`/`Deserialize` on `TransmuxResumeState`:

```toml
[dependencies]
hls-transmux = { version = "0.7", features = ["serde"] }
```

```rust
# #[cfg(feature = "serde")] {
# use hls_transmux::TransmuxResumeState;
let json = serde_json::to_string(&resume_state)?;
let restored: TransmuxResumeState = serde_json::from_str(&json)?;
# }
# fn serde_json<T>(_: T) -> Result<T, ()> { unimplemented!() }
```

## Quick start

### Local VOD playlist → standard MP4

```rust
use hls_transmux::{
    HlsInput, TransmuxOptions, transmux_hls_to_mp4_async,
};

async fn run() -> hls_transmux::Result<()> {
    let report = transmux_hls_to_mp4_async(
        HlsInput::Path("playlist.m3u8".into()),
        "output.mp4",
        TransmuxOptions::default(),
    )
    .await?;
    println!(
        "wrote {} bytes across {} segments",
        report.bytes_written, report.segment_count
    );
    Ok(())
}
```

### HTTP master playlist → fragmented MP4

```rust
use hls_transmux::{
    HlsInput, OutputFormat, TransmuxOptions, VariantSelection,
    transmux_hls_to_mp4_async,
};

async fn run() -> hls_transmux::Result<()> {
    let report = transmux_hls_to_mp4_async(
        HlsInput::Url("https://example.com/master.m3u8".to_string()),
        "output.fmp4",
        TransmuxOptions {
            variant: Some(VariantSelection::Index(0)),
            output_format: OutputFormat::FragmentedMp4,
            ..Default::default()
        },
    )
    .await?;
    Ok(())
}
```

`VariantSelection` strategies:

| Variant            | Behavior                                                              |
| ------------------ | --------------------------------------------------------------------- |
| `Index(n)`         | Explicit zero-based index (original behavior)                         |
| `HighestBandwidth` | Pick highest `BANDWIDTH`; `bandwidth=None` treated as 0               |
| `LowestBandwidth`  | Pick lowest `BANDWIDTH`; `bandwidth=None` treated as `u64::MAX`       |

On ties (same bandwidth), Rust `max_by_key` / `min_by_key` returns the last match.

### HTTP master playlist → streaming standard MP4

```rust
use hls_transmux::{
    HlsInput, OutputFormat, TransmuxOptions, VariantSelection,
    transmux_hls_to_mp4_async,
};

async fn run() -> hls_transmux::Result<()> {
    let report = transmux_hls_to_mp4_async(
        HlsInput::Url("https://example.com/master.m3u8".to_string()),
        "output.mp4",
        TransmuxOptions {
            variant: Some(VariantSelection::Index(0)),
            output_format: OutputFormat::StreamingMp4,
            ..Default::default()
        },
    )
    .await?;
    Ok(())
}
```

### Streaming standard MP4 + ffmpeg finalization (requires `ffmpeg-finalize`)

```rust
use hls_transmux::{
    FinalizeBackend, HlsInput, OutputFormat, TransmuxOptions, VariantSelection,
    transmux_hls_to_mp4_async,
};

async fn run() -> hls_transmux::Result<()> {
    let report = transmux_hls_to_mp4_async(
        HlsInput::Url("https://example.com/master.m3u8".to_string()),
        "output.mp4",
        TransmuxOptions {
            variant: Some(VariantSelection::Index(0)),
            output_format: OutputFormat::StreamingMp4,
            finalize_backend: FinalizeBackend::Ffmpeg,
            ..Default::default()
        },
    )
    .await?;
    Ok(())
}
```

For blocking callers, wrap with a tokio runtime:

```rust
let report = tokio::runtime::Runtime::new()
    .unwrap()
    .block_on(transmux_hls_to_mp4_async(
        HlsInput::Path("playlist.m3u8".into()),
        "output.mp4",
        TransmuxOptions::default(),
    ))
    .unwrap();
```

## Streaming writer API (fMP4 → AsyncWrite sink)

[`transmux_hls_to_writer_async`] writes MP4 / fMP4 bytes directly to any
`tokio::io::AsyncWrite` sink (HTTP response body, `tokio::io::duplex`, pipe, in-memory
buffer) instead of requiring a file path.

- **`OutputFormat::Mp4`** — batch pipeline: all segments are demuxed into memory, muxed
  into a single `ftyp` + `moov` + `mdat`, then written to the sink in one shot. Peak
  memory ≈ demuxed sample buffer. No streaming; the sink receives nothing until all
  segments are processed.
- **`OutputFormat::FragmentedMp4`** — streaming pipeline: each segment is demuxed, muxed,
  and written as soon as it completes. First bytes reach the sink before later segments
  are processed. Supports download-and-push scenarios (browser `<video>` + MSE).
- **`OutputFormat::StreamingMp4`** returns `Error::InvalidInput` (requires file system
  for temp fMP4 + defrag).

The writer API rejects `resume`; use the file-path API for resumable output.
[`TransmuxOptions::write_mfra`] (default `true`) controls the
trailing `mfra` box; set `false` for non-seekable HTTP sinks. This also skips
per-fragment random-access index accumulation and index reconstruction during
resume; playlist metadata and media buffers still require memory.

For a simple one-liner that returns classic MP4 bytes, use
[`transmux_hls_to_mp4_bytes`]:

```rust
use hls_transmux::{
    HlsInput, TransmuxOptions, transmux_hls_to_mp4_bytes,
};

# async fn run() -> hls_transmux::Result<()> {
let (mp4_bytes, report) = transmux_hls_to_mp4_bytes(
    HlsInput::Path("playlist.m3u8".into()),
    TransmuxOptions::default(), // OutputFormat::Mp4 (classic, in-memory)
).await?;
println!("wrote {} bytes (classic MP4)", mp4_bytes.len());
# Ok(())
# }
```

For streaming fMP4 to an `AsyncWrite` sink with `FragmentedMp4`:

```rust
use hls_transmux::{
    HlsInput, OutputFormat, TransmuxOptions, transmux_hls_to_writer_async,
};

# async fn run() -> hls_transmux::Result<()> {
let mut buf: Vec<u8> = Vec::new();
let report = transmux_hls_to_writer_async(
    HlsInput::Path("playlist.m3u8".into()),
    &mut buf,
    TransmuxOptions {
        output_format: OutputFormat::FragmentedMp4,
        ..Default::default()
    },
)
.await?;
println!("wrote {} bytes (fMP4 in memory)", report.bytes_written);
# Ok(())
# }
```

Typical streaming setup with `tokio::io::duplex` — spawn a task to pump bytes downstream
(HTTP chunked response, IPC pipe, etc.):

```rust,no_run
use hls_transmux::{
    HlsInput, OutputFormat, TransmuxOptions, transmux_hls_to_writer_async,
};
use tokio::io::AsyncReadExt;

# async fn run() -> hls_transmux::Result<()> {
let (mut tx, mut rx) = tokio::io::duplex(256 * 1024);

let pump = tokio::spawn(async move {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match rx.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => { /* push buf[..n] to HTTP response / pipe / etc. */ }
            Err(_) => break,
        }
    }
});

let report = transmux_hls_to_writer_async(
    HlsInput::Path("playlist.m3u8".into()),
    &mut tx,
    TransmuxOptions {
        output_format: OutputFormat::FragmentedMp4,
        ..Default::default()
    },
).await?;

drop(tx);
pump.await.ok();
# Ok(())
# }
```

See [docs/writer-streaming-api.md](docs/writer-streaming-api.md) for details.

## WebAssembly

This crate compiles to `wasm32-unknown-unknown` with `--no-default-features`, enabling
in-browser HLS → MP4 transmuxing without file system or network dependencies.

### Setup

```toml
[dependencies]
hls-transmux = { version = "0.7", default-features = false }
```

`default-features = false` drops `reqwest` (which requires `tokio/net` and is
incompatible with `wasm32-unknown-unknown`). You supply your own `Source`
implementation (e.g. wrapping `fetch()`) or use the built-in [`MemorySource`].

### Recommended entry points

| Use case | API | Output |
| -------- | --- | ------ |
| Download (blob URL) | [`transmux_hls_to_mp4_bytes`] | `Vec<u8>` (classic MP4: `ftyp` + `moov` + `mdat`) |
| Download (blob URL) | [`transmux_hls_to_writer_async`] + `OutputFormat::Mp4` | writes to any `AsyncWrite` |
| MSE streaming | [`transmux_hls_to_writer_async`] + `OutputFormat::FragmentedMp4` | streaming fMP4 (`moof`/`mdat` per segment) |

### What does NOT work on wasm32

- [`transmux_hls_to_mp4_async`] (file-path entry) returns `Error::Unsupported` on
  `wasm32` — it depends on `tokio::fs`. Use the writer or bytes API instead.
- `OutputFormat::StreamingMp4` requires a temp file (`tokio::fs`); not available on
  `wasm32`. Use `OutputFormat::Mp4` (batch, in-memory) instead.
- `ReqwestSource` is not compiled in without `default-features`.

### `MemorySource`

[`MemorySource`] is a simple in-memory `Source` implementation — two `HashMap`s keyed
by absolute URL string (or file path string). The browser JS pre-fetches playlists and
segment bytes, then hands them to `MemorySource`:

```rust
use hls_transmux::{
    HlsInput, MemorySource, OutputFormat, SourceLocation, TransmuxOptions,
    transmux_hls_to_mp4_bytes,
};
use std::sync::Arc;
use url::Url;

# async fn run() -> hls_transmux::Result<()> {
// JS side pre-fetches playlist text and segment bytes, then:
let source = MemorySource::new()
    .text("https://example.com/media.m3u8", playlist_text)
    .segment("https://example.com/seg0.ts", seg0_bytes)
    .segment("https://example.com/seg1.ts", seg1_bytes);

let input = HlsInput::custom(
    Arc::new(source),
    SourceLocation::Url(Url::parse("https://example.com/media.m3u8").unwrap()),
);

let (mp4_bytes, report) = transmux_hls_to_mp4_bytes(
    input,
    TransmuxOptions::default(), // OutputFormat::Mp4 (classic, in-memory)
).await?;
// mp4_bytes is a complete ftyp + moov + mdat → wrap in Blob for download
# Ok(())
# }
```

Keys must match the absolute URLs that the crate resolves from the playlist location
(relative segment URIs are resolved against the playlist URL by the crate internally).

### CI

The CI pipeline includes `cargo check --target wasm32-unknown-unknown
--no-default-features` to ensure the crate stays wasm-compatible.

## API overview

| Name                                                               | Description                                                                                                           |
| ------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------- |
| [`transmux_hls_to_mp4_async`]                                      | File-path entry: local/HTTP/custom Source, master playlist, byterange, fMP4 input, three output formats (not on wasm32) |
| [`transmux_hls_to_writer_async`]                                   | Writer entry: `Mp4` (batch) or `FragmentedMp4` (streaming) → any `AsyncWrite` sink; no resume for `Mp4`               |
| [`transmux_hls_to_mp4_bytes`]                                      | Convenience: returns `Vec<u8>` classic MP4; ideal for wasm / in-memory download                                        |
| [`MemorySource`]                                                   | In-memory `Source` impl: `HashMap<url, text>` + `HashMap<url, bytes>`; for wasm / pre-fetched data                     |
| [`HlsInput`]                                                       | Input source (`Path` / `Url` / `Custom`)                                                                              |
| [`Source`] / [`SourceLocation`] / [`TextResource`] / [`ByteRange`] | Custom resource-reading trait and types                                                                               |
| [`ReqwestSource`]                                                  | Built-in reqwest-backed `Source` (`default-source` feature)                                                           |
| [`TransmuxOptions`]                                                | Options: `variant`, `output_format`, `finalize_backend`, `on_progress`, `cancel`, `resume`, `write_mfra`            |
| [`OutputFormat`]                                                   | `Mp4` (default) / `FragmentedMp4` / `StreamingMp4`                                                                  |
| [`FinalizeBackend`]                                                | `StreamingMp4` finalization: `Native` (default, built-in defrag) / `Ffmpeg` (`ffmpeg-finalize` feature)             |
| [`TransmuxProgress`]                                               | Progress event: `total_segments`, `completed_segments`, `downloaded_bytes`, `bytes_written`, `resume`                 |
| [`CancelToken`]                                                    | Cooperative cancel trait: `is_cancelled` / `cancelled` (zero deps; implement in app)                                  |
| [`TransmuxResumeState`]                                            | Resume checkpoint: `completed_segments`, `bytes_written`, `next_sequence`, `global_base_dts_90k` (optional `serde`) |
| [`VariantSelection`]                                               | Master playlist variant pick: `Index` / `HighestBandwidth` / `LowestBandwidth`                                      |
| [`TransmuxReport`]                                                 | Return value: segment count, track info, duration, bytes written                                                      |
| [`Error`] / [`Result`]                                             | Structured errors: I/O, HTTP, invalid input, unsupported features, bitstream, muxing, cancel                        |

Full docs: `cargo doc --open`.

## Not supported yet

Unsupported profiles are rejected by the corresponding API's typed errors.
AES-128 finite input uses keyed/timeline sessions; discontinuities and presentation
ranges use the timeline API. Legacy entry points keep their original restrictions.

- TS HEVC SAMPLE-AES, TS/Packed AAC SAMPLE-AES-CTR, and AES-GCM execution
- Live/EVENT inputs and LL-HLS
- Automatic rendition selection, multiple video/audio tracks (one selected external audio track is supported)
- Codecs other than AVC / HEVC / AAC-LC (e.g. MP3, AC-3, E-AC-3, AV1)

## Design notes

- Internal timestamps keep PTS / DTS; TS uses 90 kHz clock; output zeroes at first DTS.
- TS and fMP4 demuxers share one `DemuxOutput`; AVC / HEVC share Annex B start-code scan.
- Fragmented MP4 `trun` `data_offset` is precomputed before write (no patch-back).
- `StreamingMp4` temp files use `.partial.<ext>`; extension stays `.mp4`; interrupt yields playable fMP4.
- Remux only — no high-level m3u8 / TS / MP4 parser-muxer dependencies.

## License

MIT.

---

## 中文

一个轻量级的 Rust HLS → MP4 transmuxer。读取 HLS playlist（本地文件或
HTTP/HTTPS），把底层的 MPEG-TS 或 fMP4/CMAF 分片解封装后直接重封装为单个
MP4，**不解码、不编码、不转码**。

核心 HLS / TS / ISOBMFF 逻辑全部自研，仅依赖少量基础异步与 HTTP 库。

v0.8.0 通过现有 keyed/timeline 入口支持有限 sample 解密：TS SAMPLE-AES（AVC/AAC-LC）
及 fMP4 cenc/cbcs（AVC/HEVC/AAC-LC），输出 clear MP4，保留范围、epoch、缺口策略和配置拆分。
接入与验证见 [sample 加密文档](docs/sample-encryption.md) 和 [0.8.0 验收记录](docs/release-0.8.0.md)。

### 特性

**输入**

- HLS media playlist 与 master playlist（显式 variant 索引选择）
- 本地文件路径与 HTTP/HTTPS 源（异步 API）
- 分片格式：MPEG-TS 与 fMP4 / CMAF（`#EXT-X-MAP`）
- `#EXT-X-BYTERANGE`（分片与 init segment 均支持）

**Codec**

- 视频：H.264 / AVC、H.265 / HEVC
- 音频：AAC-LC

**输出**（[`OutputFormat`]）

| 变体            | 输出布局                                     | pipeline                         | 峰值内存 | 中断可播放             |
| --------------- | -------------------------------------------- | -------------------------------- | -------- | ---------------------- |
| `Mp4`（默认）   | `ftyp` + `moov` + `mdat`                     | batch（全部 demux 到内存再 mux） | 高       | 否                     |
| `FragmentedMp4` | `ftyp` + `moov` + 每 segment `moof` + `mdat` | streaming（逐 segment 写盘）     | 低       | 是（fMP4）             |
| `StreamingMp4`  | `ftyp` + `moov` + `mdat`                     | streaming fMP4 → defrag          | sample 索引 + 有界 payload 缓冲 | 是（temp 文件为 fMP4） |

`StreamingMp4` 输出与 `Mp4` 完全一致，但用流式 fMP4 pipeline（写临时 fMP4
文件）+ 末端 defrag。Native 收尾扫描 sample 元数据，以固定 1 MiB 缓冲复制 payload。
内存由分片/预取缓冲、sample 索引和固定复制缓冲组成，仍随 sample 数量增长。临时文件
`<output>.partial.<ext>` 是合法可播放的 fMP4，中断后可直接播放已下载部分。

### 安装

```toml
[dependencies]
hls-transmux = "0.7"
```

默认启用 `default-source` feature（内置 reqwest-backed HTTP
客户端）。若要完全移除 reqwest 依赖、自行实现 HTTP 读取：

```toml
[dependencies]
hls-transmux = { version = "0.7", default-features = false }
```

可选启用 `ffmpeg-finalize` feature，在 `StreamingMp4` finalization 阶段用
ffmpeg（via `ffmpeg-next`）做 remux，替代自研 defrag 路径。需要系统安装 FFmpeg 9
共享库 + pkg-config：

```toml
[dependencies]
hls-transmux = { version = "0.7", features = ["ffmpeg-finalize"] }
```

可选启用 `serde` feature，为 `TransmuxResumeState` 派生
`Serialize`/`Deserialize`，便于 app 直接持久化续传 checkpoint：

```toml
[dependencies]
hls-transmux = { version = "0.7", features = ["serde"] }
```

### 自定义 Source

本 crate 只专注 transmux 能力，资源读取（playlist 文本 + segment 字节）通过
[`Source`] trait 抽象。内置 [`ReqwestSource`]
作为默认实现，调用方可以替换为自行实现：

```rust
use std::path::PathBuf;
use std::sync::Arc;
use hls_transmux::{
    ByteRange, HlsInput, OutputFormat, Source, SourceLocation,
    TextResource, TransmuxOptions, VariantSelection, transmux_hls_to_mp4_async,
};

#[derive(Debug)]
struct MySource;

impl Source for MySource {
    fn read_text<'a>(
        &'a self,
        location: &'a SourceLocation,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = hls_transmux::Result<TextResource>> + Send + 'a>> {
        Box::pin(async move {
            // 自行实现：从 location 读取文本，返回最终 location（处理 redirect 等）
            todo!()
        })
    }

    fn read_bytes<'a>(
        &'a self,
        location: &'a SourceLocation,
        range: Option<&'a ByteRange>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = hls_transmux::Result<Vec<u8>>> + Send + 'a>> {
        Box::pin(async move {
            // 自行实现：从 location 读取字节，按 range 切片
            todo!()
        })
    }
}

# async fn run() -> hls_transmux::Result<()> {
let report = transmux_hls_to_mp4_async(
    HlsInput::custom(
        Arc::new(MySource),
        SourceLocation::File(PathBuf::from("playlist.m3u8")),
    ),
    "output.mp4",
    TransmuxOptions::default(),
).await?;
# Ok(())
# }
```

### 并发下载

`ReqwestSource` 默认串行下载分片。通过 [`ReqwestSource::with_concurrency`]
启用有界并发预取（opt-in），让内置 HTTP 客户端在 transmuxer
顺序消费之前并发拉取最多 `concurrency` 个分片：

```rust
use std::sync::Arc;
use hls_transmux::{
    HlsInput, OutputFormat, ReqwestSource, SourceLocation,
    TransmuxOptions, VariantSelection, transmux_hls_to_mp4_async,
};

# async fn run() -> hls_transmux::Result<()> {
let source = Arc::new(ReqwestSource::with_concurrency(8));
let location = SourceLocation::Url(
    url::Url::parse("https://example.com/media.m3u8").unwrap()
);
let report = transmux_hls_to_mp4_async(
    HlsInput::custom(source, location),
    "output.fmp4",
    TransmuxOptions {
        output_format: OutputFormat::FragmentedMp4,
        ..Default::default()
    },
).await?;
# Ok(())
# }
```

**触发条件**：

- `concurrency > 1`
- 输入为 HTTP/HTTPS URL（本地文件顺序读已足够快，不预取）
- `read_text` 返回 media playlist（master playlist 不预取 —— variant 尚未选定）

**透明性**：transmuxer 仍按 `segments[i]` 顺序调用 `read_bytes(url)`，并发预取对
transmux 逻辑完全透明 —— 字节可能已在 slot cache 中，也可能需要等 fetch
完成。`concurrency = 1` 走原串行路径，零开销。

`HlsInput::Url` / `HlsInput::Path` 不变（仍用
`ReqwestSource::new()`，串行）；并发用户通过 `HlsInput::custom` 显式传入
`ReqwestSource::with_concurrency(n)` 启用。

#### 自定义请求头（鉴权 / Cookie / CDN 签名）

需要访问受保护资源时（如 `Authorization: Bearer <token>`、`Cookie`、自定义
CDN 签名头），用 [`ReqwestSource::with_headers`] 或
[`ReqwestSource::with_concurrency_and_headers`] 传入 `reqwest::header::HeaderMap`。
headers 会附加到**所有**出站 HTTP 请求（playlist `GET` + segment `GET`，含
Range 请求），sequential 与 v3 并发路径均生效。

```rust
use std::sync::Arc;
use hls_transmux::{
    HlsInput, ReqwestSource, SourceLocation, TransmuxOptions, transmux_hls_to_mp4_async,
};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};

# async fn run() -> hls_transmux::Result<()> {
let mut headers = HeaderMap::new();
headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer secret"));
let source = Arc::new(ReqwestSource::with_concurrency_and_headers(4, headers));
let location = SourceLocation::Url(
    url::Url::parse("https://example.com/media.m3u8").unwrap()
);
let _ = transmux_hls_to_mp4_async(
    HlsInput::custom(source, location),
    "output.fmp4",
    TransmuxOptions::default(),
).await?;
# Ok(())
# }
```

`headers()` accessor 可读取已配置的 `HeaderMap`。需要同时使用 custom
`reqwest::Client` + headers 时，用
`reqwest::ClientBuilder::default_headers(headers)` 构造 client，再传给
[`ReqwestSource::with_client`] / [`ReqwestSource::with_client_and_concurrency`]。

### 进度回调 / 取消 / 续传

`TransmuxOptions` 提供三个可选钩子，均默认
`None`（行为与不传时完全一致，不破坏现有调用方）：

- `on_progress`：逐分片进度回调
- `cancel`：协作取消令牌
- `resume`：断点续传 checkpoint

#### 进度回调

每个流式分片提交完成后（demux + 写入 + flush），crate 同步调用 `on_progress`
回调，报告当前进度与续传快照：

```rust
use std::sync::{Arc, Mutex};
use hls_transmux::{
    HlsInput, OutputFormat, TransmuxOptions, TransmuxProgress,
    transmux_hls_to_mp4_async,
};

# async fn run() -> hls_transmux::Result<()> {
let events: Arc<Mutex<Vec<TransmuxProgress>>> = Arc::new(Mutex::new(Vec::new()));
let events_cb = events.clone();

let report = transmux_hls_to_mp4_async(
    HlsInput::Path("playlist.m3u8".into()),
    "output.fmp4",
    TransmuxOptions {
        output_format: OutputFormat::FragmentedMp4,
        on_progress: Some(Arc::new(move |p: TransmuxProgress| {
            events_cb.lock().unwrap().push(p);
        })),
        ..Default::default()
    },
)
.await?;
# Ok(())
# }
```

`TransmuxProgress` 字段：

| 字段                    | 类型                  | 说明                                    |
| ----------------------- | --------------------- | --------------------------------------- |
| `total_segments`        | `usize`               | playlist 总分片数                       |
| `completed_segments`    | `usize`               | 已完成分片数                            |
| `downloaded_bytes`      | `u64`                 | 累计已下载分片字节（不含 init segment） |
| `bytes_written`         | `u64`                 | 已提交输出字节；batch 使用 runtime 事件 |
| `current_segment_index` | `usize`               | 刚完成的分片下标                        |
| `resume`                | `TransmuxResumeState` | 当前续传快照，app 应在每次回调时持久化  |

#### 协作取消

`cancel` 在每个分片迭代开头检查；取消后返回 `Error::Cancelled`。`StreamingMp4`
路径下 `.partial.mp4` 保留（含已写 fragment，是可播放的 fMP4），可直接用于续传。

```rust
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::future::Future;
use std::pin::Pin;
use hls_transmux::{CancelToken, Error, HlsInput, OutputFormat, TransmuxOptions, transmux_hls_to_mp4_async};

#[derive(Debug, Default)]
struct MyCancelToken(Arc<AtomicBool>);

impl MyCancelToken {
    fn trigger(&self) { self.0.store(true, Ordering::SeqCst); }
}

impl CancelToken for MyCancelToken {
    fn is_cancelled(&self) -> bool { self.0.load(Ordering::SeqCst) }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}

# async fn run() -> hls_transmux::Result<()> {
let token = Arc::new(MyCancelToken::default());
let opts = TransmuxOptions {
    output_format: OutputFormat::StreamingMp4,
    cancel: Some(token.clone()),
    ..Default::default()
};
let result = transmux_hls_to_mp4_async(
    HlsInput::Path("playlist.m3u8".into()),
    "output.mp4",
    opts,
).await;
// 取消后得到 Error::Cancelled，.partial.mp4 保留
assert!(matches!(result, Err(Error::Cancelled)));
# Ok(())
# }
```

`CancelToken` 是零依赖 trait，app 侧可包装 `tokio_util::sync::CancellationToken`
或任意取消原语。

#### 断点续传

`resume` 让 crate 跳过 `segments[..completed_segments]`，以 append
模式打开已有输出文件继续写。app 负责在每次 `on_progress` 回调时持久化
`TransmuxResumeState` 快照，取消/崩溃后传回 crate 续传。

```rust
use hls_transmux::{
    HlsInput, OutputFormat, TransmuxOptions, TransmuxResumeState,
    transmux_hls_to_mp4_async,
};

# async fn run() -> hls_transmux::Result<()> {
// app 从持久化层读回上次保存的 checkpoint
let saved: TransmuxResumeState = load_from_db()?;

let report = transmux_hls_to_mp4_async(
    HlsInput::Path("playlist.m3u8".into()),
    "output.fmp4",           // 同一文件，crate 以 append 模式打开
    TransmuxOptions {
        output_format: OutputFormat::FragmentedMp4,
        resume: Some(saved),
        ..Default::default()
    },
)
.await?;
# Ok(())
# }
# fn load_from_db() -> hls_transmux::Result<TransmuxResumeState> { unimplemented!() }
```

`TransmuxResumeState`（schema v1）字段：

| 字段                  | 类型    | 说明                                                           |
| --------------------- | ------- | -------------------------------------------------------------- |
| `completed_segments`  | `usize` | 已完成分片数；续传跳过 `segments[..completed_segments]`        |
| `bytes_written`       | `u64`   | 输出文件当前字节偏移；crate 以 append 模式打开后从此偏移继续写 |
| `next_sequence`       | `u32`   | 下一个 fragment 的 mfhd sequence number                        |
| `global_base_dts_90k` | `u64`   | 首包 DTS（90k 时钟域），所有 sample 时间线归零基准             |
| `schema_version` | `u32` | 必需的 schema 版本，当前为 1 |
| `stage` | `TransmuxStage` | Downloading、Finalizing、Completed |
| `total_segments` | `usize` | 绑定清单的分片总数 |
| `input_digest` / `init_digest` | `[u8; 32]` | 解析后清单与编码配置的 SHA-256 摘要 |
| `output_format` / `write_mfra` | enum / `bool` | 绑定输出配置 |
| `duration_ms` | `u64` | 已提交边界处的累计时长 |

**约束**：

- 仅 `StreamingMp4` / `FragmentedMp4` 支持续传；`Mp4` + `resume` 返回
  `Error::InvalidInput`
- 下载续传先重新 demux `segments[0]` 校验 codec config 和时间戳基准，
  完成文件与输入校验后才截断未提交尾部并追加
- 续传完成时 crate 扫描已有 `.partial.mp4` 的 moof 重建历史 `tfra`
  entries，输出完整 `mfra` box（与首次完成的输出字节一致，仅 wall-clock
  时间戳差异）

#### v0.3 可靠性与收尾恢复

内置 Source 每次调用创建独立任务会话，读取 playlist/init/media 和 writer
等待均可取消；丢弃入口 future 会停止该会话的 worker 与自建 fetch。
自定义后台 Source 可实现 `Source::create_session` 与 `stop_session`；默认实现保持兼容。
非 seekable sink 取消后可能保留部分字节，不支持恢复。文件入口正常取消或
错误返回前等待在途文件写入结束；Native CPU 收尾在解析/复制边界检查取消，
等待 worker 停止后返回。

文件 checkpoint 在完整分片写入和 flush 后发布；设置
`checkpoint_durability: CheckpointDurability::SyncAll` 可在回调前额外同步文件。
调用方需自行原子保存 checkpoint：写同目录临时文件、按需 sync、替换并在支持
的平台同步父目录。默认支持进程崩溃恢复；断电保证还取决于 checkpoint 持久化、
目录元数据及文件系统。通用 writer 不支持 SyncAll。

v0.2 checkpoint 和未知 schema 明确拒绝，需重新开始 v0.3 任务。下载恢复时
校验解析后的清单位置（含签名 URL 参数）、byte range、编码配置和输出配置。
短文件、非法 fragment 边界/sequence 在修改前报错；校验成功后截断未提交尾部。

StreamingMp4 在网络错误、取消和收尾失败后保留 `<stem>.partial.<ext>`。
同时保存最后 checkpoint；不传 resume 的新任务会重写旧 partial，放弃的成果
由应用显式删除。收尾先写同目录 `.hls-transmux-finalize-*.mp4`，完成并关闭后
原子替换目标；失败不删除原目标。平台不支持替换时返回错误。成功后尽力删除
partial；清理失败不改变已成功提交的输出。

最后一个分片在尾部索引写入前发布 Finalizing checkpoint，因此索引写入中断
也可恢复。通过 `finalize_partial_mp4_async(partial, output, checkpoint, options)`
并设置 `output_format: OutputFormat::StreamingMp4` 单独重试，或传给原文件入口的
`options.resume`。两条 Finalizing 路径都只读取本地校验后的数据，忽略传入的
HLS 输入；重试可切换 finalize backend。目标替换成功后才发布 Completed；
该状态不可续传。Fragmented 输出也在每片回调之外增加 Completed 回调。
Batch 回调只提供信息，不产生可续传 checkpoint。

v0.4 的 Native 收尾及续传校验扫描跳过媒体 payload；经典 MP4 支持 `co64`、
大尺寸 `mdat` 和 64 位 duration header。bytes API 和 batch `Mp4` 仍在内存中输出，
v0.3 的 schema v1 checkpoint 保持兼容。测量与限制见 [benchmarks.md](docs/benchmarks.md)。
取消为协作式；文件提交以及
FFmpeg 的 header/trailer 操作完成后才返回。

#### v0.4.2 流式索引内存修复

设置 `write_mfra=false` 后，流式处理和断点恢复均不再保留逐片段随机访问索引，
浏览器 WASM 和原生调用方都可受益。默认带索引输出、公共 API 和 checkpoint
schema v1 保持兼容；播放列表元数据和媒体缓冲仍需占用内存。

#### v0.4.1 媒体与 HTTP 修复

旧 `on_progress` 回调仅发布已提交 checkpoint；batch `Mp4` 没有 checkpoint，
进度请使用 runtime 阶段事件。

现有入口、公开结构完整字面量和 checkpoint schema v1 保持兼容，真实 v0.3/v0.4
产物加入恢复测试。fMP4 支持多个 trun、显式/moof 相对偏移及 run 连续数据偏移；
AVC/HEVC 的 1、2、4 字节 NAL 前缀统一为四字节。稳定且初始化完整的 avc3/hev1
可转换为 avc1/hvc1；相同的 in-band 参数集从 sample 移到初始化声明，配置变化
在对应 fragment 写入前失败。

保留 fMP4 原始 timescale、duration 和简单的单位速率 edit list 偏移；TS 33 位
时间戳先解回绕再排序。各输出共用 decode 起点，保留有符号 composition offset
与音视频起始差。分片报告包含 tracks 和末 sample duration，track duration 使用
归零后 decode/presentation 终点的最大值，单位为轨道自己的 timescale。

TS 视频用下一分片首帧确定当前末帧 duration，最多保留一个待提交分片，因此首写
可能等待第二分片。EOF 使用最近帧间隔，无间隔时回退到 3000/90000 秒；网络错误
保留之前已提交的成果。已知 duration 的 fMP4 不需要该等待。

元数据和未知 HLS 标签忽略；影响媒体语义的未支持功能仍报错。隐式 range 要求
前一分片是同 URI 的 byte range；init 缓存包含解析后位置与 range。HTTP 校验
206、Content-Range 精确区间、总长度及实际 body 长度。

通过 `HttpRequestPolicy` 和 `ReqwestSource::with_request_policy` 配置完整请求
超时、有限重试、指数退避与单响应大小上限，统一覆盖 playlist/init/media 和预取；
读取与退避均可取消。默认沿用客户端超时、不额外重试、不设大小上限。错误保留
分类并补充阶段、资源和分片序号，URL 去除凭据、query 和 fragment。单资源限制
不是总内存预算；测量和后续字节预算设计见 [benchmarks.md](docs/benchmarks.md)。

新增 `TransmuxRuntimeOptions::on_event` 和四类 `*_with_runtime` 入口，最后一个
参数为 runtime options，事件区分 Downloading/Processing/Finalizing/Completed。
成功 flush/文件提交后才发 Completed；旧 checkpoint 回调顺序保持不变。
`downloaded_bytes` 表示本次调用成功读取的媒体字节，包括 TS lookahead 和恢复时
的首片复核，不含 init、失败重试流量和历史下载。仅收尾恢复为零。

复杂 edit list、依赖跨 traf 隐式偏移、多 sample entry/参数集配置、需要额外 edit/run
的 decode gap，以及 C4 的加密、discontinuity、自动音轨选择、live 仍不支持。
旧 partial 保留其既有时间解释，续传不重写历史媒体。

#### `serde` feature

启用 `serde` feature 为 `TransmuxResumeState` 派生
`Serialize`/`Deserialize`，便于 app 直接持久化：

```toml
[dependencies]
hls-transmux = { version = "0.7", features = ["serde"] }
```

```rust
# #[cfg(feature = "serde")] {
# use hls_transmux::TransmuxResumeState;
let json = serde_json::to_string(&resume_state)?;
let restored: TransmuxResumeState = serde_json::from_str(&json)?;
# }
# fn serde_json<T>(_: T) -> Result<T, ()> { unimplemented!() }
```

### 快速开始

#### 本地 VOD playlist → 标准 MP4

```rust
use hls_transmux::{
    HlsInput, TransmuxOptions, transmux_hls_to_mp4_async,
};

async fn run() -> hls_transmux::Result<()> {
    let report = transmux_hls_to_mp4_async(
        HlsInput::Path("playlist.m3u8".into()),
        "output.mp4",
        TransmuxOptions::default(),
    )
    .await?;
    println!(
        "写入 {} 字节，处理 {} 个分片",
        report.bytes_written, report.segment_count
    );
    Ok(())
}
```

#### HTTP master playlist → 分片 MP4

```rust
use hls_transmux::{
    HlsInput, OutputFormat, TransmuxOptions, VariantSelection,
    transmux_hls_to_mp4_async,
};

async fn run() -> hls_transmux::Result<()> {
    let report = transmux_hls_to_mp4_async(
        HlsInput::Url("https://example.com/master.m3u8".to_string()),
        "output.fmp4",
        TransmuxOptions {
            variant: Some(VariantSelection::Index(0)),
            output_format: OutputFormat::FragmentedMp4,
            ..Default::default()
        },
    )
    .await?;
    Ok(())
}
```

`VariantSelection` 三策略：

| 变体               | 行为                                                            |
| ------------------ | --------------------------------------------------------------- |
| `Index(n)`         | 显式指定零基索引（原行为）                                      |
| `HighestBandwidth` | 选 `BANDWIDTH` 最高的 variant；`bandwidth=None` 视为 0          |
| `LowestBandwidth`  | 选 `BANDWIDTH` 最低的 variant；`bandwidth=None` 视为 `u64::MAX` |

并列时（多个 variant 带宽相同）按 Rust `max_by_key` / `min_by_key`
语义返回最后一个匹配元素。

#### HTTP master playlist → 流式标准 MP4

```rust
use hls_transmux::{
    HlsInput, OutputFormat, TransmuxOptions, VariantSelection,
    transmux_hls_to_mp4_async,
};

async fn run() -> hls_transmux::Result<()> {
    let report = transmux_hls_to_mp4_async(
        HlsInput::Url("https://example.com/master.m3u8".to_string()),
        "output.mp4",
        TransmuxOptions {
            variant: Some(VariantSelection::Index(0)),
            output_format: OutputFormat::StreamingMp4,
            ..Default::default()
        },
    )
    .await?;
    Ok(())
}
```

#### 流式标准 MP4 + ffmpeg finalization（需 `ffmpeg-finalize` feature）

```rust
use hls_transmux::{
    FinalizeBackend, HlsInput, OutputFormat, TransmuxOptions, VariantSelection,
    transmux_hls_to_mp4_async,
};

async fn run() -> hls_transmux::Result<()> {
    let report = transmux_hls_to_mp4_async(
        HlsInput::Url("https://example.com/master.m3u8".to_string()),
        "output.mp4",
        TransmuxOptions {
            variant: Some(VariantSelection::Index(0)),
            output_format: OutputFormat::StreamingMp4,
            finalize_backend: FinalizeBackend::Ffmpeg,
            ..Default::default()
        },
    )
    .await?;
    Ok(())
}
```

需要阻塞调用时，用 tokio runtime 包一层即可：

```rust
let report = tokio::runtime::Runtime::new()
    .unwrap()
    .block_on(transmux_hls_to_mp4_async(
        HlsInput::Path("playlist.m3u8".into()),
        "output.mp4",
        TransmuxOptions::default(),
    ))
    .unwrap();
```

### 流式 writer API（fMP4 → AsyncWrite sink）

[`transmux_hls_to_writer_async`] 把 MP4 / fMP4 字节直接写到任意
`tokio::io::AsyncWrite` sink（HTTP response body / `tokio::io::duplex` /
管道 / 内存 buffer），不再强制落盘到文件路径。

- **`OutputFormat::Mp4`** — batch 管线：所有 segment 先 demux 到内存，mux 成单个
  `ftyp` + `moov` + `mdat` 后一次性写入 sink。峰值内存 ≈ 解复用后样本缓冲。无流式
  语义；sink 在所有 segment 处理完之前收不到任何字节。
- **`OutputFormat::FragmentedMp4`** — 流式管线：每个 segment demux + mux 完即写入
  sink，不等后续 segment，支持 "边下边推" 场景（浏览器 `<video>` + MSE 边下边播）。
- **`OutputFormat::StreamingMp4`** 返回 `Error::InvalidInput`（需要文件系统做临时
  fMP4 + defrag）。

writer API 不支持 `resume`；需要断点恢复时请使用文件路径 API。
[`TransmuxOptions::write_mfra`]（默认 `true`）控制末端
`mfra` box：流式 HTTP sink 不可 seek 时可设 `false` 跳过。
关闭后也不再累积逐片段随机访问索引，断点恢复时不重建这些索引；
播放列表元数据和媒体缓冲仍需占用内存。

一行便捷函数 [`transmux_hls_to_mp4_bytes`] 直接返回经典 MP4 字节：

```rust
use hls_transmux::{
    HlsInput, TransmuxOptions, transmux_hls_to_mp4_bytes,
};

# async fn run() -> hls_transmux::Result<()> {
let (mp4_bytes, report) = transmux_hls_to_mp4_bytes(
    HlsInput::Path("playlist.m3u8".into()),
    TransmuxOptions::default(), // OutputFormat::Mp4（经典 MP4，内存）
).await?;
println!("写入 {} 字节（经典 MP4）", mp4_bytes.len());
# Ok(())
# }
```

用 `FragmentedMp4` 流式写入 `AsyncWrite` sink：

```rust
use hls_transmux::{
    HlsInput, OutputFormat, TransmuxOptions, transmux_hls_to_writer_async,
};

# async fn run() -> hls_transmux::Result<()> {
let mut buf: Vec<u8> = Vec::new();
let report = transmux_hls_to_writer_async(
    HlsInput::Path("playlist.m3u8".into()),
    &mut buf,
    TransmuxOptions {
        output_format: OutputFormat::FragmentedMp4,
        ..Default::default()
    },
)
.await?;
println!("wrote {} bytes (fMP4 in memory)", report.bytes_written);
# Ok(())
# }
```

典型 streaming 场景用 `tokio::io::duplex` 接收字节，spawn 一个 task 把字节
推给下游（HTTP chunked response / IPC pipe 等）：

```rust,no_run
use hls_transmux::{
    HlsInput, OutputFormat, TransmuxOptions, transmux_hls_to_writer_async,
};
use tokio::io::AsyncReadExt;

# async fn run() -> hls_transmux::Result<()> {
let (mut tx, mut rx) = tokio::io::duplex(256 * 1024);

// spawn 一个 task：从 rx 读字节推给下游
let pump = tokio::spawn(async move {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match rx.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => { /* push buf[..n] to HTTP response / pipe / etc. */ }
            Err(_) => break,
        }
    }
});

// 当前 task 调用 writer API，写满 duplex 时自动背压
let report = transmux_hls_to_writer_async(
    HlsInput::Path("playlist.m3u8".into()),
    &mut tx,
    TransmuxOptions {
        output_format: OutputFormat::FragmentedMp4,
        ..Default::default()
    },
).await?;

drop(tx);  // 让 pump 自然结束
pump.await.ok();
# Ok(())
# }
```

详见 [docs/writer-streaming-api.md](docs/writer-streaming-api.md)。

### WebAssembly

本 crate 可编译到 `wasm32-unknown-unknown`（`--no-default-features`），
实现浏览器内 HLS → MP4 transmux，无需文件系统或网络依赖。

#### 配置

```toml
[dependencies]
hls-transmux = { version = "0.7", default-features = false }
```

`default-features = false` 移除 `reqwest`（需要 `tokio/net`，与 `wasm32-unknown-unknown`
不兼容）。需自行实现 `Source`（如包装 `fetch()`）或使用内置 [`MemorySource`]。

#### 推荐入口

| 场景 | API | 输出 |
| ---- | --- | ---- |
| 下载（blob URL） | [`transmux_hls_to_mp4_bytes`] | `Vec<u8>`（经典 MP4：`ftyp` + `moov` + `mdat`） |
| 下载（blob URL） | [`transmux_hls_to_writer_async`] + `OutputFormat::Mp4` | 写入任意 `AsyncWrite` |
| MSE 流式播放 | [`transmux_hls_to_writer_async`] + `OutputFormat::FragmentedMp4` | 流式 fMP4（逐 segment `moof`/`mdat`） |

#### wasm32 不可用

- [`transmux_hls_to_mp4_async`]（文件路径入口）在 `wasm32` 上返回 `Error::Unsupported`
  —— 依赖 `tokio::fs`。请改用 writer 或 bytes API。
- `OutputFormat::StreamingMp4` 需要临时文件（`tokio::fs`），wasm32 不可用。
  请改用 `OutputFormat::Mp4`（batch，内存）。
- `ReqwestSource` 在未启用 `default-features` 时不编译。

#### `MemorySource`

[`MemorySource`] 是一个简单的内存 `Source` 实现——两个 `HashMap`，键为绝对 URL
字符串（或文件路径字符串）。浏览器 JS 预取 playlist 文本和分片字节后交给
`MemorySource`：

```rust
use hls_transmux::{
    HlsInput, MemorySource, OutputFormat, SourceLocation, TransmuxOptions,
    transmux_hls_to_mp4_bytes,
};
use std::sync::Arc;
use url::Url;

# async fn run() -> hls_transmux::Result<()> {
// JS 侧预取 playlist 文本和分片字节后：
let source = MemorySource::new()
    .text("https://example.com/media.m3u8", playlist_text)
    .segment("https://example.com/seg0.ts", seg0_bytes)
    .segment("https://example.com/seg1.ts", seg1_bytes);

let input = HlsInput::custom(
    Arc::new(source),
    SourceLocation::Url(Url::parse("https://example.com/media.m3u8").unwrap()),
);

let (mp4_bytes, report) = transmux_hls_to_mp4_bytes(
    input,
    TransmuxOptions::default(), // OutputFormat::Mp4（经典，内存）
).await?;
// mp4_bytes 是完整的 ftyp + moov + mdat → 包成 Blob 供下载
# Ok(())
# }
```

键必须与 crate 从 playlist location 解析出的绝对 URL 一致
（相对 segment URI 由 crate 内部按 playlist URL resolve）。

#### CI

CI 管道包含 `cargo check --target wasm32-unknown-unknown
--no-default-features`，确保 crate 保持 wasm 兼容。

### API 一览

| 名称                                                               | 说明                                                                                                                  |
| ------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------- |
| [`transmux_hls_to_mp4_async`]                                      | 文件路径入口，支持本地/HTTP/自定义 Source、master playlist、byterange、fMP4 输入与三种输出格式（wasm32 不可用）         |
| [`transmux_hls_to_writer_async`]                                   | writer 入口：`Mp4`（batch）或 `FragmentedMp4`（流式）→ 任意 `AsyncWrite` sink；`Mp4` 不支持 resume                    |
| [`transmux_hls_to_mp4_bytes`]                                      | 便捷函数：返回 `Vec<u8>` 经典 MP4；适合 wasm / 内存下载                                                              |
| [`MemorySource`]                                                   | 内存 `Source` 实现：`HashMap<url, 文本>` + `HashMap<url, 字节>`；用于 wasm / 预取数据                                  |
| [`HlsInput`]                                                       | 输入源（`Path` / `Url` / `Custom`）                                                                                   |
| [`Source`] / [`SourceLocation`] / [`TextResource`] / [`ByteRange`] | 自定义资源读取的 trait 与配套类型                                                                                     |
| [`ReqwestSource`]                                                  | 内置 reqwest-backed `Source` 实现（`default-source` feature）                                                         |
| [`TransmuxOptions`]                                                | 选项：`variant`、`output_format`、`finalize_backend`、`on_progress`、`cancel`、`resume`、`write_mfra`                |
| [`OutputFormat`]                                                   | `Mp4`（默认）/ `FragmentedMp4` / `StreamingMp4`                                                                       |
| [`FinalizeBackend`]                                                | `StreamingMp4` 的 finalization 后端：`Native`（默认，自研 defrag）/ `Ffmpeg`（需 `ffmpeg-finalize` feature）          |
| [`TransmuxProgress`]                                               | 进度事件：`total_segments`、`completed_segments`、`downloaded_bytes`、`bytes_written`、`resume`                       |
| [`CancelToken`]                                                    | 协作取消 trait：`is_cancelled` / `cancelled`（零依赖，app 自实现）                                                    |
| [`TransmuxResumeState`]                                            | 续传 checkpoint：`completed_segments`、`bytes_written`、`next_sequence`、`global_base_dts_90k`（可选 `serde` derive） |
| [`VariantSelection`]                                               | master playlist 的 variant 选择（`Index` / `HighestBandwidth` / `LowestBandwidth`）                                   |
| [`TransmuxReport`]                                                 | 返回值：segment 数、track 信息、duration、写入字节数                                                                  |
| [`Error`] / [`Result`]                                             | 结构化错误，区分 I/O、HTTP、非法输入、不支持特性、bitstream、muxing、取消                                             |

完整文档：`cargo doc --open`。

### 暂不支持

不支持的组合由对应入口返回 typed 错误。有限 AES-128 输入使用 keyed/timeline
session；discontinuity 和 presentation 范围使用 timeline API。旧入口保持原限制。

- TS HEVC SAMPLE-AES、TS/Packed AAC SAMPLE-AES-CTR、AES-GCM 执行
- Live/EVENT 输入及 LL-HLS
- 自动选择 alternate audio group、多视频 / 多音频 track（已选外置音轨可用 prepared API）
- 非 AVC / HEVC / AAC-LC 的 codec（如 MP3、AC-3、E-AC-3、AV1）

### 设计说明

- 内部时间戳统一保留 PTS / DTS，TS 使用 90 kHz 时钟，输出以首个 DTS 归零。
- TS 与 fMP4 demuxer 共用同一份 `DemuxOutput` 结构，AVC / HEVC 共用 Annex B
  start code 扫描。
- 分片 MP4 的 `trun` `data_offset` 在写入前预计算，避免回填。
- `StreamingMp4` 的临时文件用 `.partial.<ext>` 命名，扩展名仍是
  `.mp4`，中断时是可直接播放的 fMP4。
- 仅做 remux，不引入高层 m3u8 / TS / MP4 parser-muxer 依赖。

### License

MIT。
