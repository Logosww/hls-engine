# hls-transmux 0.5 prepared sessions / 双路接入

The prepared API accepts **already selected media playlists**, not a master
playlist or language preference. The SDK owns rendition discovery/defaults,
subtitle parsing, HTTP policy, JS AbortSignal bridging and consumer close/abort.

```rust,no_run
use hls_transmux::*;

# async fn run() -> SessionResult<()> {
let inputs = HlsInputs::new(HlsInput::Url("https://cdn.test/video.m3u8".into()))
    .with_audio(HlsInput::Url("https://cdn.test/en/audio.m3u8".into()));
let options = PrepareOptions::default()
    .with_budget(ResourceBudget::default()
        .with_max_in_flight_reads(2)
        .with_max_probe_segments_per_input(2))
    .with_write_mfra(false);
let prepared = prepare_hls(inputs, options).await?;
let mapping = prepared.info().timeline().clone();
// The SDK can now align independent WebVTT output; no output bytes exist yet.
let mut writer = Vec::new(); // substitute an AsyncWrite adapter for true streaming
let report = prepared.write_to(&mut writer).await?;
assert_eq!(report.timeline(), &mapping);
# Ok(())
# }
```

`HlsInput::custom` works for either input; the two inputs may share an underlying
Source factory but have separate operation sessions and caches. URLs in returned
`TextResource::location` control relative URI resolution, including redirects.
Initialization/media ranges retain their existing explicit/implicit semantics.

## Inputs and outputs

- Optional external audio replaces primary embedded audio. The primary must then
  contain video; the external input must contain AAC-LC. Unselected supported
  tracks are discarded before timestamp/configuration checks for selected tracks.
  Unsupported codecs or multi-track containers remain unsupported.
- Without external audio, preserve the primary tracks. Video-only and audio-only
  TS/fMP4 work through both old and prepared APIs. Pure audio means AAC in TS or
  fMP4, not Packed Audio/ADTS playlists.
- `into_mp4_bytes()` returns `(Vec<u8>, SessionReport)` and retains the complete
  output in memory. `write_to(&mut W)` produces incremental fMP4 and flushes,
  but never closes the caller's writer. It does not require `Send` on the writer.
- Native `write_to_file(path, FileOutputOptions)` supports `Mp4`, `FragmentedMp4`
  and `StreamingMp4` (default). The latter reuses native metadata scanning and
  bounded payload copying, or optional `FinalizeBackend::Ffmpeg`.
- File outputs publish through a unique same-directory temporary file and rename.
  Streaming failures retain a `.hls-transmux-session-*.partial.mp4` in that
  directory where possible; the application owns cleanup. Such files are **not
  resumable checkpoints**. Other failed staged files are removed, preserving any
  previous target. Rename is the publication boundary; filesystems that reject
  replacement return an error without deleting the old target.
- The prepared API is single-use and has no resume input. `PrepareOptions::try_from`
  rejects legacy resume/checkpoint callbacks/master selection before any I/O. It
  copies cancellation and mfra only; file format/backend are supplied separately
  through `FileOutputOptions`. No durability checkpoint setting is carried over.

## Exact time mapping

`MediaTime` stores signed `i128` ticks and a nonzero `u32` timescale.
`TimelineMapping::origin()` is the earliest **selected** decode timestamp after
TS unwrapping and supported fMP4 edit offsets. Output time is media time minus
that shared origin; tracks are never independently zeroed. Conversion uses checked
integer arithmetic, rounding toward zero by less than one destination tick.
Negative PTS/CTS remain legal even though output DTS must be nonnegative.

`TimelineMapping::tracks()` records role, track type, original timescale, edit
offset (in that scale), and optional unwrapped TS 90 kHz anchor.
`to_output(MediaTime, destination_timescale)` accepts **already unwrapped and
edit-adjusted** media time. For a raw fMP4 track time, add `edit_offset()` first.
`unwrap_mpegts(raw_33_bit, reference_90k)` selects the epoch near an explicit
reference; the exact half-period is ambiguous and fails. For WebVTT, map its
MPEGTS anchor first, then apply the LOCAL cue delta as rational time. This crate
provides mapping primitives, not a WebVTT parser. For long subtitles, advance the
reference alongside cue progression; do not reuse the initial wrap anchor after
more than half a wrap period.

