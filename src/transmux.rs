use std::path::Path;
#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;
use std::sync::Arc;

use crate::cancel::CancelToken;
use crate::codecs::avc;
use crate::error::{Error, Result};
use crate::hls::{HlsPlaylist, MasterPlaylist, MediaPlaylist, parse_hls_playlist_content};
use crate::isobmff::demux_isobmff;
use crate::mp4::{
    FragmentedMp4Muxer, FragmentedTrack, Mp4Muxer, Mp4Sample, TfraEntry, assign_delta_durations,
    make_audio_track, make_hevc_video_track, make_video_track, mfra_box,
};
use crate::mpeg_ts::demux_ts;
use crate::resume::{
    CHECKPOINT_SCHEMA_VERSION, CheckpointDurability, TransmuxResumeState, TransmuxStage,
};
use crate::source::{HlsInput, SourceLocation, SourceReader, TextResource};
use crate::types::{DemuxOutput, EncodedPacket, StreamKind, TransmuxReport};

/// Selects which variant of a master playlist to transmux.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VariantSelection {
    /// Zero-based index into the master playlist's variant list.
    Index(usize),
    /// Select the variant with the highest `BANDWIDTH`. Variants without an
    /// explicit `BANDWIDTH` attribute are treated as 0. When multiple variants
    /// share the same (maximum) bandwidth, the last one in playlist order
    /// wins (Rust's `max_by_key` tie-breaking).
    HighestBandwidth,
    /// Select the variant with the lowest `BANDWIDTH`. Variants without an
    /// explicit `BANDWIDTH` attribute are treated as `u64::MAX`. When multiple
    /// variants share the same (minimum) bandwidth, the last one in playlist
    /// order wins (Rust's `min_by_key` tie-breaking).
    LowestBandwidth,
}

impl VariantSelection {
    /// Resolves `self` to a concrete zero-based index into
    /// `master.variants`, applying the selection strategy.
    pub(crate) fn select_index(&self, master: &MasterPlaylist) -> Result<usize> {
        match self {
            Self::Index(index) => {
                if *index >= master.variants.len() {
                    return Err(Error::invalid(format!(
                        "variant index {index} is out of range for {} variants",
                        master.variants.len()
                    )));
                }
                Ok(*index)
            }
            Self::HighestBandwidth => master
                .variants
                .iter()
                .enumerate()
                .max_by_key(|(_, v)| v.bandwidth.unwrap_or(0))
                .map(|(index, _)| index)
                .ok_or_else(|| Error::invalid("master playlist has no variants")),
            Self::LowestBandwidth => master
                .variants
                .iter()
                .enumerate()
                .min_by_key(|(_, v)| v.bandwidth.unwrap_or(u64::MAX))
                .map(|(index, _)| index)
                .ok_or_else(|| Error::invalid("master playlist has no variants")),
        }
    }
}

/// Output container format and pipeline.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum OutputFormat {
    /// Standard non-fragmented MP4 (`ftyp` + `moov` + `mdat`).
    /// Batch pipeline: all segments are demuxed into memory before muxing.
    /// Fastest for small inputs; highest peak memory.
    #[default]
    Mp4,
    /// Fragmented MP4 / CMAF (`ftyp` + `moov` + per-segment `moof` + `mdat`).
    /// Streaming pipeline: each segment is written to disk as it's demuxed.
    /// Interruptible; produces fMP4 directly.
    FragmentedMp4,
    /// Standard non-fragmented MP4 via the streaming fragmented pipeline.
    /// Each segment is demuxed and written to a temp fMP4 file, then
    /// defragged into a single `ftyp` + `moov` + `mdat`. Output uses the same
    /// classic MP4 muxer as [`Mp4`](Self::Mp4). Downloading is streamed, but
    /// Native finalization currently buffers the full media in memory. The
    /// temp file (`<output>.partial.<ext>`) is a playable fMP4 if the
    /// process is interrupted before finalization.
    StreamingMp4,
}

/// Backend used for the finalization step of [`OutputFormat::StreamingMp4`].
///
/// Ignored for other output formats.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FinalizeBackend {
    /// Self-contained defrag using the crate's own ISOBMFF demuxer + MP4
    /// muxer. No external dependencies. Produces faststart (`moov` before
    /// `mdat`) output. This is the default.
    #[default]
    Native,
    /// Use ffmpeg (via `ffmpeg-next`) to remux the temp fMP4 into a standard
    /// MP4. Requires the `ffmpeg-finalize` cargo feature and FFmpeg 9 shared
    /// libraries at build time. Useful when you want to defer to ffmpeg's
    /// battle-tested muxer or need ffmpeg-specific box layout.
    #[cfg(feature = "ffmpeg-finalize")]
    Ffmpeg,
}

/// Options for the async transmux entry point.
///
/// All fields have sensible defaults; `Default::default()` transmuxes to a
/// non-fragmented MP4 and requires the input to be a media playlist (master
/// playlists require an explicit [`VariantSelection`]).
///
/// # Progress, cancellation, resume
///
/// `on_progress`, `cancel`, and `resume` are all optional (default `None`).
/// When `None`, the pipeline behaves exactly as it did before these hooks
/// existed — existing callers and tests do not need to change.
///
/// - `on_progress`: invoked synchronously after each segment is fully
///   processed. The callback receives a [`TransmuxProgress`] which includes
///   a fresh [`TransmuxResumeState`] snapshot; callers should persist it if
///   they want to support resume.
/// - `cancel`: checked at the top of each segment iteration and raced
///   against `Source::read_bytes` await points. On cancel, the pipeline
///   returns [`Error::Cancelled`] promptly.
/// - `resume`: when `Some`, skips `segments[..completed_segments]` and
///   appends to the existing output file. Only supported for
///   [`OutputFormat::StreamingMp4`] and [`OutputFormat::FragmentedMp4`];
///   passing it with [`OutputFormat::Mp4`] returns [`Error::InvalidInput`].
#[derive(Clone)]
pub struct TransmuxOptions {
    /// Required when the input is a master playlist; ignored for media playlists.
    pub variant: Option<VariantSelection>,
    /// Output container format / pipeline. See [`OutputFormat`].
    pub output_format: OutputFormat,
    /// Finalization backend for [`OutputFormat::StreamingMp4`]. Ignored for
    /// other formats. Default: [`FinalizeBackend::Native`].
    pub finalize_backend: FinalizeBackend,
    /// Per-segment progress callback. Invoked synchronously after each
    /// segment is fully processed (demuxed +, for streaming paths, written
    /// to disk). `None` (default) skips the callback entirely.
    pub on_progress: Option<Arc<dyn Fn(TransmuxProgress) + Send + Sync>>,
    /// Cooperative cancellation token. Checked at the top of each segment
    /// iteration and raced against `Source::read_bytes` await points.
    pub cancel: Option<Arc<dyn CancelToken>>,
    /// Resume checkpoint. `None` (default) starts a fresh transmux. `Some`
    /// resumes an interrupted run by skipping `segments[..completed_segments]`
    /// and appending to the output file. Only supported for
    /// [`OutputFormat::StreamingMp4`] and [`OutputFormat::FragmentedMp4`].
    pub resume: Option<TransmuxResumeState>,
    /// Whether to append a trailing `mfra` box at the end of fMP4 output.
    /// Only affects [`OutputFormat::FragmentedMp4`] and the stage-1 temp
    /// file of [`OutputFormat::StreamingMp4`]. Default: `true` (matches
    /// historical behavior). The writer entry point
    /// ([`transmux_hls_to_writer_async`]) honors this flag so callers
    /// targeting a non-seekable streaming sink can set it to `false` to
    /// skip the trailing index (it has little value when the sink cannot
    /// be seeked from the end).
    pub write_mfra: bool,
    /// File persistence guarantee at each checkpoint. Writer sinks only flush.
    pub checkpoint_durability: CheckpointDurability,
}

impl Default for TransmuxOptions {
    fn default() -> Self {
        Self {
            variant: None,
            output_format: OutputFormat::default(),
            finalize_backend: FinalizeBackend::default(),
            on_progress: None,
            cancel: None,
            resume: None,
            write_mfra: true,
            checkpoint_durability: CheckpointDurability::Flush,
        }
    }
}

impl std::fmt::Debug for TransmuxOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransmuxOptions")
            .field("variant", &self.variant)
            .field("output_format", &self.output_format)
            .field("finalize_backend", &self.finalize_backend)
            .field(
                "on_progress",
                &self.on_progress.as_ref().map(|_| "<callback>"),
            )
            .field("cancel", &self.cancel.as_ref().map(|_| "<cancel token>"))
            .field("resume", &self.resume)
            .field("write_mfra", &self.write_mfra)
            .field("checkpoint_durability", &self.checkpoint_durability)
            .finish()
    }
}

