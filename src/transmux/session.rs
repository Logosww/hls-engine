//! Prepared, demand-driven media sessions. Legacy resume stays in the old pipeline.
mod keyed;
mod timeline;
use super::*;
use crate::source::{ByteRange, Source, SourceSessionOptions, safe_location};
use crate::types::PacketTiming;
pub use keyed::*;
use std::collections::VecDeque;
pub use timeline::*;
use tokio::io::{AsyncWrite, AsyncWriteExt};

/// The role of a selected media playlist (not a language or rendition selector).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputRole {
    Primary,
    Audio,
}
/// Resources and operations that may fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    Preparing,
    Playlist,
    Initialization,
    Downloading,
    Processing,
    Writing,
    Finalizing,
    Completed,
}
/// Structured context without changing the legacy exhaustive `Error` enum.
#[derive(Debug)]
pub struct SessionError {
    error: Error,
    role: Option<InputRole>,
    phase: SessionPhase,
    segment_index: Option<usize>,
    resource: Option<String>,
    byte_range: Option<ByteRange>,
    keyed_cause: Option<Box<crate::crypto::resource::ResourceError>>,
}
impl SessionError {
    fn new(error: Error, phase: SessionPhase) -> Self {
        Self {
            error,
            role: None,
            phase,
            segment_index: None,
            resource: None,
            byte_range: None,
            keyed_cause: None,
        }
    }
    pub fn error(&self) -> &Error {
        &self.error
    }
    pub fn into_error(self) -> Error {
        self.error
    }
    pub fn role(&self) -> Option<InputRole> {
        self.role
    }
    pub fn phase(&self) -> SessionPhase {
        self.phase
    }
    pub fn segment_index(&self) -> Option<usize> {
        self.segment_index
    }
    pub fn resource(&self) -> Option<&str> {
        self.resource.as_deref()
    }
    pub fn byte_range(&self) -> Option<ByteRange> {
        self.byte_range
    }
}
impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:?} {:?} segment {:?}: {}",
            self.role, self.phase, self.segment_index, self.error
        )
    }
}
impl std::error::Error for SessionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}
pub type SessionResult<T> = std::result::Result<T, SessionError>;