TS inputs must share a presentation clock. The primary anchors TS-only inputs;
a full fMP4 timestamp anchors mixed inputs. Initial cross-input distance and
successive TS advances must be unambiguous within half of 2^33/90000 seconds.
The API cannot infer a missing external epoch or repair unrelated playlists.
It never aligns by segment index, EXTINF accumulation, language or wall clock.

Preparation fixes mapping once and retains probe samples for execution. Default
probe limit is two media segments per input, stopping as soon as announced tracks
have configuration and initial samples. Insufficient configuration/samples at the
limit fail. Execution rejects timestamp resets, track/configuration/edit-map
changes and unrepresentable decode gaps; one-tick quantization is normalized.
TS video uses one following segment for its last frame duration, then the most
recent interval at EOF (3000/90000 only if no interval exists).

## Bounds and lifecycle

The positive read budget is a shared upper bound; at most one resource per input
is being read at once. Default total concurrency is two. Each executing input
holds current plus one lookahead segment, and the muxer assembles one output
fragment. Preparation may retain up to the configured probe segment count. A
blocked writer starts no additional reads. A fragment may contain only one track;
one input ending never truncates the other or inserts silence.

`Source::create_session_with_options` has a default implementation for compatibility.
Demand-driven custom sessions must not autonomously prefetch; drop of read futures
and `stop_session` must terminate visible effects. Sources with background work
must isolate sessions from other operations. Built-in Reqwest sessions disable
internal prefetch and retain their headers, retry and timeout policies. The
optional resource byte cap is enforced during built-in HTTP reads and checked
on every returned body/text; custom readers own allocations before returning.
This is not a total byte budget. See `BENCHMARKS.md` for remaining growing buffers.

One cancellation token covers both inputs, writer waits and finalize. Dropping
preparation/execution drops pending reads and stops both sessions. Native blocking
finalize checks cancellation and is awaited before returning; dropping its future
signals the worker to stop and clean up its temporary output. Already emitted
stream bytes cannot be rolled back; the SDK must abort its consumer on error.

`SessionError` preserves the underlying legacy `Error` and exposes optional role,
phase, zero-based playlist segment index, sanitized resource and byte range. Global
mux/writer/finalize errors have no input role. No English-message parsing is needed.

`SessionEvent` includes preparation, downloaded snapshots, processed snapshots,
finalize and completion. Downloaded counts exclude initialization; processed
counts advance only after a fragment is written and flushed, or after samples are
accepted by the in-memory batch collector. Aggregate counts are sums of input
counts. Events never contain checkpoint state. `Completed` follows successful
flush/file publication and is never emitted for a failed operation. No callbacks
occur after the operation returns.

## 中文接入摘要

先由 SDK 选定主媒体和可选音频 playlist，再调用 `prepare_hls`。准备结果拥有已读
样本及 Source session，`info().timeline()` 可立即用于独立字幕时间换算；执行时
复用这些样本，不重新下载。调用者放弃准备结果时直接 drop，内部不继续预取。

新接口与旧单路续传接口并存，不生成双路 checkpoint。外置音频必须存在，缺失
即失败；未传外置音频则保留主路已有轨道。MP4 bytes、文件收尾和流式 writer
使用同一映射，保留两路起始差和各自结尾。错误上下文可通过访问器读取。

内存按分片数量约束，仍受单段大小、解封装复制、探测上限影响；全量 bytes、
playlist 元数据、native finalize 样本索引和开启 mfra 的索引需要单独计量。
Browser/Node 的请求中止、writable close/abort、流事件及最终资源发布由 SDK 负责。