/// One progress event, emitted via [`TransmuxOptions::on_progress`] after a
/// segment is committed, and on successful completion.
///
/// The `resume` field is a fresh checkpoint snapshot; callers should persist
/// it on every callback so a crash or cancel can be resumed from the last
/// fully-written segment.
#[derive(Debug, Clone)]
pub struct TransmuxProgress {
    /// Current lifecycle stage; Completed is emitted after final output commit.
    pub stage: TransmuxStage,
    /// Total segments in the media playlist.
    pub total_segments: usize,
    /// Segments fully processed so far.
    pub completed_segments: usize,
    /// Cumulative downloaded segment bytes (excludes init segments).
    pub downloaded_bytes: u64,
    /// Bytes written to the output file. 0 for the [`Mp4`](OutputFormat::Mp4)
    /// batch path (which buffers in memory until the end).
    pub bytes_written: u64,
    /// Index of the segment just completed.
    pub current_segment_index: usize,
    /// Current resume checkpoint snapshot. Callers should persist this on
    /// every callback so a crash/cancel can be resumed.
    pub resume: TransmuxResumeState,
}

/// Internal bundle of the optional hooks extracted from `TransmuxOptions`,
/// passed down to the per-segment loops. Holds borrowed `Arc`s so the loops
/// can invoke the callback / check cancellation without re-reading the
/// `Option`s each iteration.
struct Hooks<'a> {
    on_progress: Option<&'a Arc<dyn Fn(TransmuxProgress) + Send + Sync>>,
    cancel: Option<&'a Arc<dyn CancelToken>>,
    options: &'a TransmuxOptions,
    #[cfg(not(target_arch = "wasm32"))]
    sync_file: Option<&'a tokio::fs::File>,
    #[cfg(test)]
    sync_failure: bool,
}

impl<'a> Hooks<'a> {
    /// Returns `Err(Error::Cancelled)` if the cancel token has been triggered.
    fn check_cancel(&self) -> Result<()> {
        if self.cancel.is_some_and(|c| c.is_cancelled()) {
            return Err(Error::Cancelled);
        }
        Ok(())
    }

    async fn commit<W: tokio::io::AsyncWrite + Send + Unpin>(&self, writer: &mut W) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        crate::cancel::wait(self.cancel, async {
            writer.flush().await.map_err(Error::from)
        })
        .await?;
        #[cfg(test)]
        if self.sync_failure {
            return Err(Error::Io(std::io::Error::other("injected sync failure")));
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(file) = self.sync_file {
            crate::cancel::wait(self.cancel, async {
                file.sync_all().await.map_err(Error::from)
            })
            .await?;
        }
        Ok(())
    }

    /// Emits a progress event if a callback is configured. No-op otherwise.
    fn emit(&self, progress: TransmuxProgress) {
        if let Some(cb) = self.on_progress {
            cb(progress);
        }
    }
}

#[derive(Debug, Default)]
struct PacketCollector {
    packets: Vec<EncodedPacket>,
    vps: Option<Vec<u8>>,
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    audio_specific_config: Option<Vec<u8>>,
    sample_rate: Option<u32>,
    channel_count: Option<u8>,
}

/// Remuxes an HLS playlist to an MP4 file.
///
/// Supports local paths and HTTP/HTTPS URLs, master playlists (with an explicit
/// [`VariantSelection`]), `#EXT-X-BYTERANGE` segments, fMP4/CMAF input via
/// `#EXT-X-MAP`, and both non-fragmented and fragmented MP4 output (selected via
/// [`OutputFormat`]). Master playlists require `options.variant` to be set.
pub async fn transmux_hls_to_mp4_async(
    input: HlsInput,
    output: impl AsRef<Path>,
    options: TransmuxOptions,
) -> Result<TransmuxReport> {
    // On wasm32, file system APIs (tokio::fs) are unavailable. Keep the
    // symbol so consumers don't get link errors, but return a clear error
    // guiding them to the writer / bytes API.
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (&input, &output, &options);
        return Err(Error::unsupported(
            "transmux_hls_to_mp4_async is not available on wasm32 (requires file system); \
             use transmux_hls_to_writer_async or transmux_hls_to_mp4_bytes instead",
        ));
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let output = output.as_ref();
        if let Some(r) = &options.resume {
            r.validate()?;
            if r.stage == TransmuxStage::Completed {
                return Err(Error::invalid("Completed checkpoint is terminal"));
            }
            if r.output_format != options.output_format || r.write_mfra != options.write_mfra {
                return Err(Error::invalid("checkpoint output configuration mismatch"));
            }
            if r.stage == TransmuxStage::Finalizing
                && options.output_format == OutputFormat::StreamingMp4
            {
                return finalize_partial_mp4_async(
                    temp_fmp4_path(output),
                    output,
                    r.clone(),
                    options,
                )
                .await;
            }
        }
        let (root_location, source) = input.into_parts()?;
        let reader = SourceReader::new(source, options.cancel.clone());
        let root_resource = reader.read_text(&root_location).await?;
        let (media_playlist, media_location) =
            resolve_media_playlist(&reader, &root_resource, options.variant).await?;

        // `Mp4` batch path can't resume (it buffers everything in memory and
        // writes once at the end — there's nothing to append to). Reject early
        // so callers get a clear error instead of silent fallback.
        if options.resume.is_some() && matches!(options.output_format, OutputFormat::Mp4) {
            return Err(Error::invalid(
                "resume is only supported with OutputFormat::StreamingMp4 or FragmentedMp4",
            ));
        }

        let hooks = Hooks {
            on_progress: options.on_progress.as_ref(),
            cancel: options.cancel.as_ref(),
            options: &options,
            #[cfg(not(target_arch = "wasm32"))]
            sync_file: None,
            #[cfg(test)]
            sync_failure: false,
        };

        match options.output_format {
            OutputFormat::Mp4 => {
                mux_to_mp4(&reader, &media_location, &media_playlist, output, &hooks).await
            }
            OutputFormat::FragmentedMp4 => {
                transmux_fragmented_async(
                    &reader,
                    &media_location,
                    &media_playlist,
                    output,
                    &hooks,
                    options.resume.clone(),
                )
                .await
            }
            OutputFormat::StreamingMp4 => {
                let temp_path = temp_fmp4_path(output);
                let checkpoint = Arc::new(std::sync::Mutex::new(None));
                let saved = checkpoint.clone();
                let outer = options.on_progress.clone();
                let callback = Arc::new(move |event: TransmuxProgress| {
                    *saved.lock().unwrap() = Some(event.clone());
                    if let Some(cb) = &outer {
                        cb(event);
                    }
                });
                let stage_options = TransmuxOptions {
                    on_progress: Some(callback),
                    ..options.clone()
                };
                let stage_hooks = Hooks {
                    on_progress: stage_options.on_progress.as_ref(),
                    cancel: options.cancel.as_ref(),
                    options: &stage_options,
                    sync_file: None,
                    #[cfg(test)]
                    sync_failure: false,
                };
                transmux_fragmented_async(
                    &reader,
                    &media_location,
                    &media_playlist,
                    &temp_path,
                    &stage_hooks,
                    options.resume.clone(),
                )
                .await?;
                // Ending this session aborts outstanding prefetch before CPU work.
                drop(reader);
                let progress = checkpoint
                    .lock()
                    .unwrap()
                    .clone()
                    .ok_or_else(|| Error::invalid("missing finalization checkpoint"))?;
                let downloaded = progress.downloaded_bytes;
                let mut final_options = options;
                final_options.on_progress = final_options.on_progress.map(|callback| {
                    Arc::new(move |mut event: TransmuxProgress| {
                        event.downloaded_bytes = downloaded;
                        callback(event);
                    }) as Arc<dyn Fn(TransmuxProgress) + Send + Sync>
                });
                finalize_partial_mp4_async(&temp_path, output, progress.resume, final_options).await
            }
        }
    }
}