/// Selected media inputs. The prepared API deliberately does not select masters.
#[derive(Debug)]
pub struct HlsInputs {
    primary: HlsInput,
    audio: Option<HlsInput>,
}
impl HlsInputs {
    pub fn new(primary: HlsInput) -> Self {
        Self {
            primary,
            audio: None,
        }
    }
    pub fn with_audio(mut self, audio: HlsInput) -> Self {
        self.audio = Some(audio);
        self
    }
}
/// Request/segment bounds, not a process-wide byte budget.
#[derive(Debug, Clone)]
pub struct ResourceBudget {
    reads: usize,
    probe: usize,
    bytes: Option<u64>,
}
impl Default for ResourceBudget {
    fn default() -> Self {
        Self {
            reads: 2,
            probe: 2,
            bytes: None,
        }
    }
}
impl ResourceBudget {
    pub fn with_max_in_flight_reads(mut self, value: usize) -> Self {
        self.reads = value;
        self
    }
    pub fn with_max_probe_segments_per_input(mut self, value: usize) -> Self {
        self.probe = value;
        self
    }
    pub fn with_max_resource_bytes(mut self, value: u64) -> Self {
        self.bytes = Some(value);
        self
    }
    pub fn max_in_flight_reads(&self) -> usize {
        self.reads
    }
    pub fn max_probe_segments_per_input(&self) -> usize {
        self.probe
    }
    pub fn max_resource_bytes(&self) -> Option<u64> {
        self.bytes
    }
}
/// Per-input counters. Initialization reads are excluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputProgress {
    role: InputRole,
    total: usize,
    downloaded: usize,
    processed: usize,
    downloaded_bytes: u64,
}
impl InputProgress {
    pub fn role(&self) -> InputRole {
        self.role
    }
    pub fn total_segments(&self) -> usize {
        self.total
    }
    pub fn downloaded_segments(&self) -> usize {
        self.downloaded
    }
    pub fn processed_segments(&self) -> usize {
        self.processed
    }
    pub fn downloaded_bytes(&self) -> u64 {
        self.downloaded_bytes
    }
}
#[derive(Debug, Clone)]
pub struct SessionEvent {
    phase: SessionPhase,
    role: Option<InputRole>,
    index: Option<usize>,
    inputs: Vec<InputProgress>,
    bytes: u64,
}
impl SessionEvent {
    pub fn phase(&self) -> SessionPhase {
        self.phase
    }
    pub fn role(&self) -> Option<InputRole> {
        self.role
    }
    pub fn segment_index(&self) -> Option<usize> {
        self.index
    }
    pub fn inputs(&self) -> &[InputProgress] {
        &self.inputs
    }
    pub fn total_segments(&self) -> usize {
        self.inputs.iter().map(|p| p.total).sum()
    }
    pub fn processed_segments(&self) -> usize {
        self.inputs.iter().map(|p| p.processed).sum()
    }
    pub fn downloaded_segments(&self) -> usize {
        self.inputs.iter().map(|p| p.downloaded).sum()
    }
    pub fn bytes_written(&self) -> u64 {
        self.bytes
    }
}
#[cfg(not(target_arch = "wasm32"))]
type CoreEventCallback = dyn Fn(SessionEvent) + Send + Sync;
#[cfg(target_arch = "wasm32")]
type CoreEventCallback = dyn Fn(SessionEvent);
#[derive(Clone, Default)]
pub struct PrepareOptions {
    cancel: Option<Arc<dyn CancelToken>>,
    on_event: Option<Arc<CoreEventCallback>>,
    budget: ResourceBudget,
    write_mfra: bool,
}
impl PrepareOptions {
    pub fn with_cancel(mut self, value: Arc<dyn CancelToken>) -> Self {
        self.cancel = Some(value);
        self
    }
    pub fn with_on_event(mut self, value: Arc<dyn Fn(SessionEvent) + Send + Sync>) -> Self {
        self.on_event = Some(value);
        self
    }
    pub fn with_budget(mut self, value: ResourceBudget) -> Self {
        self.budget = value;
        self
    }
    pub fn with_write_mfra(mut self, value: bool) -> Self {
        self.write_mfra = value;
        self
    }
}
impl TryFrom<TransmuxOptions> for PrepareOptions {
    type Error = SessionError;
    fn try_from(value: TransmuxOptions) -> SessionResult<Self> {
        if value.resume.is_some() || value.on_progress.is_some() || value.variant.is_some() {
            return Err(SessionError::new(
                Error::invalid(
                    "prepared sessions do not support resume, checkpoint callbacks or master selection",
                ),
                SessionPhase::Preparing,
            ));
        }
        Ok(Self {
            cancel: value.cancel,
            write_mfra: value.write_mfra,
            ..Self::default()
        })
    }
}
#[derive(Debug, Clone)]
pub struct FileOutputOptions {
    format: OutputFormat,
    backend: FinalizeBackend,
}
impl Default for FileOutputOptions {
    fn default() -> Self {
        Self {
            format: OutputFormat::StreamingMp4,
            backend: FinalizeBackend::Native,
        }
    }
}
impl FileOutputOptions {
    pub fn with_format(mut self, format: OutputFormat) -> Self {
        self.format = format;
        self
    }
    pub fn with_finalize_backend(mut self, backend: FinalizeBackend) -> Self {
        self.backend = backend;
        self
    }
}
/// Exact signed time; no floating-point seconds are used in mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaTime {
    ticks: i128,
    timescale: u32,
}
impl MediaTime {
    pub fn new(ticks: i128, timescale: u32) -> Result<Self> {
        if timescale == 0 {
            return Err(Error::invalid("zero timescale"));
        }
        Ok(Self { ticks, timescale })
    }
    pub fn ticks(&self) -> i128 {
        self.ticks
    }
    pub fn timescale(&self) -> u32 {
        self.timescale
    }
    fn compare(self, other: Self) -> Result<std::cmp::Ordering> {
        let a = self.ticks.checked_mul(other.timescale.into());
        let b = other.ticks.checked_mul(self.timescale.into());
        let (a, b) = a
            .zip(b)
            .ok_or_else(|| Error::muxing("timestamp comparison overflow"))?;
        Ok(a.cmp(&b))
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackTimeline {
    role: InputRole,
    track_type: crate::TrackType,
    timescale: u32,
    edit_offset: i128,
    wrap_anchor: Option<i128>,
}
impl TrackTimeline {
    pub fn role(&self) -> InputRole {
        self.role
    }
    pub fn track_type(&self) -> crate::TrackType {
        self.track_type
    }
    pub fn timescale(&self) -> u32 {
        self.timescale
    }
    pub fn edit_offset(&self) -> i128 {
        self.edit_offset
    }
    pub fn wrap_anchor(&self) -> Option<i128> {
        self.wrap_anchor
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineMapping {
    origin: MediaTime,
    tracks: Vec<TrackTimeline>,
}
impl TimelineMapping {
    pub fn origin(&self) -> MediaTime {
        self.origin
    }
    pub fn tracks(&self) -> &[TrackTimeline] {
        &self.tracks
    }
    pub fn ts_wrap_period(&self) -> u64 {
        1 << 33
    }
    /// Map an already unwrapped, edit-adjusted media time. Rounds toward zero.
    pub fn to_output(&self, media: MediaTime, output_timescale: u32) -> Result<i128> {
        if output_timescale == 0 {
            return Err(Error::invalid("zero output timescale"));
        }
        media
            .ticks
            .checked_mul(self.origin.timescale.into())
            .and_then(|a| {
                self.origin
                    .ticks
                    .checked_mul(media.timescale.into())
                    .and_then(|b| a.checked_sub(b))
            })
            .and_then(|v| v.checked_mul(output_timescale.into()))
            .map(|v| v / (i128::from(media.timescale) * i128::from(self.origin.timescale)))
            .ok_or_else(|| Error::muxing("timestamp mapping overflow"))
    }
    /// Unwrap a raw PES/WebVTT MPEGTS value near an explicit 90 kHz reference.
    /// Advance the reference for cues more than half a wrap period from prepare.
    pub fn unwrap_mpegts(&self, raw: u64, reference_90k: i128) -> Result<MediaTime> {
        if raw >= 1 << 33 {
            return Err(Error::invalid("MPEGTS value exceeds 33 bits"));
        }
        Ok(MediaTime {
            ticks: unwrap_near(raw as i128, reference_90k)?,
            timescale: 90_000,
        })
    }
}
#[derive(Debug, Clone)]
pub struct PreparedInfo {
    tracks: Vec<crate::TrackInfo>,
    timeline: TimelineMapping,
}
impl PreparedInfo {
    pub fn tracks(&self) -> &[crate::TrackInfo] {
        &self.tracks
    }
    pub fn timeline(&self) -> &TimelineMapping {
        &self.timeline
    }
}
#[derive(Debug, Clone)]
pub struct SessionReport {
    media: TransmuxReport,
    timeline: TimelineMapping,
    inputs: Vec<InputProgress>,
}
impl SessionReport {
    pub fn media(&self) -> &TransmuxReport {
        &self.media
    }
    pub fn timeline(&self) -> &TimelineMapping {
        &self.timeline
    }
    pub fn inputs(&self) -> &[InputProgress] {
        &self.inputs
    }
}
fn unwrap_near(raw: i128, reference: i128) -> Result<i128> {
    let period = 1i128 << 33;
    let delta = (raw.rem_euclid(period) - reference.rem_euclid(period)).rem_euclid(period);
    if delta == period / 2 {
        return Err(Error::unsupported("ambiguous TS wrap epoch"));
    }
    reference
        .checked_add(if delta > period / 2 {
            delta - period
        } else {
            delta
        })
        .ok_or_else(|| Error::bitstream("TS timestamp overflow"))
}

struct SessionSource(Arc<dyn Source>);
impl Drop for SessionSource {
    fn drop(&mut self) {
        self.0.stop_session();
    }
}
impl ClearInputResources {
    async fn read(
        &mut self,
        index: usize,
        role: InputRole,
        options: &PrepareOptions,
    ) -> SessionResult<(DemuxOutput, SourceLocation, Option<ByteRange>, u64)> {
        let segment = &self.media.segments[index];
        let context = |error, phase, location: Option<&SourceLocation>, byte_range| SessionError {
            error,
            role: Some(role),
            phase,
            segment_index: Some(index),
            resource: location.map(safe_location),
            byte_range,
            keyed_cause: None,
        };
        let location = self
            .location
            .resolve(&segment.uri)
            .map_err(|e| context(e, SessionPhase::Downloading, None, segment.byte_range))?;
        let range = segment.byte_range;
        if segment.init_segment.is_some() != self.media.segments[0].init_segment.is_some() {
            return Err(context(
                Error::unsupported("input container changes mid-playlist"),
                SessionPhase::Processing,
                Some(&location),
                range,
            ));
        }
        if let Some(spec) = &segment.init_segment {
            let init_location = self
                .location
                .resolve(&spec.uri)
                .map_err(|e| context(e, SessionPhase::Initialization, None, spec.byte_range))?;
            let key = (init_location.clone(), spec.byte_range);
            if self.init.as_ref().is_none_or(|(old, _)| old != &key) {
                let bytes = crate::cancel::wait(
                    options.cancel.as_ref(),
                    self.source
                        .0
                        .read_bytes(&init_location, spec.byte_range.as_ref()),
                )
                .await
                .map_err(|e| {
                    context(
                        e,
                        SessionPhase::Initialization,
                        Some(&init_location),
                        spec.byte_range,
                    )
                })?;
                check_size(bytes.len(), options.budget.bytes).map_err(|e| {
                    context(
                        e,
                        SessionPhase::Initialization,
                        Some(&init_location),
                        spec.byte_range,
                    )
                })?;
                self.init = Some((key, bytes));
            }
        }
        let bytes = crate::cancel::wait(
            options.cancel.as_ref(),
            self.source.0.read_bytes(&location, range.as_ref()),
        )
        .await
        .map_err(|e| context(e, SessionPhase::Downloading, Some(&location), range))?;
        check_size(bytes.len(), options.budget.bytes)
            .map_err(|e| context(e, SessionPhase::Downloading, Some(&location), range))?;
        let data = if segment.init_segment.is_some() {
            demux_isobmff(&self.init.as_ref().unwrap().1, &bytes)
        } else {
            demux_ts(&bytes)
        }
        .map_err(|e| context(e, SessionPhase::Processing, Some(&location), range))?;
        Ok((data, location, range, bytes.len() as u64))
    }
}
struct SegmentBatch {
    index: usize,
    data: DemuxOutput,
}
struct ClearInputResources {
    source: SessionSource,
    location: SourceLocation,
    media: MediaPlaylist,
    init: InitCache,
}
enum InputResources {
    Clear(Box<ClearInputResources>),
    Keyed(Box<keyed::KeyedCursor>),
}
struct InputCursor {
    role: InputRole,
    resources: InputResources,
    clock: TimestampClock,
    config: Option<DemuxOutput>,
    pending: VecDeque<SegmentBatch>,
    progress: InputProgress,
    next: usize,
    external: bool,
    shift: i128,
    last_video_delta: u64,
    probing: bool,
    ts_last: [Option<u64>; 2],
}
impl InputCursor {
    fn error(
        &self,
        error: Error,
        phase: SessionPhase,
        index: Option<usize>,
        location: Option<&SourceLocation>,
        range: Option<ByteRange>,
    ) -> SessionError {
        let resolved = index.and_then(|i| match &self.resources {
            InputResources::Clear(clear) => clear.media.segments.get(i).and_then(|s| {
                clear
                    .location
                    .resolve(&s.uri)
                    .ok()
                    .map(|l| (l, s.byte_range))
            }),
            InputResources::Keyed(keyed) => keyed.snapshot.segments().get(i).map(|s| {
                (
                    s.location().location().clone(),
                    s.range().map(|r| r.byte_range()),
                )
            }),
        });
        SessionError {
            error,
            role: Some(self.role),
            phase,
            segment_index: index,
            resource: location
                .or(resolved.as_ref().map(|r| &r.0))
                .map(safe_location),
            byte_range: range.or_else(|| resolved.and_then(|r| r.1)),
            keyed_cause: None,
        }
    }
    async fn open(
        input: HlsInput,
        role: InputRole,
        external: bool,
        options: &PrepareOptions,
    ) -> SessionResult<Self> {
        let (root, source) = input.into_parts().map_err(|e| {
            let mut error = SessionError::new(e, SessionPhase::Playlist);
            error.role = Some(role);
            error
        })?;
        let settings = SourceSessionOptions {
            demand_driven: true,
            max_resource_bytes: options.budget.bytes,
        };
        let source = SessionSource(
            source
                .create_session_with_options(&settings)
                .unwrap_or(source),
        );
        let context = |error| SessionError {
            error,
            role: Some(role),
            phase: SessionPhase::Playlist,
            segment_index: None,
            resource: Some(safe_location(&root)),
            byte_range: None,
            keyed_cause: None,
        };
        let text = crate::cancel::wait(options.cancel.as_ref(), source.0.read_text(&root))
            .await
            .map_err(context)?;
        check_size(text.content.len(), options.budget.bytes).map_err(context)?;
        let media = match parse_hls_playlist_content(None, &text.content).map_err(context)? {
            HlsPlaylist::Media(media) => media,
            HlsPlaylist::Master(_) => {
                return Err(context(Error::invalid(
                    "prepare_hls requires selected media playlists",
                )));
            }
        };
        if media.segments.is_empty() {
            return Err(context(Error::invalid("empty media playlist")));
        }
        let total = media.segments.len();
        Ok(Self {
            role,
            resources: InputResources::Clear(Box::new(ClearInputResources {
                source,
                location: text.location,
                media,
                init: None,
            })),
            clock: TimestampClock::default(),
            config: None,
            pending: VecDeque::new(),
            progress: InputProgress {
                role,
                total,
                downloaded: 0,
                processed: 0,
                downloaded_bytes: 0,
            },
            next: 0,
            external,
            shift: 0,
            last_video_delta: 3000,
            probing: true,
            ts_last: [None, None],
        })
    }
    async fn read(&mut self, options: &PrepareOptions) -> SessionResult<()> {
        if self.next == self.progress.total {
            return Ok(());
        }
        let role = self.role;
        let index = self.next;
        let (mut data, location, range, size) = match &mut self.resources {
            InputResources::Clear(clear) => clear.read(index, role, options).await?,
            InputResources::Keyed(keyed) => keyed.read(index, role, options).await?,
        };
        self.progress.downloaded += 1;
        self.progress.downloaded_bytes += size;
        if self.external {
            select_track(&mut data, self.role);
        }
        data.saw_video |= data.sps.is_some() || data.vps.is_some();
        data.saw_audio |= data.audio_specific_config.is_some();
        if let Some(old) = &self.config {
            if data.saw_video && data.video_timescale.is_none() {
                data.video_timescale = old.video_timescale;
            }
            if data.saw_audio && data.audio_timescale.is_none() {
                data.audio_timescale = old.audio_timescale;
            }
        }
        self.clock.normalize(&mut data).map_err(|e| {
            self.error(
                e,
                SessionPhase::Processing,
                Some(self.next),
                Some(&location),
                range,
            )
        })?;
        for packet in &data.packets {
            if packet.timing.is_none() {
                let track = usize::from(matches!(packet.kind, StreamKind::Aac));
                if self.ts_last[track]
                    .is_some_and(|last| packet.dts_90k.saturating_sub(last) >= 1 << 32)
                {
                    return Err(self.error(
                        Error::unsupported("TS advancement exceeds unambiguous half-wrap interval"),
                        SessionPhase::Processing,
                        Some(self.next),
                        Some(&location),
                        range,
                    ));
                }
                self.ts_last[track] = Some(packet.dts_90k);
            }
        }
        let validation = if self.probing {
            data.saw_video |= data.sps.is_some() || data.vps.is_some();
            data.saw_audio |= data.audio_specific_config.is_some();
            merge_probe_config(&mut self.config, &mut data)
        } else {
            check_media_config(&mut self.config, &data)
        };
        validation.map_err(|e| {
            self.error(
                e,
                SessionPhase::Processing,
                Some(self.next),
                Some(&location),
                range,
            )
        })?;
        self.pending.push_back(SegmentBatch {
            index: self.next,
            data,
        });
        self.next += 1;
        Ok(())
    }
    async fn probe(&mut self, options: &PrepareOptions) -> SessionResult<()> {
        for _ in 0..options.budget.probe {
            self.read(options).await?;
            if let Some(config) = &self.config
                && build_fragmented_tracks(config).is_ok()
                && (!config.saw_video
                    || self
                        .pending
                        .iter()
                        .flat_map(|b| &b.data.packets)
                        .any(|p| !matches!(p.kind, StreamKind::Aac)))
                && (!config.saw_audio
                    || self
                        .pending
                        .iter()
                        .flat_map(|b| &b.data.packets)
                        .any(|p| matches!(p.kind, StreamKind::Aac)))
            {
                self.probing = false;
                return Ok(());
            }
            // Empty segments own no samples, but remain for progress accounting.
            if self.next == self.progress.total {
                break;
            }
        }
        Err(self.error(
            Error::unsupported(
                "probe limit reached without required track configuration and samples",
            ),
            SessionPhase::Preparing,
            self.next.checked_sub(1),
            None,
            None,
        ))
    }
    fn first_time(&self) -> Option<MediaTime> {
        self.pending
            .iter()
            .flat_map(|b| &b.data.packets)
            .next()
            .map(|p| packet_time(p, self.shift))
    }
    async fn fill(&mut self, options: &PrepareOptions) -> SessionResult<()> {
        if self.pending.is_empty() {
            self.read(options).await?;
        }
        let needs_next = self.pending.front().is_some_and(|b| {
            b.data
                .packets
                .iter()
                .any(|p| p.timing.is_none() && !matches!(p.kind, StreamKind::Aac))
        });
        if needs_next && self.pending.len() == 1 && self.next < self.progress.total {
            self.read(options).await?;
        }
        Ok(())
    }
    fn take(&mut self) -> Result<Option<SegmentBatch>> {
        let Some(mut batch) = self.pending.pop_front() else {
            return Ok(None);
        };
        if let Some(next) = self.pending.front() {
            set_boundary_duration(&mut batch.data, &next.data)?;
        }
        let video_indices: Vec<_> = batch
            .data
            .packets
            .iter()
            .enumerate()
            .filter(|(_, p)| p.timing.is_none() && !matches!(p.kind, StreamKind::Aac))
            .map(|(i, _)| i)
            .collect();
        for pair in video_indices.windows(2) {
            batch.data.packets[pair[0]].duration = batch.data.packets[pair[1]]
                .dts_90k
                .checked_sub(batch.data.packets[pair[0]].dts_90k)
                .filter(|d| *d > 0)
                .ok_or_else(|| Error::unsupported("non-increasing video DTS"))?;
        }
        for packet in &mut batch.data.packets {
            if packet.timing.is_none() {
                let duration = if matches!(packet.kind, StreamKind::Aac) {
                    packet.duration
                } else {
                    if packet.duration > 0 {
                        self.last_video_delta = packet.duration;
                    }
                    self.last_video_delta
                };
                // TS AAC duration is expressed in output audio samples, not 90k ticks.
                let duration_90k = if matches!(packet.kind, StreamKind::Aac) {
                    duration
                        .checked_mul(90_000)
                        .ok_or_else(|| Error::muxing("duration overflow"))?
                        / u64::from(batch.data.sample_rate.unwrap_or(48_000))
                } else {
                    duration
                };
                packet.timing = Some(PacketTiming {
                    edit_offset: 0,
                    timescale: 90_000,
                    dts: i128::from(packet.dts_90k) + self.shift,
                    pts: packet.pts_90k + self.shift,
                    duration: u32::try_from(duration_90k)
                        .map_err(|_| Error::muxing("duration overflow"))?,
                });
            }
        }
        Ok(Some(batch))
    }
}
fn merge_probe_config(previous: &mut Option<DemuxOutput>, current: &mut DemuxOutput) -> Result<()> {
    if let Some(old) = previous.as_ref() {
        macro_rules! merge {
            ($($field:ident),*) => {$(
                if let (Some(a), Some(b)) = (&old.$field, &current.$field) && a != b {
                    return Err(Error::unsupported("configuration changed during probe"));
                }
                if current.$field.is_none() { current.$field = old.$field.clone(); }
            )*};
        }
        merge!(
            video_timescale,
            audio_timescale,
            vps,
            sps,
            pps,
            width,
            height,
            audio_specific_config,
            sample_rate,
            channel_count
        );
        current.saw_video |= old.saw_video;
        current.saw_audio |= old.saw_audio;
    }
    *previous = None;
    check_media_config(previous, current)
}

fn packet_time(packet: &EncodedPacket, shift: i128) -> MediaTime {
    packet.timing.map_or(
        MediaTime {
            ticks: i128::from(packet.dts_90k) + shift,
            timescale: 90_000,
        },
        |t| MediaTime {
            ticks: t.dts,
            timescale: t.timescale,
        },
    )
}
fn select_track(data: &mut DemuxOutput, role: InputRole) {
    let audio = role == InputRole::Audio;
    data.packets
        .retain(|p| matches!(p.kind, StreamKind::Aac) == audio);
    if audio {
        data.saw_video = false;
        data.video_timescale = None;
        data.vps = None;
        data.sps = None;
        data.pps = None;
        data.width = None;
        data.height = None;
    } else {
        data.saw_audio = false;
        data.audio_timescale = None;
        data.audio_specific_config = None;
        data.sample_rate = None;
        data.channel_count = None;
    }
}
fn check_size(size: usize, limit: Option<u64>) -> Result<()> {
    if limit.is_some_and(|limit| size as u64 > limit) {
        Err(Error::invalid("resource exceeds configured byte limit"))
    } else {
        Ok(())
    }
}

/// A single-use operation. Dropping it stops both source sessions.
pub struct PreparedTransmux {
    inputs: Vec<InputCursor>,
    options: PrepareOptions,
    info: PreparedInfo,
    tracks: Vec<FragmentedTrack>,
    decode_offsets: Vec<u64>,
    ends: Vec<Option<u64>>,
    reports: Vec<crate::TrackInfo>,
    bytes: u64,
}
/// Probe selected VOD media playlists without writing output or downloading all media.
pub async fn prepare_hls(
    inputs: HlsInputs,
    options: PrepareOptions,
) -> SessionResult<PreparedTransmux> {
    if options.budget.reads == 0 || options.budget.probe == 0 || options.budget.bytes == Some(0) {
        return Err(SessionError::new(
            Error::invalid("resource budgets must be positive"),
            SessionPhase::Preparing,
        ));
    }
    let external = inputs.audio.is_some();
    let mut cursors =
        vec![InputCursor::open(inputs.primary, InputRole::Primary, external, &options).await?];
    if let Some(audio) = inputs.audio {
        cursors.push(InputCursor::open(audio, InputRole::Audio, true, &options).await?);
    }
    prepare_cursors(cursors, options).await
}
async fn prepare_cursors(
    mut cursors: Vec<InputCursor>,
    options: PrepareOptions,
) -> SessionResult<PreparedTransmux> {
    if cursors.len() == 2 && options.budget.reads >= 2 {
        let (left, right) = cursors.split_at_mut(1);
        tokio::try_join!(left[0].probe(&options), right[0].probe(&options))?;
    } else {
        for cursor in &mut cursors {
            cursor.probe(&options).await?;
        }
    }
    let failure = |e| SessionError::new(e, SessionPhase::Preparing);
    // An explicit fMP4 epoch is authoritative; otherwise the primary anchors TS.
    let anchor = cursors
        .iter()
        .flat_map(|c| c.pending.iter())
        .flat_map(|b| &b.data.packets)
        .find_map(|p| {
            p.timing.map(|t| MediaTime {
                ticks: t.dts,
                timescale: t.timescale,
            })
        })
        .or_else(|| cursors[0].first_time())
        .ok_or_else(|| failure(Error::invalid("no media samples")))?;
    let anchor_90k = anchor
        .ticks
        .checked_mul(90_000)
        .ok_or_else(|| failure(Error::muxing("timestamp overflow")))?
        / i128::from(anchor.timescale);
    for cursor in &mut cursors {
        if let Some(packet) = cursor
            .pending
            .iter()
            .flat_map(|b| &b.data.packets)
            .find(|p| p.timing.is_none())
        {
            cursor.shift = unwrap_near(i128::from(packet.dts_90k), anchor_90k).map_err(failure)?
                - i128::from(packet.dts_90k);
        }
    }
    let mut origin = anchor;
    for cursor in &cursors {
        for packet in cursor.pending.iter().flat_map(|b| &b.data.packets) {
            let time = packet_time(packet, cursor.shift);
            if time.compare(origin).map_err(failure)?.is_lt() {
                origin = time;
            }
        }
    }
    let mut tracks = Vec::new();
    let mut decode_offsets = Vec::new();
    let mut timeline = TimelineMapping {
        origin,
        tracks: Vec::new(),
    };
    for cursor in &cursors {
        let config = cursor
            .config
            .as_ref()
            .ok_or_else(|| failure(Error::invalid("missing track config")))?;
        for mut track in build_fragmented_tracks(config).map_err(failure)? {
            track.track_id = tracks.len() as u32 + 1;
            let audio = matches!(track.kind, crate::mp4::FragmentedTrackKind::Audio { .. });
            let packet = cursor
                .pending
                .iter()
                .flat_map(|b| &b.data.packets)
                .find(|p| matches!(p.kind, StreamKind::Aac) == audio)
                .ok_or_else(|| failure(Error::invalid("configured track has no samples")))?;
            let offset = timeline
                .to_output(packet_time(packet, cursor.shift), track.timescale)
                .map_err(failure)?;
            decode_offsets.push(
                u64::try_from(offset)
                    .map_err(|_| failure(Error::muxing("track decode offset exceeds u64")))?,
            );
            timeline.tracks.push(TrackTimeline {
                role: cursor.role,
                track_type: if audio {
                    crate::TrackType::Audio
                } else {
                    crate::TrackType::Video
                },
                timescale: packet.timing.map_or(90_000, |t| t.timescale),
                edit_offset: packet.timing.map_or(0, |t| t.edit_offset),
                wrap_anchor: if packet.timing.is_none() {
                    Some(i128::from(packet.dts_90k) + cursor.shift)
                } else {
                    None
                },
            });
            tracks.push(track);
        }
    }
    let reports: Vec<_> = tracks.iter().map(|t| track_report(t, 0, 0)).collect();
    let result = PreparedTransmux {
        ends: vec![None; tracks.len()],
        info: PreparedInfo {
            tracks: reports.clone(),
            timeline,
        },
        reports,
        inputs: cursors,
        options,
        tracks,
        decode_offsets,
        bytes: 0,
    };
    result.emit(SessionPhase::Preparing, None, None)?;
    Ok(result)
}
impl PreparedTransmux {
    pub fn info(&self) -> &PreparedInfo {
        &self.info
    }
    fn check(&self) -> Result<()> {
        check_cancel(self.options.cancel.as_ref())
    }
    fn emit(
        &self,
        phase: SessionPhase,
        role: Option<InputRole>,
        index: Option<usize>,
    ) -> SessionResult<()> {
        self.check().map_err(|e| SessionError::new(e, phase))?;
        if let Some(callback) = &self.options.on_event {
            callback(SessionEvent {
                phase,
                role,
                index,
                inputs: self.inputs.iter().map(|c| c.progress.clone()).collect(),
                bytes: self.bytes,
            });
        }
        Ok(())
    }
    fn report(&self) -> SessionReport {
        SessionReport {
            media: TransmuxReport {
                segment_count: self.inputs.iter().map(|c| c.progress.total).sum(),
                tracks: self.reports.clone(),
                duration: self
                    .reports
                    .iter()
                    .map(|t| (u128::from(t.duration) * 1000 / u128::from(t.timescale)) as u64)
                    .max()
                    .unwrap_or(0),
                duration_timescale: 1000,
                bytes_written: self.bytes,
            },
            timeline: self.info.timeline.clone(),
            inputs: self.inputs.iter().map(|c| c.progress.clone()).collect(),
        }
    }
    async fn next_samples(&mut self) -> SessionResult<Option<(usize, usize, Vec<Vec<Mp4Sample>>)>> {
        self.check()
            .map_err(|e| SessionError::new(e, SessionPhase::Processing))?;
        if self.inputs.len() == 2 && self.options.budget.reads >= 2 {
            let (left, right) = self.inputs.split_at_mut(1);
            tokio::try_join!(left[0].fill(&self.options), right[0].fill(&self.options))?;
        } else {
            for input in &mut self.inputs {
                input.fill(&self.options).await?;
            }
        }
        self.emit(SessionPhase::Downloading, None, None)?;
        let mut selected = None;
        for (index, input) in self.inputs.iter().enumerate() {
            if input.pending.is_empty() {
                continue;
            }
            if let Some(previous) = selected {
                let old: &InputCursor = &self.inputs[previous];
                let time = input.first_time();
                if let (Some(a), Some(b)) = (time, old.first_time()) {
                    if !a
                        .compare(b)
                        .map_err(|e| SessionError::new(e, SessionPhase::Processing))?
                        .is_lt()
                    {
                        continue;
                    }
                } else if time.is_some() {
                    continue;
                }
            }
            selected = Some(index);
        }
        let Some(selected) = selected else {
            return Ok(None);
        };
        let input = &mut self.inputs[selected];
        let index = input.pending.front().unwrap().index;
        let is_ts = match &input.resources {
            InputResources::Clear(clear) => clear.media.segments[index].init_segment.is_none(),
            InputResources::Keyed(keyed) => keyed.snapshot.segments()[index].map().is_none(),
        };
        let batch = input
            .take()
            .map_err(|e| input.error(e, SessionPhase::Processing, Some(index), None, None))?
            .unwrap();
        let mut samples: Vec<Vec<Mp4Sample>> = self.tracks.iter().map(|_| Vec::new()).collect();
        for packet in batch.data.packets {
            let audio = matches!(packet.kind, StreamKind::Aac);
            let i = self
                .info
                .timeline
                .tracks
                .iter()
                .position(|t| {
                    t.role == input.role && (t.track_type == crate::TrackType::Audio) == audio
                })
                .ok_or_else(|| {
                    input.error(
                        Error::unsupported("unannounced track"),
                        SessionPhase::Processing,
                        Some(index),
                        None,
                        None,
                    )
                })?;
            let mapping = &self.info.timeline.tracks[i];
            if packet.timing.is_some_and(|t| {
                t.timescale != mapping.timescale || t.edit_offset != mapping.edit_offset
            }) {
                return Err(input.error(
                    Error::unsupported("track timescale or edit mapping changed"),
                    SessionPhase::Processing,
                    Some(index),
                    None,
                    None,
                ));
            }
            let original_duration = packet.duration;
            let mut sample = packet_sample(
                packet,
                self.tracks[i].timescale,
                0,
                Some((
                    self.info.timeline.origin.ticks,
                    self.info.timeline.origin.timescale,
                )),
            )
            .map_err(|e| input.error(e, SessionPhase::Processing, Some(index), None, None))?;
            if is_ts && audio {
                sample.duration = u32::try_from(original_duration).map_err(|_| {
                    input.error(
                        Error::muxing("AAC duration overflow"),
                        SessionPhase::Processing,
                        Some(index),
                        None,
                        None,
                    )
                })?;
            }
            if sample.duration == 0 {
                return Err(input.error(
                    Error::bitstream("zero sample duration"),
                    SessionPhase::Processing,
                    Some(index),
                    None,
                    None,
                ));
            }
            if let Some(end) = self.ends[i] {
                if sample.dts.abs_diff(end) > 1 {
                    return Err(input.error(
                        Error::unsupported("non-contiguous track DTS"),
                        SessionPhase::Processing,
                        Some(index),
                        None,
                        None,
                    ));
                }
                sample.pts += i128::from(end) - i128::from(sample.dts);
                sample.dts = end;
            }
            let end = sample
                .dts
                .checked_add(u64::from(sample.duration))
                .ok_or_else(|| {
                    input.error(
                        Error::muxing("sample end overflow"),
                        SessionPhase::Processing,
                        Some(index),
                        None,
                        None,
                    )
                })?;
            self.ends[i] = Some(end);
            self.reports[i].sample_count += 1;
            self.reports[i].duration = self.reports[i]
                .duration
                .max(end)
                .max(u64::try_from(sample.pts + i128::from(sample.duration)).unwrap_or(0));
            samples[i].push(sample);
        }
        Ok(Some((selected, index, samples)))
    }
    fn committed(&mut self, input: usize, index: usize) -> SessionResult<()> {
        self.inputs[input].progress.processed += 1;
        self.emit(
            SessionPhase::Processing,
            Some(self.inputs[input].role),
            Some(index),
        )
    }
    async fn bytes_impl(&mut self) -> SessionResult<Vec<u8>> {
        let mut all: Vec<Vec<Mp4Sample>> = self.tracks.iter().map(|_| Vec::new()).collect();
        while let Some((input, index, samples)) = self.next_samples().await? {
            for (track, samples) in all.iter_mut().zip(samples) {
                track.extend(samples);
            }
            self.committed(input, index)?;
        }
        let tracks = self
            .tracks
            .clone()
            .into_iter()
            .zip(all)
            .map(|(t, samples)| t.into_classic(samples))
            .collect();
        let (bytes, reports) = Mp4Muxer::new(tracks)
            .write_checked(&|| self.check())
            .map_err(|e| SessionError::new(e, SessionPhase::Writing))?;
        self.reports = reports;
        self.bytes = bytes.len() as u64;
        Ok(bytes)
    }
    /// Classic MP4 retains all samples and the final output in memory.
    pub async fn into_mp4_bytes(mut self) -> SessionResult<(Vec<u8>, SessionReport)> {
        let bytes = self.bytes_impl().await?;
        self.emit(SessionPhase::Completed, None, None)?;
        Ok((bytes, self.report()))
    }
    async fn write_bytes<W: AsyncWrite + Unpin>(
        &mut self,
        writer: &mut W,
        bytes: &[u8],
    ) -> SessionResult<()> {
        crate::cancel::wait(self.options.cancel.as_ref(), async {
            writer.write_all(bytes).await?;
            writer.flush().await?;
            Ok(())
        })
        .await
        .map_err(|e| SessionError::new(e, SessionPhase::Writing))?;
        self.bytes = self.bytes.checked_add(bytes.len() as u64).ok_or_else(|| {
            SessionError::new(Error::muxing("output size overflow"), SessionPhase::Writing)
        })?;
        Ok(())
    }
    async fn writer_impl<W: AsyncWrite + Unpin>(&mut self, writer: &mut W) -> SessionResult<()> {
        let mut muxer = FragmentedMp4Muxer::new(self.tracks.clone());
        let header = muxer
            .write_header_with_offsets(&self.decode_offsets)
            .map_err(|e| SessionError::new(e, SessionPhase::Writing))?;
        self.write_bytes(writer, &header).await?;
        let mut entries: Vec<Vec<TfraEntry>> = self.tracks.iter().map(|_| Vec::new()).collect();
        while let Some((input, index, mut samples)) = self.next_samples().await? {
            // Reports and validation use the shared clock. Only the container
            // samples/index use media-local time, paired with the header edits.
            for (samples, offset) in samples.iter_mut().zip(&self.decode_offsets) {
                for sample in samples {
                    sample.dts = sample.dts.checked_sub(*offset).ok_or_else(|| {
                        SessionError::new(
                            Error::muxing("sample precedes track decode origin"),
                            SessionPhase::Writing,
                        )
                    })?;
                    sample.pts -= i128::from(*offset);
                }
            }
            if samples.iter().any(|s| !s.is_empty()) {
                let bytes = muxer
                    .write_fragment(&samples)
                    .map_err(|e| SessionError::new(e, SessionPhase::Writing))?;
                record_fragment_index(
                    self.options.write_mfra,
                    &mut entries,
                    &samples,
                    self.bytes,
                    &bytes,
                )
                .map_err(|e| SessionError::new(e, SessionPhase::Writing))?;
                self.write_bytes(writer, &bytes).await?;
            }
            self.committed(input, index)?;
        }
        if self.options.write_mfra {
            let index = mfra_box(&self.tracks, &entries)
                .map_err(|e| SessionError::new(e, SessionPhase::Writing))?;
            self.write_bytes(writer, &index).await?;
        }
        Ok(())
    }
    /// Incremental fMP4; flushes but never closes the caller-owned writer.
    pub async fn write_to<W: AsyncWrite + Unpin>(
        mut self,
        writer: &mut W,
    ) -> SessionResult<SessionReport> {
        self.writer_impl(writer).await?;
        self.emit(SessionPhase::Completed, None, None)?;
        Ok(self.report())
    }
}
fn check_cancel(cancel: Option<&Arc<dyn CancelToken>>) -> Result<()> {
    if cancel.is_some_and(|token| token.is_cancelled()) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
struct TemporaryFile(std::path::PathBuf);
#[cfg(not(target_arch = "wasm32"))]
impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
#[cfg(not(target_arch = "wasm32"))]
fn temporary_file(target: &Path) -> Result<(TemporaryFile, std::fs::File)> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    loop {
        let path = parent.join(format!(
            ".hls-transmux-session-{}-{}.mp4",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok((TemporaryFile(path), file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
}
#[cfg(not(target_arch = "wasm32"))]
impl PreparedTransmux {
    /// Atomic publication; StreamingMp4 retains a non-resumable partial on failure.
    pub async fn write_to_file(
        mut self,
        path: impl AsRef<Path>,
        output: FileOutputOptions,
    ) -> SessionResult<SessionReport> {
        let path = path.as_ref();
        self.check()
            .map_err(|e| SessionError::new(e, SessionPhase::Writing))?;
        let (mut temporary, file) =
            temporary_file(path).map_err(|e| SessionError::new(e, SessionPhase::Writing))?;
        let mut writer = tokio::fs::File::from_std(file);
        let result = if output.format == OutputFormat::Mp4 {
            let bytes = self.bytes_impl().await?;
            self.bytes = 0;
            self.write_bytes(&mut writer, &bytes).await
        } else {
            self.writer_impl(&mut writer).await
        };
        drop(writer);
        if let Err(error) = result {
            if output.format == OutputFormat::StreamingMp4 {
                // A distinct file name never overwrites an older operation's partial.
                let partial = temporary.0.with_extension("partial.mp4");
                let _ = tokio::fs::rename(&temporary.0, partial).await;
            }
            return Err(error);
        }
        if output.format == OutputFormat::StreamingMp4 {
            let partial = temporary.0.with_extension("partial.mp4");
            tokio::fs::rename(&temporary.0, &partial)
                .await
                .map_err(|e| SessionError::new(e.into(), SessionPhase::Finalizing))?;
            self.emit(SessionPhase::Finalizing, None, None)?;
            let cancel = self.options.cancel.clone();
            let tracks = self.tracks.clone();
            let target = path.to_path_buf();
            let source = partial.clone();
            let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let _guard = BlockingStopGuard(stopped.clone());
            let (next, bytes, reports) = tokio::task::spawn_blocking(move || -> Result<_> {
                use std::io::Write;
                let check = || {
                    if stopped.load(std::sync::atomic::Ordering::Acquire) {
                        return Err(Error::Cancelled);
                    }
                    check_cancel(cancel.as_ref())
                };
                check()?;
                let mut input = std::fs::File::open(&source)?;
                let size = input.metadata()?.len();
                let index = crate::isobmff::scan_file(
                    &mut input,
                    size,
                    matches!(output.backend, FinalizeBackend::Native),
                    false,
                    &check,
                )?;
                let (next, mut file) = temporary_file(&target)?;
                let (bytes, reports) = match output.backend {
                    FinalizeBackend::Native => {
                        let classic = tracks
                            .into_iter()
                            .zip(index.samples)
                            .map(|(t, samples)| t.into_classic(samples))
                            .collect();
                        Mp4Muxer::new(classic).write_file(&mut input, &mut file, &check)?
                    }
                    #[cfg(feature = "ffmpeg-finalize")]
                    FinalizeBackend::Ffmpeg => {
                        crate::ffmpeg_finalize::remux_blocking(&source, &next.0, &check)?;
                        let reports = tracks
                            .iter()
                            .enumerate()
                            .map(|(i, t)| {
                                track_report(
                                    t,
                                    index.sample_counts[i],
                                    index.decode_ends[i].max(
                                        u64::try_from(index.presentation_ends[i]).unwrap_or(0),
                                    ),
                                )
                            })
                            .collect();
                        (file.metadata()?.len(), reports)
                    }
                };
                file.flush()?;
                drop(file);
                check()?;
                Ok((next, bytes, reports))
            })
            .await
            .map_err(|e| {
                SessionError::new(
                    Error::muxing(format!("finalize worker: {e}")),
                    SessionPhase::Finalizing,
                )
            })?
            .map_err(|e| SessionError::new(e, SessionPhase::Finalizing))?;
            temporary = next;
            self.bytes = bytes;
            self.reports = reports;
            self.check()
                .map_err(|e| SessionError::new(e, SessionPhase::Finalizing))?;
            tokio::fs::rename(&temporary.0, path)
                .await
                .map_err(|e| SessionError::new(e.into(), SessionPhase::Finalizing))?;
            let _ = tokio::fs::remove_file(partial).await;
        } else {
            self.check()
                .map_err(|e| SessionError::new(e, SessionPhase::Writing))?;
            tokio::fs::rename(&temporary.0, path)
                .await
                .map_err(|e| SessionError::new(e.into(), SessionPhase::Writing))?;
        }
        self.emit(SessionPhase::Completed, None, None)?;
        Ok(self.report())
    }
}
