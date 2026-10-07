//! Fixed multi-track sessions. Input and track configuration is immutable once created.
use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackMetadata {
    pub(crate) language: String,
    pub(crate) name: String,
    pub(crate) default: bool,
    pub(crate) group: u16,
}
impl Default for TrackMetadata {
    fn default() -> Self {
        Self {
            language: "und".into(),
            name: String::new(),
            default: false,
            group: 0,
        }
    }
}
impl TrackMetadata {
    pub fn new(language: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            language: language.into(),
            name: name.into(),
            ..Self::default()
        }
    }
    pub fn with_default(mut self, value: bool) -> Self {
        self.default = value;
        self
    }
    pub fn language(&self) -> &str {
        &self.language
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn is_default(&self) -> bool {
        self.default
    }
    pub(super) fn validate(&self) -> ContinuousResult<()> {
        if self.language.is_empty()
            || self.language.len() > 63
            || self.name.len() > 1024
            || !self
                .language
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || self.name.contains('\0')
        {
            return Err(fail(ContinuousErrorKind::InvalidOptions));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OutputTrackId(pub(super) u32);
impl OutputTrackId {
    pub fn get(self) -> u32 {
        self.0
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EmbeddedAudio {
    Keep,
    Exclude,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OutputTrackKind {
    Video,
    Audio,
    Subtitle,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OutputTrackCodec {
    Avc,
    Hevc,
    AacLc,
    Wvtt,
}
impl OutputTrackCodec {
    pub(super) fn of(track: &FragmentedTrack) -> Self {
        match &track.kind {
            crate::mp4::FragmentedTrackKind::Video { codec, .. } => match codec {
                crate::mp4::VideoCodec::Avc { .. } => Self::Avc,
                crate::mp4::VideoCodec::Hevc { .. } => Self::Hevc,
            },
            crate::mp4::FragmentedTrackKind::Audio { .. } => Self::AacLc,
            crate::mp4::FragmentedTrackKind::Wvtt => Self::Wvtt,
        }
    }
}
#[derive(Debug, Clone)]
pub struct OutputTrackInfo {
    pub(super) id: OutputTrackId,
    pub(super) output: u64,
    pub(super) input: InputId,
    pub(super) kind: OutputTrackKind,
    pub(super) codec: OutputTrackCodec,
    pub(super) metadata: TrackMetadata,
    pub(super) timescale: u32,
    pub(super) duration: u64,
    pub(super) samples: u64,
}
impl OutputTrackInfo {
    pub fn output_index(&self) -> u64 {
        self.output
    }
    pub fn id(&self) -> OutputTrackId {
        self.id
    }
    pub fn input_id(&self) -> &InputId {
        &self.input
    }
    pub fn codec(&self) -> OutputTrackCodec {
        self.codec
    }
    pub fn kind(&self) -> OutputTrackKind {
        self.kind
    }
    pub fn metadata(&self) -> &TrackMetadata {
        &self.metadata
    }
    pub fn timescale(&self) -> u32 {
        self.timescale
    }
    pub fn duration(&self) -> u64 {
        self.duration
    }
    pub fn sample_count(&self) -> u64 {
        self.samples
    }
}

#[derive(Clone)]
pub struct MultiTrackInputs {
    inputs: Vec<ContinuousInput>,
    embedded: EmbeddedAudio,
    metadata: Vec<TrackMetadata>,
    subtitles: Vec<SubtitleTrack>,
}
impl MultiTrackInputs {
    pub fn new(primary: ContinuousInput, embedded: EmbeddedAudio) -> Self {
        Self {
            inputs: vec![primary],
            embedded,
            metadata: vec![TrackMetadata::default()],
            subtitles: vec![],
        }
    }
    pub fn with_subtitle(mut self, track: SubtitleTrack) -> Self {
        self.subtitles.push(track);
        self
    }
    pub fn with_primary_audio(mut self, metadata: TrackMetadata) -> Self {
        self.metadata[0] = metadata;
        self
    }
    pub fn with_audio(mut self, audio: ContinuousInput, metadata: TrackMetadata) -> Self {
        self.inputs.push(audio);
        self.metadata.push(metadata);
        self
    }
}

pub(super) struct MultiState {
    pub configuration: [u8; 32],
    pub metadata: Vec<TrackMetadata>,
    pub input_ids: Vec<InputId>,
    pub tracks: Vec<OutputTrackInfo>,
    pub track_history_truncated: bool,
    pub subtitles: Vec<super::subtitles::SubtitleLane>,
    pub subtitle_reports: VecDeque<SubtitleCueReport>,
    pub subtitle_history_truncated: bool,
    pub media_samples: usize,
    pub media_bytes: usize,
}
pub struct MultiTrackSession {
    core: ContinuousSession,
    state: Arc<std::sync::Mutex<MultiState>>,
}
#[derive(Clone)]
pub struct MultiTrackHandle {
    core: ContinuousHandle,
    shared: Arc<std::sync::Mutex<MultiState>>,
}
impl MultiTrackHandle {
    pub fn state(&self) -> ContinuousState {
        self.core.state()
    }
    pub fn stop(&self) {
        self.core.stop();
    }
    pub fn cancel(&self) {
        self.core.cancel();
    }
    pub fn end_input(&self, input: &InputId) -> ContinuousResult<()> {
        self.core.end_input(input)
    }
    pub fn restart(&self, input: &InputId, generation: u64) -> ContinuousResult<()> {
        self.core.restart(input, generation)
    }
    pub fn pause(&self) -> ContinuousResult<()> {
        self.core.pause()
    }
    pub fn resume(&self) -> ContinuousResult<()> {
        self.core.resume()
    }
    pub async fn wait_paused(&self) -> ContinuousResult<()> {
        self.core.wait_paused().await
    }
    pub async fn wait_capacity(&self) -> ContinuousResult<()> {
        self.core.wait_capacity().await
    }
    pub fn accept_snapshot(
        &self,
        input: &InputId,
        snapshot: &PlaylistSnapshot,
    ) -> ContinuousResult<SnapshotAcceptance> {
        self.core.accept_snapshot(input, snapshot)
    }
    pub fn subtitle_track_id(&self, id: &InputId) -> Option<OutputTrackId> {
        self.shared
            .lock()
            .unwrap()
            .subtitles
            .iter()
            .find(|s| &s.config.id == id)
            .map(|s| s.track)
    }
    pub fn accept_cues(
        &self,
        track: OutputTrackId,
        cues: &[SubtitleCue],
    ) -> ContinuousResult<SubtitleAcceptance> {
        let state = self.core.shared.inner.lock().unwrap();
        if self.core.shared.signal.is_cancelled() {
            return Err(fail(ContinuousErrorKind::Cancelled));
        }
        if state.state.terminal() || state.reason.is_some() {
            return Err(fail(ContinuousErrorKind::Closed));
        }
        if state.blocked {
            return Err(fail(ContinuousErrorKind::WouldBlock));
        }
        let result =
            self.shared
                .lock()
                .unwrap()
                .accept_cues(track, cues, &self.core.shared.options.limits);
        drop(state);
        if result.is_ok() {
            self.core.shared.signal.wake();
        }
        result
    }
    /// Retry the same atomic batch after producer/consumer progress, without polling.
    pub async fn accept_cues_when_ready(
        &self,
        track: OutputTrackId,
        cues: &[SubtitleCue],
    ) -> ContinuousResult<SubtitleAcceptance> {
        let mut wake = self.core.shared.signal.changed.subscribe();
        loop {
            match self.accept_cues(track, cues) {
                Err(error) if error.kind() == ContinuousErrorKind::WouldBlock => {
                    wake.changed()
                        .await
                        .map_err(|_| fail(ContinuousErrorKind::Closed))?;
                }
                result => return result,
            }
        }
    }
    pub fn end_subtitles(&self, track: OutputTrackId) -> ContinuousResult<()> {
        let mut state = self.shared.lock().unwrap();
        state
            .subtitles
            .iter_mut()
            .find(|s| s.track == track)
            .ok_or_else(|| fail(ContinuousErrorKind::UnknownInput))?
            .end();
        drop(state);
        self.core.shared.signal.wake();
        Ok(())
    }
    pub fn control(&self) -> &ContinuousHandle {
        &self.core
    }
}
#[derive(Debug, Clone)]
pub struct MultiTrackReport {
    configuration: [u8; 32],
    media: ContinuousReport,
    tracks: Vec<OutputTrackInfo>,
    track_history_truncated: bool,
    subtitle_reports: Vec<SubtitleCueReport>,
    subtitle_history_truncated: bool,
}
impl MultiTrackReport {
    pub fn configuration_id(&self) -> &[u8; 32] {
        &self.configuration
    }
    pub fn media(&self) -> &ContinuousReport {
        &self.media
    }
    pub fn tracks(&self) -> &[OutputTrackInfo] {
        &self.tracks
    }
    pub fn track_history_truncated(&self) -> bool {
        self.track_history_truncated
    }
    pub fn subtitle_reports(&self) -> &[SubtitleCueReport] {
        &self.subtitle_reports
    }
    pub fn subtitle_history_truncated(&self) -> bool {
        self.subtitle_history_truncated
    }
}
impl MultiState {
    pub fn trim_tracks(&mut self, output: u64, limits: &ContinuousLimits) {
        while self.tracks.len() > limits.history
            || self
                .tracks
                .iter()
                .map(|t| {
                    std::mem::size_of::<OutputTrackInfo>()
                        + t.input.as_str().len()
                        + t.metadata.language.len()
                        + t.metadata.name.len()
                })
                .sum::<usize>()
                > limits.metadata
        {
            let Some(oldest) = self
                .tracks
                .iter()
                .map(|t| t.output)
                .min()
                .filter(|old| *old < output)
            else {
                break;
            };
            self.tracks.retain(|t| t.output != oldest);
            self.track_history_truncated = true;
        }
    }
    fn report(&self, media: ContinuousReport) -> MultiTrackReport {
        MultiTrackReport {
            configuration: self.configuration,
            media,
            tracks: self.tracks.clone(),
            track_history_truncated: self.track_history_truncated,
            subtitle_reports: self.subtitle_reports.iter().cloned().collect(),
            subtitle_history_truncated: self.subtitle_history_truncated,
        }
    }
}
impl MultiTrackSession {
    /// Restore a native file session. Re-submit required snapshots and subtitle cues
    /// before running the file writer; validation precedes any output modification.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn restore(
        inputs: MultiTrackInputs,
        keys: KeySession,
        options: ContinuousOptions,
        checkpoint: EngineCheckpoint,
    ) -> ContinuousResult<Self> {
        let mut session = Self::new(inputs, keys, options)?;
        if session.state.lock().unwrap().configuration != checkpoint.configuration {
            return Err(fail(ContinuousErrorKind::ResumeConflict));
        }
        session.core.recovery = Some(checkpoint);
        Ok(session)
    }

    pub fn new(
        inputs: MultiTrackInputs,
        keys: KeySession,
        options: ContinuousOptions,
    ) -> ContinuousResult<Self> {
        if inputs.inputs.len() > 32 {
            return Err(fail(ContinuousErrorKind::InvalidOptions));
        }
        for metadata in &inputs.metadata {
            metadata.validate()?;
        }
        if inputs
            .metadata
            .iter()
            .enumerate()
            .filter(|(i, m)| m.default && (*i > 0 || inputs.embedded == EmbeddedAudio::Keep))
            .count()
            > 1
        {
            return Err(fail(ContinuousErrorKind::InvalidOptions));
        }
        if inputs.subtitles.len() > 32 {
            return Err(fail(ContinuousErrorKind::InvalidOptions));
        }
        for (index, sub) in inputs.subtitles.iter().enumerate() {
            sub.metadata.validate()?;
            if !inputs.inputs.iter().any(|i| i.id == sub.timeline_input)
                || inputs.inputs.iter().any(|i| i.id == sub.id)
                || inputs.subtitles[..index].iter().any(|s| s.id == sub.id)
            {
                return Err(fail(ContinuousErrorKind::InvalidOptions));
            }
        }
        if inputs
            .subtitles
            .iter()
            .filter(|s| s.metadata.default)
            .count()
            > 1
        {
            return Err(fail(ContinuousErrorKind::InvalidOptions));
        }
        let mut identity = b"hls-engine-configuration-v2".to_vec();
        identity.push(u8::from(inputs.embedded == EmbeddedAudio::Keep));
        identity.extend_from_slice(&(inputs.inputs.len() as u32).to_be_bytes());
        identity.extend_from_slice(&(inputs.subtitles.len() as u32).to_be_bytes());
        let mut field = |value: &str| {
            identity.extend_from_slice(&(value.len() as u64).to_be_bytes());
            identity.extend_from_slice(value.as_bytes());
        };
        for (input, metadata) in inputs.inputs.iter().zip(&inputs.metadata) {
            field(input.id.as_str());
            field(&metadata.language);
            field(&metadata.name);
            field(if metadata.default {
                "default"
            } else {
                "alternate"
            });
        }
        for subtitle in &inputs.subtitles {
            field(subtitle.id.as_str());
            field(subtitle.timeline_input.as_str());
            field(&subtitle.metadata.language);
            field(&subtitle.metadata.name);
            field(if subtitle.metadata.default {
                "default"
            } else {
                "alternate"
            });
        }
        // Commit the mapping policy as well as the immutable selection. This is
        // configuration identity only; it does not create a resumable checkpoint.
        field(match options.mode {
            ContinuousMode::Vod => "vod",
            ContinuousMode::Open => "open",
        });
        field(match options.gaps {
            GapPolicy::Preserve => "preserve",
            GapPolicy::Collapse => "collapse",
        });
        field(match options.changes {
            TimelineChangePolicy::Fail => "fail",
            TimelineChangePolicy::Split => "split",
        });
        field(match options.missing {
            MissingSegmentPolicy::Fail => "fail",
            MissingSegmentPolicy::Skip => "skip",
            MissingSegmentPolicy::Split => "split",
        });
        field(if options.resources.experimental_gcm() {
            "gcm-draft22"
        } else {
            "stable"
        });
        let mut time = |v: Option<MediaTime>| {
            field(if v.is_some() { "some" } else { "none" });
            if let Some(v) = v {
                field(&v.ticks.to_string());
                field(&v.timescale.to_string());
            }
        };
        time(options.range.map(|r| r.start()));
        time(options.range.map(|r| r.end()));
        time(options.duration);
        time(match options.tail {
            TailDurationPolicy::RequireEvidence => None,
            TailDurationPolicy::Explicit(t) => Some(t),
        });
        field(&options.anchors.len().to_string());
        for anchor in &options.anchors {
            field(anchor.input.as_str());
            field(&anchor.generation.to_string());
            field(&anchor.epoch.to_string());
            field(&anchor.source.ticks.to_string());
            field(&anchor.source.timescale.to_string());
            field(&anchor.presentation.ticks.to_string());
            field(&anchor.presentation.timescale.to_string());
        }
        let configuration = crate::resume::digest(&identity);
        let state = Arc::new(std::sync::Mutex::new(MultiState {
            configuration,
            input_ids: inputs.inputs.iter().map(|i| i.id.clone()).collect(),
            metadata: inputs.metadata,
            tracks: vec![],
            track_history_truncated: false,
            subtitles: inputs
                .subtitles
                .into_iter()
                .enumerate()
                .map(|(i, c)| super::subtitles::SubtitleLane::new(c, i))
                .collect(),
            subtitle_reports: VecDeque::new(),
            subtitle_history_truncated: false,
            media_samples: 0,
            media_bytes: 0,
        }));
        let mut core = ContinuousSession::new(
            ContinuousInputs {
                inputs: inputs.inputs,
            },
            keys,
            options,
        )?;
        core.shared.inner.lock().unwrap().global_history = true;
        core.keep_embedded = inputs.embedded == EmbeddedAudio::Keep;
        core.multi = Some(state.clone());
        Ok(Self { core, state })
    }
    /// Recoverable native fMP4/classic output. Split children append
    /// `.part-000001.mp4`, etc. Each `.hls-partial` sibling is retained for recovery;
    /// the caller persists every checkpoint atomically, including acquisition intent.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn write_recoverable_to_file(
        self,
        path: impl AsRef<Path>,
        recovery: RecoveryOptions,
    ) -> ContinuousResult<MultiTrackReport> {
        let media = self.core.recoverable_file(path.as_ref(), recovery).await?;
        Ok(self.state.lock().unwrap().report(media))
    }
    pub fn handle(&self) -> MultiTrackHandle {
        MultiTrackHandle {
            core: self.core.handle(),
            shared: self.state.clone(),
        }
    }
    pub async fn write_to<W: AsyncWrite + Unpin>(
        self,
        writer: &mut W,
    ) -> ContinuousResult<MultiTrackReport> {
        let media = self.core.write_to(writer).await?;
        Ok(self.state.lock().unwrap().report(media))
    }
    pub async fn into_bytes(
        self,
        capacity: usize,
        format: OutputFormat,
    ) -> ContinuousResult<(Vec<u8>, MultiTrackReport)> {
        let (bytes, media) = self.core.into_bytes(capacity, format).await?;
        Ok((bytes, self.state.lock().unwrap().report(media)))
    }
    pub async fn write_to_outputs<P: ContinuousWriterProvider>(
        self,
        provider: &mut P,
    ) -> ContinuousResult<MultiTrackReport> {
        let media = self.core.write_to_outputs(provider).await?;
        Ok(self.state.lock().unwrap().report(media))
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn write_to_files<P: ContinuousFileProvider>(
        self,
        provider: &mut P,
        options: FileOutputOptions,
    ) -> ContinuousResult<MultiTrackReport> {
        let media = self.core.write_to_files(provider, options).await?;
        Ok(self.state.lock().unwrap().report(media))
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn write_to_file(
        self,
        path: impl AsRef<Path>,
        options: FileOutputOptions,
    ) -> ContinuousResult<MultiTrackReport> {
        let media = self.core.write_to_file(path, options).await?;
        Ok(self.state.lock().unwrap().report(media))
    }
}

use crate::state_codec::{StateCodec, state_enum, state_struct};
state_struct!(TrackMetadata {
    language,
    name,
    default,
    group
});
state_enum!(OutputTrackKind {0 => Video, 1 => Audio, 2 => Subtitle});
state_enum!(OutputTrackCodec {0 => Avc, 1 => Hevc, 2 => AacLc, 3 => Wvtt});
state_struct!(OutputTrackInfo {
    id,
    output,
    input,
    kind,
    codec,
    metadata,
    timescale,
    duration,
    samples
});
impl StateCodec for OutputTrackId {
    fn put(&self, out: &mut Vec<u8>) {
        self.0.put(out);
    }
    fn get(r: &mut crate::state_codec::Reader<'_>) -> crate::state_codec::DecodeResult<Self> {
        Ok(Self(u32::get(r)?))
    }
}