/// Finalizes a fully downloaded partial file without accessing any Source.
/// Requires a v0.3 `Finalizing` checkpoint for StreamingMp4. Uncommitted
/// trailing bytes are ignored. Failure/cancellation preserves partial and target.
/// Success atomically replaces the target and removes the partial file.
#[cfg(not(target_arch = "wasm32"))]
pub async fn finalize_partial_mp4_async(
    partial: impl AsRef<Path>,
    output: impl AsRef<Path>,
    checkpoint: TransmuxResumeState,
    options: TransmuxOptions,
) -> Result<TransmuxReport> {
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    checkpoint.validate()?;
    if checkpoint.stage != TransmuxStage::Finalizing
        || checkpoint.output_format != OutputFormat::StreamingMp4
        || options.output_format != OutputFormat::StreamingMp4
        || checkpoint.write_mfra != options.write_mfra
    {
        return Err(Error::invalid(
            "finalize requires a matching StreamingMp4 Finalizing checkpoint",
        ));
    }
    let partial = partial.as_ref().to_path_buf();
    let output = output.as_ref().to_path_buf();
    let canonical_partial = tokio::fs::canonicalize(&partial).await?;
    if let Ok(canonical_output) = tokio::fs::canonicalize(&output).await
        && canonical_partial == canonical_output
    {
        return Err(Error::invalid("partial and target paths must differ"));
    }
    struct WorkGuard(Arc<AtomicBool>);
    impl Drop for WorkGuard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    struct TempOutput(PathBuf);
    impl Drop for TempOutput {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    let stopped = Arc::new(AtomicBool::new(false));
    let _work_guard = WorkGuard(stopped.clone());
    let cancel = options.cancel.clone();
    let state = checkpoint.clone();
    let target = output.clone();
    let source = partial.clone();
    let durability = options.checkpoint_durability;
    let backend = options.finalize_backend;
    // Await the worker rather than racing and detaching it: explicit cancel
    // only returns after all local file work has stopped. Dropping this future
    // signals the guard; worker-owned temporary output cleans itself on exit.
    let (report, temporary) = tokio::task::spawn_blocking(move || {
        let check = || {
            if stopped.load(Ordering::Acquire) || cancel.as_ref().is_some_and(|c| c.is_cancelled())
            {
                Err(Error::Cancelled)
            } else {
                Ok(())
            }
        };
        check()?;
        let mut input = std::fs::File::open(&source)?;
        if input.metadata()?.len() < state.bytes_written {
            return Err(Error::invalid("file shorter than checkpoint bytes_written"));
        }
        let size = usize::try_from(state.bytes_written)
            .map_err(|_| Error::invalid("checkpoint too large"))?;
        let mut data = Vec::with_capacity(size);
        let mut buffer = vec![0u8; 1024 * 1024];
        while data.len() < size {
            check()?;
            let n = (size - data.len()).min(buffer.len());
            input.read_exact(&mut buffer[..n])?;
            data.extend_from_slice(&buffer[..n]);
        }
        validate_prefix_checked(&data, &state, &check)?;
        check()?;
        let parent = target
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let (temporary, mut file) = loop {
            let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!(
                ".hls-transmux-finalize-{}-{id}.mp4",
                std::process::id()
            ));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => break (TempOutput(path), file),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(Error::from(e)),
            }
        };
        let report = match backend {
            FinalizeBackend::Native => {
                let init_end = split_fmp4_init(&data)?;
                let demuxed = crate::isobmff::demux_isobmff_checked(
                    &data[..init_end],
                    &data[init_end..],
                    &check,
                )?;
                let mut collector = PacketCollector::default();
                collector.push_demuxed(demuxed)?;
                let (mp4, mut report) =
                    mux_collected_packets_checked(collector, state.total_segments, &check)?;
                for chunk in mp4.chunks(1024 * 1024) {
                    check()?;
                    file.write_all(chunk)?;
                }
                report.bytes_written = mp4.len() as u64;
                report
            }
            #[cfg(feature = "ffmpeg-finalize")]
            FinalizeBackend::Ffmpeg => {
                // Feed FFmpeg a validated prefix, never an uncommitted tail.
                // A separate local staging file also leaves partial untouched.
                let prefix_path = temporary.0.with_extension("input.mp4");
                let mut prefix_file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&prefix_path)?;
                let prefix_guard = TempOutput(prefix_path.clone());
                for chunk in data.chunks(1024 * 1024) {
                    check()?;
                    prefix_file.write_all(chunk)?;
                }
                drop(prefix_file);
                let duration =
                    crate::ffmpeg_finalize::remux_blocking(&prefix_path, &temporary.0, &check)?;
                drop(prefix_guard);
                TransmuxReport {
                    segment_count: state.total_segments,
                    tracks: Vec::new(),
                    duration: duration.max(0) as u64 / 1000,
                    duration_timescale: 1000,
                    bytes_written: file.metadata()?.len(),
                }
            }
        };
        file.flush()?;
        if durability == CheckpointDurability::SyncAll {
            file.sync_all()?;
        }
        drop(file);
        check()?;
        Ok::<_, Error>((report, temporary))
    })
    .await
    .map_err(|e| Error::muxing(format!("finalize worker failed: {e}")))??;
    if options.cancel.as_ref().is_some_and(|c| c.is_cancelled()) {
        return Err(Error::Cancelled);
    }
    // rename never removes the destination first. Platforms/filesystems that
    // refuse replacement return an error and leave the previous target intact.
    tokio::fs::rename(&temporary.0, &output).await?;
    if let Some(callback) = &options.on_progress {
        let mut completed = checkpoint;
        completed.stage = TransmuxStage::Completed;
        completed.bytes_written = report.bytes_written;
        completed.duration_ms = report.duration;
        callback(TransmuxProgress {
            stage: TransmuxStage::Completed,
            total_segments: completed.total_segments,
            completed_segments: completed.completed_segments,
            downloaded_bytes: 0,
            bytes_written: report.bytes_written,
            current_segment_index: completed.completed_segments - 1,
            resume: completed,
        });
    }
    // A cleanup failure cannot invalidate an already committed output.
    let _ = tokio::fs::remove_file(partial).await;
    Ok(report)
}

/// Transmuxes HLS to an MP4 written directly into `writer`.
///
/// Unlike [`transmux_hls_to_mp4_async`] (which writes to a file path), this
/// entry point accepts any [`tokio::io::AsyncWrite`] sink — an HTTP response
/// body, a `tokio::io::duplex`, a pipe, or an in-memory `Vec<u8>`. No file
/// system access is required, so this function works on all targets including
/// `wasm32-unknown-unknown`.
///
/// # Output formats
///
/// - [`OutputFormat::FragmentedMp4`]: streaming pipeline. The transmuxer
///   writes `ftyp` + `moov` after the first segment is demuxed, then one
///   `styp` + `moof` + `mdat` per segment as it is consumed, so the sink
///   receives playable fMP4 bytes before all segments are processed.
/// - [`OutputFormat::Mp4`]: batch pipeline. All segments are demuxed into
///   memory, muxed into a single `ftyp` + `moov` + `mdat`, then written to
///   the sink in one shot. The entire MP4 is buffered before writing —
///   suitable for `download()` semantics (e.g. blob: downloads in a
///   browser). For long inputs where peak memory is a concern, use
///   `FragmentedMp4` instead.
/// - [`OutputFormat::StreamingMp4`]: rejected with [`Error::InvalidInput`].
///   `StreamingMp4` requires a file system for its defrag stage; use
///   `OutputFormat::Mp4` for classic MP4 output via the writer API.
///
/// # Constraints
///
/// - `options.resume` **must** be `None`. Resume requires re-reading the
///   already-written output to rebuild the `mfra` index, which is not possible
///   for a non-seekable sink. Passing `Some` is rejected with
///   [`Error::InvalidInput`].
///
/// # Trailing `mfra`
///
/// Controlled by [`TransmuxOptions::write_mfra`] (default `true`). For
/// non-seekable streaming sinks (e.g. an HTTP chunked response), the trailing
/// `mfra` box has little value since the player cannot seek from the end;
/// callers may set `write_mfra: false` to skip it. With `write_mfra: true`,
/// the byte sequence written to `writer` is identical to what
/// `transmux_hls_to_mp4_async` writes to a file at the same
/// `OutputFormat::FragmentedMp4` setting.
///
/// # Completion semantics
///
/// Before returning `Ok(report)`, the function calls `writer.flush().await?`
/// so all bytes are pushed to the sink. On [`Error::Cancelled`], bytes
/// already written to the sink are not rolled back — the caller is
/// responsible for sink cleanup.
///
/// # Example
///
/// ```no_run
/// use hls_transmux::{
///     HlsInput, OutputFormat, TransmuxOptions, transmux_hls_to_writer_async,
/// };
///
/// # async fn run() -> hls_transmux::Result<()> {
/// let mut buf: Vec<u8> = Vec::new();
/// let report = transmux_hls_to_writer_async(
///     HlsInput::Path("playlist.m3u8".into()),
///     &mut buf,
///     TransmuxOptions {
///         output_format: OutputFormat::FragmentedMp4,
///         ..Default::default()
///     },
/// )
/// .await?;
/// println!("wrote {} bytes (fMP4 in memory)", report.bytes_written);
/// # Ok(())
/// # }
/// ```
pub async fn transmux_hls_to_writer_async<W>(
    input: HlsInput,
    writer: &mut W,
    options: TransmuxOptions,
) -> Result<TransmuxReport>
where
    W: tokio::io::AsyncWrite + Send + Unpin,
{
    use tokio::io::AsyncWriteExt;
    if options.resume.is_some() {
        return Err(Error::invalid("writer API does not support resume"));
    }
    if options.output_format == OutputFormat::StreamingMp4 {
        return Err(Error::invalid("writer API does not support StreamingMp4"));
    }
    if options.checkpoint_durability == CheckpointDurability::SyncAll {
        return Err(Error::invalid("SyncAll requires a file output"));
    }

    let (root_location, source) = input.into_parts()?;
    let reader = SourceReader::new(source, options.cancel.clone());
    let root_resource = reader.read_text(&root_location).await?;
    let (media_playlist, media_location) =
        resolve_media_playlist(&reader, &root_resource, options.variant).await?;

    let hooks = Hooks {
        on_progress: options.on_progress.as_ref(),
        cancel: options.cancel.as_ref(),
        options: &options,
        #[cfg(not(target_arch = "wasm32"))]
        sync_file: None,
        #[cfg(test)]
        sync_failure: false,
    };

    match options.output_format {
        OutputFormat::Mp4 => {
            // Batch pipeline: demux all segments → mux to classic MP4 Vec<u8>
            // → write to writer. No file system needed; works on wasm32.
            // The entire MP4 is buffered in memory before writing, so the
            // sink receives all bytes at once (not streaming).
            if options.resume.is_some() {
                return Err(Error::invalid(
                    "resume is only supported with OutputFormat::StreamingMp4 or FragmentedMp4",
                ));
            }
            let (mp4, mut report) =
                mux_to_mp4_bytes(&reader, &media_location, &media_playlist, &hooks).await?;
            crate::cancel::wait(hooks.cancel, async {
                writer.write_all(&mp4).await.map_err(Error::from)
            })
            .await?;
            hooks.commit(writer).await?;
            report.bytes_written = mp4.len() as u64;
            Ok(report)
        }
        OutputFormat::FragmentedMp4 => {
            if options.resume.is_some() {
                return Err(Error::invalid(
                    "writer API does not support resume (sink is not seekable)",
                ));
            }
            transmux_fragmented_to_writer(
                &reader,
                &media_location,
                &media_playlist,
                writer,
                &hooks,
                None, // resume: always None (rejected above)
                None, // resume_existing: always None (writer sink not re-readable)
            )
            .await
        }
        OutputFormat::StreamingMp4 => Err(Error::invalid(
            "writer API does not support OutputFormat::StreamingMp4 \
             (requires file system for defrag); use OutputFormat::Mp4 for \
             classic MP4 or OutputFormat::FragmentedMp4 for streaming",
        )),
    }
}

/// Convenience wrapper: transmuxes HLS to a classic MP4 (`ftyp` + `moov` +
/// `mdat`) returned as `Vec<u8>`. No file system access — works on all
/// targets including `wasm32-unknown-unknown`.
///
/// Uses the batch pipeline: all segments are demuxed into memory before
/// muxing. For long inputs where peak memory is a concern, use
/// [`transmux_hls_to_writer_async`] with [`OutputFormat::FragmentedMp4`]
/// for streaming output.
///
/// `options.output_format` is overridden to [`OutputFormat::Mp4`]; other
/// options (`variant`, `on_progress`, `cancel`) are honored. `resume` is
/// rejected (batch pipeline does not support resume).
///
/// # Example
///
/// ```no_run
/// use hls_transmux::{
///     HlsInput, TransmuxOptions, transmux_hls_to_mp4_bytes,
/// };
///
/// # async fn run() -> hls_transmux::Result<()> {
/// let (bytes, report) = transmux_hls_to_mp4_bytes(
///     HlsInput::Path("playlist.m3u8".into()),
///     TransmuxOptions::default(),
/// )
/// .await?;
/// println!("{} bytes, {} segments", bytes.len(), report.segment_count);
/// # Ok(())
/// # }
/// ```
pub async fn transmux_hls_to_mp4_bytes(
    input: HlsInput,
    options: TransmuxOptions,
) -> Result<(Vec<u8>, TransmuxReport)> {
    let mut buf: Vec<u8> = Vec::new();
    let report = transmux_hls_to_writer_async(
        input,
        &mut buf,
        TransmuxOptions {
            output_format: OutputFormat::Mp4,
            ..options
        },
    )
    .await?;
    Ok((buf, report))
}

/// Resolves the root resource into a `(MediaPlaylist, SourceLocation)` pair.
///
/// If the root playlist is a master playlist, resolves the selected variant
/// (requires `variant` to be `Some`) and fetches its media playlist. If the
/// root playlist is already a media playlist, returns it directly with its
/// resolved location.
///
/// Shared by [`transmux_hls_to_mp4_async`] and
/// [`transmux_hls_to_writer_async`] so both entry points apply identical
/// master/variant resolution logic.
async fn resolve_media_playlist(
    reader: &SourceReader,
    root_resource: &TextResource,
    variant: Option<VariantSelection>,
) -> Result<(MediaPlaylist, SourceLocation)> {
    let root_playlist = parse_hls_playlist_content(None, &root_resource.content)?;
    match root_playlist {
        HlsPlaylist::Media(media) => Ok((media, root_resource.location.clone())),
        HlsPlaylist::Master(master) => {
            let Some(selection) = variant else {
                return Err(Error::invalid(
                    "master playlist input requires TransmuxOptions.variant",
                ));
            };
            let index = selection.select_index(&master)?;
            let variant_uri = &master.variants[index].uri;
            let variant_location = root_resource.location.resolve(variant_uri)?;
            let variant_resource = reader.read_text(&variant_location).await?;
            let playlist = parse_hls_playlist_content(None, &variant_resource.content)?;
            let HlsPlaylist::Media(media) = playlist else {
                return Err(Error::unsupported(
                    "nested master playlists are not supported",
                ));
            };
            Ok((media, variant_resource.location))
        }
    }
}

/// Batch pipeline: demux all segments into a [`PacketCollector`], then mux
/// to a classic MP4 `Vec<u8>` (`ftyp` + `moov` + `mdat`). No file system
/// access — works on all targets including `wasm32-unknown-unknown`.
///
/// The entire MP4 is buffered in memory before being returned. For long
/// inputs where peak memory is a concern, use the fragmented streaming
/// pipeline ([`transmux_fragmented_to_writer`]) instead.
async fn mux_to_mp4_bytes(
    reader: &SourceReader,
    media_location: &SourceLocation,
    media_playlist: &MediaPlaylist,
    hooks: &Hooks<'_>,
) -> Result<(Vec<u8>, TransmuxReport)> {
    let mut collector = PacketCollector::default();
    read_media_segments(
        reader,
        media_location,
        media_playlist,
        &mut collector,
        hooks,
    )
    .await?;
    let (mp4, report) = mux_collected_packets(collector, media_playlist.segments.len())?;
    Ok((mp4, report))
}

/// Streaming demux + standard MP4 mux to a file path. Native only — uses
/// `tokio::fs::write`.
#[cfg(not(target_arch = "wasm32"))]
async fn mux_to_mp4(
    reader: &SourceReader,
    media_location: &SourceLocation,
    media_playlist: &MediaPlaylist,
    output: &Path,
    hooks: &Hooks<'_>,
) -> Result<TransmuxReport> {
    let (mp4, mut report) = mux_to_mp4_bytes(reader, media_location, media_playlist, hooks).await?;
    use tokio::io::AsyncWriteExt;
    hooks.check_cancel()?;
    let mut file = tokio::fs::File::create(output).await?;
    crate::cancel::wait(hooks.cancel, async {
        file.write_all(&mp4).await.map_err(Error::from)
    })
    .await?;
    hooks.commit(&mut file).await?;
    report.bytes_written = mp4.len() as u64;
    Ok(report)
}

fn config_digest(tracks: &[FragmentedTrack]) -> [u8; 32] {
    use crate::mp4::{FragmentedTrackKind, VideoCodec};
    let mut bytes = Vec::new();
    fn field(out: &mut Vec<u8>, data: &[u8]) {
        out.extend_from_slice(&(data.len() as u64).to_be_bytes());
        out.extend_from_slice(data);
    }
    bytes.extend_from_slice(b"hls-transmux-codecs-v1");
    bytes.extend_from_slice(&(tracks.len() as u64).to_be_bytes());
    for track in tracks {
        bytes.extend_from_slice(&track.track_id.to_be_bytes());
        bytes.extend_from_slice(&track.timescale.to_be_bytes());
        match &track.kind {
            FragmentedTrackKind::Video {
                width,
                height,
                codec,
            } => {
                bytes.extend_from_slice(&width.to_be_bytes());
                bytes.extend_from_slice(&height.to_be_bytes());
                match codec {
                    VideoCodec::Avc { avcc } => {
                        field(&mut bytes, b"avc");
                        field(&mut bytes, avcc);
                    }
                    VideoCodec::Hevc { hvcc } => {
                        field(&mut bytes, b"hevc");
                        field(&mut bytes, hvcc);
                    }
                }
            }
            FragmentedTrackKind::Audio {
                sample_rate,
                channel_count,
                audio_specific_config,
            } => {
                field(&mut bytes, b"aac");
                bytes.extend_from_slice(&sample_rate.to_be_bytes());
                bytes.push(*channel_count);
                field(&mut bytes, audio_specific_config);
            }
        }
    }
    crate::resume::digest(&bytes)
}

fn tracks_from_file(data: &[u8]) -> Result<Vec<FragmentedTrack>> {
    let end = split_fmp4_init(data)?;
    let mut init = demux_isobmff(&data[..end], &[])?;
    init.saw_video = init.sps.is_some();
    init.saw_audio = init.audio_specific_config.is_some();
    build_fragmented_tracks(&init)
}

#[cfg(not(target_arch = "wasm32"))]
fn validate_prefix(data: &[u8], r: &TransmuxResumeState) -> Result<()> {
    validate_prefix_checked(data, r, &|| Ok(()))
}

#[cfg(not(target_arch = "wasm32"))]
fn validate_prefix_checked(
    data: &[u8],
    r: &TransmuxResumeState,
    check: &dyn Fn() -> Result<()>,
) -> Result<()> {
    check()?;
    r.validate()?;
    let end = usize::try_from(r.bytes_written)
        .map_err(|_| Error::invalid("checkpoint offset too large"))?;
    let prefix = data
        .get(..end)
        .ok_or_else(|| Error::invalid("file shorter than checkpoint bytes_written"))?;
    let boxes = crate::resume::boxes(prefix)?;
    if boxes.len() < 5 || boxes[0].0 != *b"ftyp" || boxes[1].0 != *b"moov" {
        return Err(Error::invalid(
            "checkpoint missing initialization or fragments",
        ));
    }
    let mut count = 0u32;
    let mut pos = 2;
    while pos < boxes.len() {
        check()?;
        if boxes[pos].0 == *b"mfra" && pos + 1 == boxes.len() && r.stage == TransmuxStage::Completed
        {
            break;
        }
        if pos + 2 >= boxes.len()
            || boxes[pos].0 != *b"styp"
            || boxes[pos + 1].0 != *b"moof"
            || boxes[pos + 2].0 != *b"mdat"
        {
            return Err(Error::invalid(
                "checkpoint is not at a complete fragment boundary",
            ));
        }
        count = count
            .checked_add(1)
            .ok_or_else(|| Error::invalid("fragment sequence overflow"))?;
        let children = crate::resume::boxes(boxes[pos + 1].2)?;
        let mfhd = children
            .iter()
            .find(|b| b.0 == *b"mfhd")
            .ok_or_else(|| Error::invalid("missing fragment sequence"))?
            .2;
        if mfhd.len() != 8 || u32::from_be_bytes(mfhd[4..8].try_into().unwrap()) != count {
            return Err(Error::invalid("checkpoint fragment sequence mismatch"));
        }
        pos += 3;
    }
    if count as usize != r.completed_segments {
        return Err(Error::invalid("checkpoint fragment count mismatch"));
    }
    let tracks = tracks_from_file(prefix)?;
    if config_digest(&tracks) != r.init_digest {
        return Err(Error::invalid("checkpoint initialization mismatch"));
    }
    let init_end = split_fmp4_init(prefix)?;
    crate::isobmff::demux_isobmff_checked(&prefix[..init_end], &prefix[init_end..], check)?;
    Ok(())
}

/// Splits a fragmented MP4 file into init (`ftyp` + `moov`) and media (the
/// rest). Returns the byte offset where the media portion begins.
fn split_fmp4_init(data: &[u8]) -> Result<usize> {
    let mut offset = 0;
    for expected in [*b"ftyp", *b"moov"] {
        let header = data
            .get(offset..offset + 8)
            .ok_or_else(|| Error::invalid("truncated fMP4 initialization"))?;
        let size = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        if header[4..8] != expected || size < 8 {
            return Err(Error::invalid("invalid fMP4 initialization layout"));
        }
        offset = offset
            .checked_add(size)
            .filter(|&v| v <= data.len())
            .ok_or_else(|| Error::invalid("truncated initialization box"))?;
    }
    Ok(offset)
}

/// Temp file path for stage 1 of the finalized-fragmented path. Uses `.mp4`
/// (a real fragmented MP4 file) so that if the process is interrupted the
/// temp file is still a playable fMP4.
///
/// Native only — only used by the file-path `StreamingMp4` pipeline.
#[cfg(not(target_arch = "wasm32"))]
fn temp_fmp4_path(output: &Path) -> PathBuf {
    let mut p = output.to_path_buf();
    let stem = p.file_stem().map(|s| s.to_os_string()).unwrap_or_default();
    let ext = p.extension().map(|s| s.to_os_string()).unwrap_or_default();
    let mut name = stem;
    name.push(".partial");
    if !ext.is_empty() {
        name.push(".");
        name.push(&ext);
    }
    p.set_file_name(name);
    p
}

/// File-path fragmented MP4 transmux. Native only — uses `tokio::fs` for
/// file create/append/read (resume path).
#[cfg(not(target_arch = "wasm32"))]
async fn transmux_fragmented_async(
    reader: &SourceReader,
    media_location: &SourceLocation,
    media_playlist: &MediaPlaylist,
    output: &Path,
    hooks: &Hooks<'_>,
    resume: Option<TransmuxResumeState>,
) -> Result<TransmuxReport> {
    hooks.check_cancel()?;
    let resume_existing = if let Some(r) = &resume {
        r.validate()?;
        if r.input_digest != crate::resume::input_digest(media_playlist, media_location)?
            || r.total_segments != media_playlist.segments.len()
            || r.output_format != hooks.options.output_format
            || r.write_mfra != hooks.options.write_mfra
            || r.stage == TransmuxStage::Completed
        {
            return Err(Error::invalid(
                "checkpoint input, output configuration or stage mismatch",
            ));
        }
        let data = tokio::fs::read(output).await?;
        validate_prefix(&data, r)?;
        // Recheck the source codec config and normalization base before modifying
        // the file. Finalize-only recovery instead uses exclusively local data.
        let (first, _) = demux_segment(
            reader,
            media_location,
            &media_playlist.segments[0],
            &mut None,
        )
        .await?;
        if config_digest(&build_fragmented_tracks(&first)?) != r.init_digest
            || first.packets.first().map_or(0, |p| p.dts_90k) != r.global_base_dts_90k
        {
            return Err(Error::invalid(
                "checkpoint initialization or timestamp base mismatch",
            ));
        }
        Some(
            data[..usize::try_from(r.bytes_written)
                .map_err(|_| Error::invalid("checkpoint offset too large"))?]
                .to_vec(),
        )
    } else {
        None
    };
    hooks.check_cancel()?;
    let mut file = if let Some(r) = &resume {
        let file = tokio::fs::OpenOptions::new()
            .write(true)
            .append(true)
            .open(output)
            .await?;
        file.set_len(r.bytes_written).await?;
        file
    } else {
        tokio::fs::File::create(output).await?
    };
    let sync_file = if hooks.options.checkpoint_durability == CheckpointDurability::SyncAll {
        Some(file.try_clone().await?)
    } else {
        None
    };
    let hooks = &Hooks {
        on_progress: hooks.on_progress,
        cancel: hooks.cancel,
        options: hooks.options,
        sync_file: sync_file.as_ref(),
        #[cfg(test)]
        sync_failure: hooks.sync_failure,
    };

    // File and writer entry points honor the same trailing-index option.
    let result = transmux_fragmented_to_writer(
        reader,
        media_location,
        media_playlist,
        &mut file,
        hooks,
        resume,
        resume_existing.as_deref(),
    )
    .await;
    // Tokio filesystem writes may already be running in the blocking pool.
    // Settle them on ordinary exit so immediate recovery cannot race a late write.
    use tokio::io::AsyncWriteExt;
    let flush = file.flush().await;
    match result {
        Ok(report) => {
            flush?;
            Ok(report)
        }
        Err(error) => Err(error),
    }
}

/// fMP4 streaming transmux core loop shared by the file path entry point
/// ([`transmux_fragmented_async`]) and the writer entry point
/// ([`transmux_hls_to_writer_async`]).
///
/// Writes `ftyp` + `moov` header on the first segment, then one
/// `styp` + `moof` + `mdat` per segment directly to `writer`, and optionally
/// a trailing `mfra` box at EOF.
///
/// `resume_existing` carries the already-written output bytes for the
/// file-path resume case (so tfra entries can be rebuilt by scanning moof
/// boxes). The writer entry point always passes `None` (it rejects resume
/// upfront). `write_mfra` controls the trailing mfra box.
async fn transmux_fragmented_to_writer<W>(
    reader: &SourceReader,
    media_location: &SourceLocation,
    media_playlist: &MediaPlaylist,
    writer: &mut W,
    hooks: &Hooks<'_>,
    resume: Option<TransmuxResumeState>,
    resume_existing: Option<&[u8]>,
) -> Result<TransmuxReport>
where
    W: tokio::io::AsyncWrite + Send + Unpin,
{
    use tokio::io::AsyncWriteExt;

    let write_mfra = hooks.options.write_mfra;
    let segments = &media_playlist.segments;
    if segments.is_empty() {
        return Err(Error::invalid("media playlist contains no segments"));
    }

    let mut muxer: Option<FragmentedMp4Muxer> = None;
    let mut layout = TrackLayout::default();
    let mut init_cache: Option<(String, Vec<u8>)> = None;
    let mut saved_tracks: Option<Vec<FragmentedTrack>> = None;
    let mut max_duration_ms = resume.as_ref().map_or(0, |r| r.duration_ms);
    let mut downloaded_bytes = 0_u64;

    // Per-track tfra entries accumulated as each fragment is streamed to the
    // writer; consumed by the trailing mfra box at the end. Fresh runs start
    // empty and are sized inside the loop when the muxer is first created.
    // Resumed runs are pre-populated with historical entries rebuilt from
    // `resume_existing` (the file-path case), then new entries are appended
    // as fresh fragments are written — producing a complete mfra at EOF.
    let mut tfra_entries_per_track: Vec<Vec<TfraEntry>> = Vec::new();

    // Restore the committed timestamp/sequence state and reconstruct tracks
    // and historical tfra entries from the already validated local prefix.
    let mut bytes_written: u64 = resume.as_ref().map(|r| r.bytes_written).unwrap_or(0);
    let mut global_base_dts_90k: Option<u64> = resume.as_ref().map(|r| r.global_base_dts_90k);

    if let Some(r) = &resume {
        let existing = resume_existing.ok_or_else(|| Error::invalid("missing recovery prefix"))?;
        let tracks = tracks_from_file(existing)?;
        layout = TrackLayout::from_tracks(&tracks);
        let m = FragmentedMp4Muxer::new_with_sequence(tracks.clone(), r.next_sequence);
        saved_tracks = Some(tracks);
        muxer = Some(m);

        // Rebuild historical tfra entries by scanning the existing output
        // bytes (passed in by the file-path wrapper). The moof boxes are
        // walked to recover each fragment's track ID, base decode time, and
        // absolute moof offset. These are mapped to track indices via the
        // rebuilt tracks and pushed into tfra_entries_per_track, so the
        // resumed run emits a complete mfra box at EOF (matching a fresh
        // run's output byte-for-byte). The writer entry point never reaches
        // here (it rejects resume upfront).
        let existing = resume_existing.unwrap_or(&[]);
        // Scan only up to the checkpoint's bytes_written — guards against
        // any accidental truncation or padding beyond the checkpoint.
        let scan_end = (r.bytes_written as usize).min(existing.len());
        let scanned = crate::isobmff::extract_tfra_entries(&existing[..scan_end])?;
        let tracks_ref = saved_tracks.as_ref().expect("tracks were just saved");
        let track_id_to_index: std::collections::HashMap<u32, usize> = tracks_ref
            .iter()
            .enumerate()
            .map(|(i, t)| (t.track_id, i))
            .collect();
        tfra_entries_per_track = (0..tracks_ref.len()).map(|_| Vec::new()).collect();
        for entry in scanned {
            if let Some(&track_index) = track_id_to_index.get(&entry.track_id) {
                tfra_entries_per_track[track_index].push(TfraEntry {
                    time: entry.base_decode_time,
                    moof_offset: entry.moof_offset,
                    traf_number: 1,
                    trun_number: 1,
                    sample_number: 1,
                });
            }
        }
    }

    let start_index = resume.as_ref().map(|r| r.completed_segments).unwrap_or(0);
    let input_digest = crate::resume::input_digest(media_playlist, media_location)?;

    // --- Streaming: write header (fresh run only), then one (styp + moof +
    // mdat) per segment directly to the writer. The output grows as segments
    // are demuxed, so an interrupted run still leaves a playable fMP4 (ftyp +
    // moov + the fragments written so far). sidx is intentionally omitted: it
    // must reference the total size of all fragments, which is unknown until
    // the end, and writing it would require buffering everything in memory
    // (the exact pattern we are avoiding). mfra is appended at EOF instead.
    for (loop_index, segment) in segments[start_index..].iter().enumerate() {
        // Cooperative cancellation: check at the top of each iteration so a
        // cancelled run stops before downloading the next segment.
        hooks.check_cancel()?;

        let segment_index = start_index + loop_index;
        let (demuxed, segment_bytes) =
            demux_segment(reader, media_location, segment, &mut init_cache).await?;
        downloaded_bytes += segment_bytes;

        // Capture the global base DTS from the first packet we ever see, so
        // every sample across all segments is shifted to a zero-based timeline.
        // Skipped on resume: the checkpoint already carries the original base.
        if global_base_dts_90k.is_none()
            && let Some(first) = demuxed.packets.first()
        {
            global_base_dts_90k = Some(first.dts_90k);
        }
        let base = global_base_dts_90k.unwrap_or(0);

        if muxer.is_none() {
            let tracks = build_fragmented_tracks(&demuxed)?;
            layout = TrackLayout::from_tracks(&tracks);
            tfra_entries_per_track = (0..tracks.len()).map(|_| Vec::new()).collect();
            let m = FragmentedMp4Muxer::new(tracks.clone());
            let header = m.write_header()?;
            crate::cancel::wait(hooks.cancel, async {
                writer.write_all(&header).await.map_err(Error::from)
            })
            .await?;
            bytes_written += header.len() as u64;
            saved_tracks = Some(tracks);
            muxer = Some(m);
        }

        let muxer = muxer.as_mut().expect("muxer was just initialized");
        let samples_per_track = group_samples_per_track(&demuxed.packets, &layout, base)?;

        // Record per-track tfra entries before writing the fragment, using the
        // current writer offset (pointing at the styp that starts this fragment).
        // Each fragment begins with an styp box; its size (first 4 bytes) is
        // the offset of the moof within the fragment.
        let pre_write_offset = bytes_written;
        for (track_index, samples) in samples_per_track.iter().enumerate() {
            if let Some(first) = samples.first() {
                // moof_offset is filled after we know the styp size below;
                // for now record the fragment start and update afterwards.
                // We'll fix this by computing styp size from the fragment bytes.
                tfra_entries_per_track[track_index].push(TfraEntry {
                    time: first.dts,
                    moof_offset: 0, // placeholder, fixed after write_fragment
                    traf_number: 1,
                    trun_number: 1,
                    sample_number: 1,
                });
            }
        }

        let fragment_bytes = muxer.write_fragment(&samples_per_track)?;
        // Each fragment starts with an styp box; its size field (first 4
        // bytes, big-endian) gives us the moof offset relative to the
        // fragment start.
        let styp_size = u32::from_be_bytes(fragment_bytes[0..4].try_into().unwrap()) as u64;

        // Fix up the moof_offset for the entries we just pushed.
        for track_index in 0..samples_per_track.len() {
            if !samples_per_track[track_index].is_empty() {
                let entry = tfra_entries_per_track[track_index]
                    .last_mut()
                    .expect("entry was just pushed");
                entry.moof_offset = pre_write_offset + styp_size;
            }
        }

        crate::cancel::wait(hooks.cancel, async {
            writer.write_all(&fragment_bytes).await.map_err(Error::from)
        })
        .await?;
        bytes_written += fragment_bytes.len() as u64;

        if let Some(last) = demuxed.packets.last() {
            let ms = last.dts_90k.saturating_sub(base).saturating_mul(1000) / 90_000;
            if ms > max_duration_ms {
                max_duration_ms = ms;
            }
        }

        // Emit progress with a fresh resume snapshot. The caller should
        // persist this on every callback so a crash/cancel can resume from
        // the last fully-written fragment.
        let next_sequence = muxer.next_sequence();
        let progress_base = global_base_dts_90k.unwrap_or(0);
        hooks.commit(writer).await?;
        let stage = if segment_index + 1 == segments.len()
            && hooks.options.output_format == OutputFormat::StreamingMp4
        {
            TransmuxStage::Finalizing
        } else {
            TransmuxStage::Downloading
        };
        hooks.emit(TransmuxProgress {
            stage,
            total_segments: segments.len(),
            completed_segments: segment_index + 1,
            downloaded_bytes,
            bytes_written,
            current_segment_index: segment_index,
            resume: TransmuxResumeState {
                completed_segments: segment_index + 1,
                bytes_written,
                next_sequence,
                global_base_dts_90k: progress_base,
                schema_version: CHECKPOINT_SCHEMA_VERSION,
                stage,
                total_segments: segments.len(),
                input_digest,
                init_digest: config_digest(saved_tracks.as_ref().unwrap()),
                output_format: hooks.options.output_format,
                write_mfra,
                duration_ms: max_duration_ms,
            },
        });
    }

    if saved_tracks.is_none() {
        return Err(Error::invalid("no segments were processed"));
    }

    // mfra at EOF lets players find sync samples by seeking from the end.
    // Resumed runs rebuild historical tfra entries by scanning the existing
    // output (plan §5.5), so both fresh and resumed runs emit a complete
    // mfra box — the outputs are byte-identical (after wall-clock timestamp
    // normalization). The writer entry point may skip this via `write_mfra`
    // for non-seekable streaming sinks.
    if write_mfra && !tfra_entries_per_track.is_empty() {
        let tracks_ref = saved_tracks.as_ref().expect("tracks were saved");
        let mfra = mfra_box(tracks_ref, &tfra_entries_per_track)?;
        crate::cancel::wait(hooks.cancel, async {
            writer.write_all(&mfra).await.map_err(Error::from)
        })
        .await?;
        bytes_written += mfra.len() as u64;
    }

    hooks.commit(writer).await?;

    if hooks.options.output_format == OutputFormat::FragmentedMp4 {
        let completed = TransmuxResumeState {
            schema_version: CHECKPOINT_SCHEMA_VERSION,
            stage: TransmuxStage::Completed,
            completed_segments: segments.len(),
            total_segments: segments.len(),
            bytes_written,
            next_sequence: muxer.as_ref().unwrap().next_sequence(),
            global_base_dts_90k: global_base_dts_90k.unwrap_or(0),
            input_digest,
            init_digest: config_digest(saved_tracks.as_ref().unwrap()),
            output_format: hooks.options.output_format,
            write_mfra,
            duration_ms: max_duration_ms,
        };
        hooks.emit(TransmuxProgress {
            stage: TransmuxStage::Completed,
            total_segments: segments.len(),
            completed_segments: segments.len(),
            downloaded_bytes,
            bytes_written,
            current_segment_index: segments.len() - 1,
            resume: completed,
        });
    }

    Ok(TransmuxReport {
        segment_count: media_playlist.segments.len(),
        tracks: Vec::new(),
        duration: max_duration_ms,
        duration_timescale: 1000,
        bytes_written,
    })
}

#[derive(Debug, Clone, Copy, Default)]
struct TrackLayout {
    /// Index of the video track in the muxer, if present.
    video_index: Option<usize>,
    /// Index of the audio track in the muxer, if present.
    audio_index: Option<usize>,
    /// Audio timescale (AAC sample rate) for rescaling 90 kHz timestamps.
    audio_timescale: u32,
}

impl TrackLayout {
    fn from_tracks(tracks: &[FragmentedTrack]) -> Self {
        use crate::mp4::FragmentedTrackKind;
        let mut layout = Self::default();
        for (index, track) in tracks.iter().enumerate() {
            match &track.kind {
                FragmentedTrackKind::Video { .. } => layout.video_index = Some(index),
                FragmentedTrackKind::Audio { sample_rate, .. } => {
                    layout.audio_index = Some(index);
                    layout.audio_timescale = *sample_rate;
                }
            }
        }
        layout
    }

    fn track_count(&self) -> usize {
        self.video_index.is_some() as usize + self.audio_index.is_some() as usize
    }
}

/// Demux a single HLS segment, handling both TS and fMP4/CMAF inputs.
///
/// Returns the demuxed packets plus the segment's raw byte length (excluding
/// any init segment). The byte length is used to populate progress callbacks.
async fn demux_segment(
    reader: &SourceReader,
    playlist_location: &SourceLocation,
    segment: &crate::hls::HlsSegment,
    init_cache: &mut Option<(String, Vec<u8>)>,
) -> Result<(DemuxOutput, u64)> {
    let location = playlist_location.resolve(&segment.uri)?;
    let data = reader
        .read_bytes(&location, segment.byte_range.as_ref())
        .await?;
    let segment_bytes = data.len() as u64;

    let demuxed = if let Some(init_spec) = &segment.init_segment {
        let init_bytes = if let Some((_, cached_bytes)) =
            init_cache.as_ref().filter(|(uri, _)| uri == &init_spec.uri)
        {
            cached_bytes.clone()
        } else {
            let init_location = playlist_location.resolve(&init_spec.uri)?;
            let bytes = reader
                .read_bytes(&init_location, init_spec.byte_range.as_ref())
                .await?;
            *init_cache = Some((init_spec.uri.clone(), bytes.clone()));
            bytes
        };
        demux_isobmff(&init_bytes, &data)?
    } else {
        demux_ts(&data)?
    };
    Ok((demuxed, segment_bytes))
}

fn build_fragmented_tracks(first: &DemuxOutput) -> Result<Vec<FragmentedTrack>> {
    let mut tracks = Vec::new();
    let mut next_track_id = 1_u32;

    if first.saw_video {
        if let Some(vps) = &first.vps {
            let sps = first
                .sps
                .as_ref()
                .ok_or_else(|| Error::bitstream("HEVC SPS was not found in first segment"))?;
            let pps = first
                .pps
                .as_ref()
                .ok_or_else(|| Error::bitstream("HEVC PPS was not found in first segment"))?;
            tracks.push(FragmentedTrack::hevc_video(next_track_id, vps, sps, pps)?);
        } else {
            let sps = first
                .sps
                .as_ref()
                .ok_or_else(|| Error::bitstream("H.264 SPS was not found in first segment"))?;
            let pps = first
                .pps
                .as_ref()
                .ok_or_else(|| Error::bitstream("H.264 PPS was not found in first segment"))?;
            tracks.push(FragmentedTrack::avc_video(next_track_id, sps, pps)?);
        }
        next_track_id += 1;
    }

    if first.saw_audio {
        let sample_rate = first
            .sample_rate
            .ok_or_else(|| Error::bitstream("AAC sample rate was not found in first segment"))?;
        let channel_count = first
            .channel_count
            .ok_or_else(|| Error::bitstream("AAC channel count was not found in first segment"))?;
        let asc = first.audio_specific_config.clone().ok_or_else(|| {
            Error::bitstream("AAC AudioSpecificConfig was not found in first segment")
        })?;
        tracks.push(FragmentedTrack::audio(
            next_track_id,
            sample_rate,
            channel_count,
            asc,
        ));
    }

    if tracks.is_empty() {
        return Err(Error::invalid("first segment produced no tracks"));
    }
    Ok(tracks)
}

/// Groups a segment's packets into per-track `Mp4Sample` vectors matching the
/// muxer's track layout. Tracks with no samples in this segment get an empty vec.
///
/// `base_dts_90k` is the global base DTS (90k domain) captured from the first
/// packet of the first segment. All sample DTS/PTS are shifted by this base so
/// the timeline starts at 0; this keeps tfdt values correct (fragment 0 starts
/// at 0, later fragments at their cumulative offset) without inflating the
/// track duration with a non-zero TS encoder initial timestamp.
fn group_samples_per_track(
    packets: &[EncodedPacket],
    layout: &TrackLayout,
    base_dts_90k: u64,
) -> Result<Vec<Vec<Mp4Sample>>> {
    let mut video_samples: Vec<Mp4Sample> = Vec::new();
    let mut audio_samples: Vec<Mp4Sample> = Vec::new();

    for packet in packets {
        match packet.kind {
            StreamKind::Avc | StreamKind::Hevc => {
                let data = if packet.is_length_prefixed {
                    packet.data.clone()
                } else {
                    avc::annex_b_to_length_prefixed(&packet.data)?
                };
                // Shift to a zero-based timeline using the global base so tfdt
                // starts at 0 and the track duration is not inflated by the
                // TS encoder's initial timestamp offset.
                video_samples.push(Mp4Sample {
                    data,
                    dts: packet.dts_90k.saturating_sub(base_dts_90k),
                    pts: packet.pts_90k.saturating_sub(base_dts_90k),
                    duration: 0,
                    is_key: packet.is_key,
                    offset: 0,
                });
            }
            StreamKind::Aac => {
                let audio_ts = if layout.audio_timescale > 0 {
                    layout.audio_timescale
                } else {
                    90_000
                };
                let dts = rescale_90k(packet.dts_90k.saturating_sub(base_dts_90k), audio_ts);
                let pts = rescale_90k(packet.pts_90k.saturating_sub(base_dts_90k), audio_ts);
                audio_samples.push(Mp4Sample {
                    data: packet.data.clone(),
                    dts,
                    pts,
                    duration: 1024,
                    is_key: true,
                    offset: 0,
                });
            }
        }
    }

    // Build the output in track-index order, with empty vecs for missing tracks.
    let track_count = layout.track_count();
    let mut out: Vec<Vec<Mp4Sample>> = (0..track_count).map(|_| Vec::new()).collect();
    if let Some(vi) = layout.video_index {
        if !video_samples.is_empty() {
            assign_delta_durations(&mut video_samples)?;
        }
        out[vi] = video_samples;
    }
    if let Some(ai) = layout.audio_index {
        out[ai] = audio_samples;
    }
    Ok(out)
}

async fn read_media_segments(
    reader: &SourceReader,
    playlist_location: &SourceLocation,
    playlist: &MediaPlaylist,
    collector: &mut PacketCollector,
    hooks: &Hooks<'_>,
) -> Result<()> {
    let mut init_cache: Option<(String, Vec<u8>)> = None;
    let mut downloaded_bytes = 0_u64;
    let total_segments = playlist.segments.len();

    for (index, segment) in playlist.segments.iter().enumerate() {
        // Cooperative cancellation: check before downloading each segment.
        hooks.check_cancel()?;

        let (demuxed, segment_bytes) =
            demux_segment(reader, playlist_location, segment, &mut init_cache).await?;
        downloaded_bytes += segment_bytes;
        collector.push_demuxed(demuxed)?;

        // Mp4 batch path doesn't write to disk per-segment, so bytes_written
        // stays 0 and the resume snapshot is informational only — Mp4 output
        // does not support resume (rejected at the entry point).
        hooks.emit(TransmuxProgress {
            stage: TransmuxStage::Downloading,
            total_segments,
            completed_segments: index + 1,
            downloaded_bytes,
            bytes_written: 0,
            current_segment_index: index,
            resume: TransmuxResumeState {
                completed_segments: index + 1,
                bytes_written: 0,
                next_sequence: (index + 1) as u32,
                global_base_dts_90k: 0,
                ..Default::default()
            },
        });
    }
    Ok(())
}

impl PacketCollector {
    fn push_demuxed(&mut self, demuxed: DemuxOutput) -> Result<()> {
        if let Some(segment_vps) = demuxed.vps {
            update_param(
                &mut self.vps,
                segment_vps,
                "mid-stream HEVC VPS changes are out of Phase 3 scope",
            )?;
        }
        if let Some(segment_sps) = demuxed.sps {
            update_param(
                &mut self.sps,
                segment_sps,
                "mid-stream SPS changes are out of Phase 3 scope",
            )?;
        }
        if let Some(segment_pps) = demuxed.pps {
            update_param(
                &mut self.pps,
                segment_pps,
                "mid-stream PPS changes are out of Phase 3 scope",
            )?;
        }
        if let Some(config) = demuxed.audio_specific_config {
            update_param(
                &mut self.audio_specific_config,
                config,
                "mid-stream AAC config changes are out of Phase 3 scope",
            )?;
        }
        if let Some(rate) = demuxed.sample_rate {
            if self.sample_rate.is_some_and(|existing| existing != rate) {
                return Err(Error::unsupported(
                    "mid-stream AAC sample rate changes are out of Phase 3 scope",
                ));
            }
            self.sample_rate = Some(rate);
        }
        if let Some(channels) = demuxed.channel_count {
            if self
                .channel_count
                .is_some_and(|existing| existing != channels)
            {
                return Err(Error::unsupported(
                    "mid-stream AAC channel count changes are out of Phase 3 scope",
                ));
            }
            self.channel_count = Some(channels);
        }

        self.packets.extend(demuxed.packets);
        Ok(())
    }
}

fn update_param(slot: &mut Option<Vec<u8>>, new_value: Vec<u8>, conflict_msg: &str) -> Result<()> {
    if let Some(existing) = slot {
        if existing != &new_value {
            return Err(Error::unsupported(conflict_msg));
        }
    } else {
        *slot = Some(new_value);
    }
    Ok(())
}

fn mux_collected_packets(
    collector: PacketCollector,
    segment_count: usize,
) -> Result<(Vec<u8>, TransmuxReport)> {
    mux_collected_packets_checked(collector, segment_count, &|| Ok(()))
}

fn mux_collected_packets_checked(
    collector: PacketCollector,
    segment_count: usize,
    check: &dyn Fn() -> Result<()>,
) -> Result<(Vec<u8>, TransmuxReport)> {
    check()?;
    let PacketCollector {
        packets,
        vps,
        sps,
        pps,
        audio_specific_config,
        sample_rate,
        channel_count,
    } = collector;

    if packets.is_empty() {
        return Err(Error::invalid(
            "HLS playlist did not produce any encoded packets",
        ));
    }

    let base_dts = packets
        .iter()
        .map(|packet| packet.dts_90k)
        .min()
        .ok_or_else(|| Error::invalid("HLS playlist did not produce any encoded packets"))?;
    let sample_rate = sample_rate
        .ok_or_else(|| Error::unsupported("AAC audio is required for non-fragmented MP4"))?;
    let channel_count = channel_count
        .ok_or_else(|| Error::unsupported("AAC audio is required for non-fragmented MP4"))?;

    let mut video_samples = Vec::new();
    let mut audio_samples = Vec::new();
    for packet in packets {
        check()?;
        if packet.dts_90k < base_dts || packet.pts_90k < base_dts {
            return Err(Error::muxing(
                "packet timestamps precede the normalization base",
            ));
        }

        match packet.kind {
            StreamKind::Avc | StreamKind::Hevc => {
                let data = if packet.is_length_prefixed {
                    packet.data
                } else {
                    // Annex B — reuse the AVC start-code scanner for both codecs
                    // (HEVC and AVC share the 0x000001 / 0x00000001 start code syntax).
                    avc::annex_b_to_length_prefixed(&packet.data)?
                };
                video_samples.push(Mp4Sample {
                    data,
                    dts: packet.dts_90k - base_dts,
                    pts: packet.pts_90k - base_dts,
                    duration: 0,
                    is_key: packet.is_key,
                    offset: 0,
                });
            }
            StreamKind::Aac => {
                let dts = rescale_90k(packet.dts_90k - base_dts, sample_rate);
                let pts = rescale_90k(packet.pts_90k - base_dts, sample_rate);
                audio_samples.push(Mp4Sample {
                    data: packet.data,
                    dts,
                    pts,
                    duration: u32::try_from(packet.duration)
                        .map_err(|_| Error::muxing("AAC packet duration exceeds u32"))?,
                    is_key: true,
                    offset: 0,
                });
            }
        }
    }

    let audio_specific_config = audio_specific_config
        .ok_or_else(|| Error::bitstream("AAC AudioSpecificConfig was not found"))?;

    let video_track = if let Some(vps) = vps {
        let sps = sps.ok_or_else(|| Error::bitstream("HEVC SPS was not found"))?;
        let pps = pps.ok_or_else(|| Error::bitstream("HEVC PPS was not found"))?;
        make_hevc_video_track(video_samples, &vps, &sps, &pps)?
    } else {
        let sps = sps.ok_or_else(|| Error::bitstream("H.264 SPS was not found"))?;
        let pps = pps.ok_or_else(|| Error::bitstream("H.264 PPS was not found"))?;
        make_video_track(video_samples, &sps, &pps)?
    };

    let tracks = vec![
        video_track,
        make_audio_track(
            audio_samples,
            sample_rate,
            channel_count,
            audio_specific_config,
        )?,
    ];
    let (mp4, track_infos) = Mp4Muxer::new(tracks).write_checked(check)?;

    let duration = track_infos
        .iter()
        .map(|track| track.duration.saturating_mul(1000) / u64::from(track.timescale))
        .max()
        .unwrap_or(0);

    Ok((
        mp4,
        TransmuxReport {
            segment_count,
            tracks: track_infos,
            duration,
            duration_timescale: 1000,
            bytes_written: 0,
        },
    ))
}

fn rescale_90k(value: u64, to_timescale: u32) -> u64 {
    value.saturating_mul(u64::from(to_timescale)) / 90_000
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "default-source")]
    use std::fs;

    use super::*;
    use crate::hls::VariantStream;

    fn variant(uri: &str, bandwidth: Option<u64>) -> VariantStream {
        VariantStream {
            uri: uri.to_string(),
            path: PathBuf::new(),
            bandwidth,
            resolution: None,
            codecs: None,
        }
    }

    fn master(variants: Vec<VariantStream>) -> MasterPlaylist {
        MasterPlaylist {
            path: PathBuf::new(),
            variants,
        }
    }

    #[test]
    fn rescales_90k_to_audio_timescale() {
        assert_eq!(rescale_90k(90_000, 44_100), 44_100);
    }

    #[test]
    fn variant_selection_index_returns_specified() {
        let m = master(vec![
            variant("a.m3u8", Some(100)),
            variant("b.m3u8", Some(200)),
        ]);
        assert_eq!(VariantSelection::Index(0).select_index(&m).unwrap(), 0);
        assert_eq!(VariantSelection::Index(1).select_index(&m).unwrap(), 1);
    }

    #[test]
    fn variant_selection_index_out_of_range_errors() {
        let m = master(vec![variant("a.m3u8", Some(100))]);
        let err = VariantSelection::Index(5).select_index(&m).unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));
    }

    #[test]
    fn variant_selection_highest_bandwidth_picks_max() {
        let m = master(vec![
            variant("low.m3u8", Some(500_000)),
            variant("mid.m3u8", Some(1_500_000)),
            variant("high.m3u8", Some(3_000_000)),
        ]);
        assert_eq!(
            VariantSelection::HighestBandwidth.select_index(&m).unwrap(),
            2
        );
    }

    #[test]
    fn variant_selection_lowest_bandwidth_picks_min() {
        let m = master(vec![
            variant("low.m3u8", Some(500_000)),
            variant("mid.m3u8", Some(1_500_000)),
            variant("high.m3u8", Some(3_000_000)),
        ]);
        assert_eq!(
            VariantSelection::LowestBandwidth.select_index(&m).unwrap(),
            0
        );
    }

    #[test]
    fn variant_selection_highest_bandwidth_with_missing_bandwidth_picks_real() {
        // Variants without BANDWIDTH are treated as 0 for HighestBandwidth,
        // so a real-bandwidth variant always wins. When ALL lack bandwidth,
        // max_by_key returns the last (Rust tie-breaking).
        let all_missing = master(vec![variant("a.m3u8", None), variant("b.m3u8", None)]);
        assert_eq!(
            VariantSelection::HighestBandwidth
                .select_index(&all_missing)
                .unwrap(),
            1
        );

        let mixed = master(vec![
            variant("a.m3u8", None),
            variant("b.m3u8", Some(1_000_000)),
            variant("c.m3u8", None),
        ]);
        assert_eq!(
            VariantSelection::HighestBandwidth
                .select_index(&mixed)
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    #[cfg(feature = "default-source")]
    async fn rejects_master_without_variant() {
        let temp_dir =
            std::env::temp_dir().join(format!("hls-transmux-master-test-{}", std::process::id()));
        fs::create_dir_all(&temp_dir).unwrap();
        let master = temp_dir.join("master.m3u8");
        fs::write(
            &master,
            "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nmedia.m3u8\n",
        )
        .unwrap();

        let err = transmux_hls_to_mp4_async(
            HlsInput::Path(master),
            temp_dir.join("out.mp4"),
            TransmuxOptions::default(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));
    }
    #[tokio::test]
    async fn sync_failure_does_not_publish_checkpoint() {
        let source = crate::MemorySource::new()
            .text(
                "playlist.m3u8",
                "#EXTM3U\n#EXT-X-TARGETDURATION:10\n#EXTINF:10,\nsegment.ts\n#EXT-X-ENDLIST\n",
            )
            .segment(
                "segment.ts",
                include_bytes!("../tests/fixtures/h264_aac_fhd.ts").to_vec(),
            );
        let location = SourceLocation::File("playlist.m3u8".into());
        let reader = SourceReader::new(Arc::new(source), None);
        let resource = reader.read_text(&location).await.unwrap();
        let (playlist, location) = resolve_media_playlist(&reader, &resource, None)
            .await
            .unwrap();
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let saved = events.clone();
        let options = TransmuxOptions {
            output_format: OutputFormat::FragmentedMp4,
            on_progress: Some(Arc::new(move |p| saved.lock().unwrap().push(p))),
            ..Default::default()
        };
        let hooks = Hooks {
            on_progress: options.on_progress.as_ref(),
            cancel: None,
            options: &options,
            #[cfg(not(target_arch = "wasm32"))]
            sync_file: None,
            sync_failure: true,
        };
        let mut bytes = Vec::new();
        let result = transmux_fragmented_to_writer(
            &reader, &location, &playlist, &mut bytes, &hooks, None, None,
        )
        .await;
        assert!(matches!(result, Err(Error::Io(_))));
        assert!(!bytes.is_empty());
        assert!(events.lock().unwrap().is_empty());
    }
}
