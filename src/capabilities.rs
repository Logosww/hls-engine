//! Declarative support queries for the finite keyed prepared entry, not a media validator.
#![doc = include_str!("../docs/keyed-contracts.md")]
use crate::InputRole;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeyedContainer {
    TransportStream,
    FragmentedMp4,
    PackedAac,
    Other,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeyedEncryption {
    Clear,
    Aes128,
    ClearAndAes128,
    SampleAes,
    SampleAesCtr,
    Aes256Gcm,
    Other,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeyedCodec {
    Avc,
    Hevc,
    AacLc,
    Other,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeyedProtectionScheme {
    None,
    Cenc,
    Cbcs,
    Other,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeyedKeySource {
    None,
    ExternalProvider,
    BuiltInHttp,
    Cdm,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeyedSourceMode {
    FiniteVod,
    Event,
    Live,
    RewrittenSnapshot,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeyedOutput {
    Mp4Bytes,
    FragmentedWriter,
    Mp4File,
    FragmentedFile,
    NativeStreamingFile,
    FfmpegStreamingFile,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeyedRange {
    WholeResources,
    ClearByteRanges,
    CompleteEncryptedResources,
    ArbitraryEncryptedRanges,
    PresentationRange,
    IFrameRanges,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CapabilityDimension {
    Container,
    Encryption,
    Codec,
    ProtectionScheme,
    KeySource,
    SourceMode,
    Output,
    Range,
    Resume,
    TrackSelection,
    Subtitles,
    Experimental,
    Timeline,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CapabilityRequirement {
    FiniteSnapshotValidation,
    IncrementalSnapshotValidation,
    ContainerAndCodecValidation,
    CompatibleTimelineAndConfiguration,
    ProviderResolution,
    CompleteEncryptionBoundaries,
    CallerOwnedWriterCompletion,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyedInputCapability {
    container: KeyedContainer,
    encryption: KeyedEncryption,
    codecs: Vec<KeyedCodec>,
    scheme: KeyedProtectionScheme,
    key_source: KeyedKeySource,
}
impl KeyedInputCapability {
    pub fn new(
        container: KeyedContainer,
        encryption: KeyedEncryption,
        codecs: Vec<KeyedCodec>,
    ) -> Self {
        Self {
            container,
            encryption,
            codecs,
            scheme: KeyedProtectionScheme::None,
            key_source: if encryption == KeyedEncryption::Clear {
                KeyedKeySource::None
            } else {
                KeyedKeySource::ExternalProvider
            },
        }
    }
    pub fn with_scheme(mut self, value: KeyedProtectionScheme) -> Self {
        self.scheme = value;
        self
    }
    pub fn with_key_source(mut self, value: KeyedKeySource) -> Self {
        self.key_source = value;
        self
    }
    pub fn container(&self) -> KeyedContainer {
        self.container
    }
    pub fn encryption(&self) -> KeyedEncryption {
        self.encryption
    }
    pub fn codecs(&self) -> &[KeyedCodec] {
        &self.codecs
    }
    pub fn scheme(&self) -> KeyedProtectionScheme {
        self.scheme
    }
    pub fn key_source(&self) -> KeyedKeySource {
        self.key_source
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyedCapabilityQuery {
    primary: KeyedInputCapability,
    audio: Option<KeyedInputCapability>,
    source_mode: KeyedSourceMode,
    output: KeyedOutput,
    range: KeyedRange,
    resume: bool,
    multitrack: bool,
    subtitles: bool,
    experimental: bool,
    timeline_changes: bool,
}
impl KeyedCapabilityQuery {
    pub fn new(primary: KeyedInputCapability, output: KeyedOutput) -> Self {
        Self {
            primary,
            audio: None,
            source_mode: KeyedSourceMode::FiniteVod,
            output,
            range: KeyedRange::WholeResources,
            resume: false,
            multitrack: false,
            subtitles: false,
            experimental: false,
            timeline_changes: false,
        }
    }
    pub fn with_audio(mut self, value: KeyedInputCapability) -> Self {
        self.audio = Some(value);
        self
    }
    pub fn with_source_mode(mut self, value: KeyedSourceMode) -> Self {
        self.source_mode = value;
        self
    }
    pub fn with_range(mut self, value: KeyedRange) -> Self {
        self.range = value;
        self
    }
    pub fn with_resume(mut self, value: bool) -> Self {
        self.resume = value;
        self
    }
    pub fn with_multitrack(mut self, value: bool) -> Self {
        self.multitrack = value;
        self
    }
    pub fn with_subtitles(mut self, value: bool) -> Self {
        self.subtitles = value;
        self
    }
    pub fn with_experimental(mut self, value: bool) -> Self {
        self.experimental = value;
        self
    }
    pub fn with_timeline_changes(mut self, value: bool) -> Self {
        self.timeline_changes = value;
        self
    }
    pub fn primary(&self) -> &KeyedInputCapability {
        &self.primary
    }
    pub fn audio(&self) -> Option<&KeyedInputCapability> {
        self.audio.as_ref()
    }
    pub fn source_mode(&self) -> KeyedSourceMode {
        self.source_mode
    }
    pub fn output(&self) -> KeyedOutput {
        self.output
    }
    pub fn range(&self) -> KeyedRange {
        self.range
    }
    pub fn resume(&self) -> bool {
        self.resume
    }
    pub fn multitrack(&self) -> bool {
        self.multitrack
    }
    pub fn subtitles(&self) -> bool {
        self.subtitles
    }
    pub fn experimental(&self) -> bool {
        self.experimental
    }
    pub fn timeline_changes(&self) -> bool {
        self.timeline_changes
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityRejection {
    dimension: CapabilityDimension,
    role: Option<InputRole>,
}
impl CapabilityRejection {
    pub fn dimension(&self) -> CapabilityDimension {
        self.dimension
    }
    pub fn role(&self) -> Option<InputRole> {
        self.role
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyedCapabilityDecision {
    rejections: Vec<CapabilityRejection>,
    requirements: Vec<CapabilityRequirement>,
}
impl KeyedCapabilityDecision {
    /// Means this declared combination has an implementation in this build. Does not certify input bytes or a provider.
    pub fn supported(&self) -> bool {
        self.rejections.is_empty()
    }
    pub fn rejections(&self) -> &[CapabilityRejection] {
        &self.rejections
    }
    pub fn requirements(&self) -> &[CapabilityRequirement] {
        &self.requirements
    }
}
/// No I/O. Every unsupported dimension is returned, in stable dimension/input order.
/// Native files and FFmpeg depend on this build's target/features; no experimental bypass exists.
pub fn query_keyed_capability(query: &KeyedCapabilityQuery) -> KeyedCapabilityDecision {
    use CapabilityDimension as D;
    use CapabilityRequirement as R;
    let mut rejections = Vec::new();
    let mut requirements = vec![
        R::FiniteSnapshotValidation,
        R::ContainerAndCodecValidation,
        R::CompatibleTimelineAndConfiguration,
    ];
    for (role, input) in std::iter::once((InputRole::Primary, &query.primary))
        .chain(query.audio.as_ref().map(|a| (InputRole::Audio, a)))
    {
        let mut reject = |dimension| {
            rejections.push(CapabilityRejection {
                dimension,
                role: Some(role),
            })
        };
        if !matches!(
            input.container,
            KeyedContainer::TransportStream | KeyedContainer::FragmentedMp4
        ) {
            reject(D::Container);
        }
        if !matches!(
            input.encryption,
            KeyedEncryption::Clear
                | KeyedEncryption::Aes128
                | KeyedEncryption::ClearAndAes128
                | KeyedEncryption::SampleAes
                | KeyedEncryption::SampleAesCtr
        ) {
            reject(D::Encryption);
        }
        if input.codecs.is_empty() || input.codecs.contains(&KeyedCodec::Other) {
            reject(D::Codec);
        }
        let video = input
            .codecs
            .iter()
            .filter(|c| matches!(c, KeyedCodec::Avc | KeyedCodec::Hevc))
            .count();
        let audio = input
            .codecs
            .iter()
            .filter(|c| **c == KeyedCodec::AacLc)
            .count();
        if video > 1
            || audio > 1
            || (role == InputRole::Audio && audio != 1)
            || (role == InputRole::Primary && query.audio.is_some() && video != 1)
        {
            reject(D::TrackSelection);
        }
        let expected_scheme = match (input.container, input.encryption) {
            (KeyedContainer::FragmentedMp4, KeyedEncryption::SampleAes) => {
                KeyedProtectionScheme::Cbcs
            }
            (KeyedContainer::FragmentedMp4, KeyedEncryption::SampleAesCtr) => {
                KeyedProtectionScheme::Cenc
            }
            _ => KeyedProtectionScheme::None,
        };
        if input.scheme != expected_scheme {
            reject(D::ProtectionScheme);
        }
        if input.container == KeyedContainer::TransportStream {
            if input.encryption == KeyedEncryption::SampleAesCtr {
                reject(D::Encryption);
            }
            if input.encryption == KeyedEncryption::SampleAes
                && input.codecs.contains(&KeyedCodec::Hevc)
            {
                reject(D::Codec);
            }
        }
        if !matches!(
            input.key_source,
            KeyedKeySource::None | KeyedKeySource::ExternalProvider
        ) || (input.encryption != KeyedEncryption::Clear
            && input.key_source != KeyedKeySource::ExternalProvider)
        {
            reject(D::KeySource);
        }
        if input.encryption != KeyedEncryption::Clear
            && !requirements.contains(&R::ProviderResolution)
        {
            requirements.push(R::ProviderResolution);
        }
    }
    let mut reject = |dimension| {
        rejections.push(CapabilityRejection {
            dimension,
            role: None,
        })
    };
    if query.source_mode != KeyedSourceMode::FiniteVod {
        reject(D::SourceMode);
    }
    if matches!(
        query.output,
        KeyedOutput::Mp4File
            | KeyedOutput::FragmentedFile
            | KeyedOutput::NativeStreamingFile
            | KeyedOutput::FfmpegStreamingFile
    ) && cfg!(target_arch = "wasm32")
        || query.output == KeyedOutput::FfmpegStreamingFile && !cfg!(feature = "ffmpeg-finalize")
    {
        reject(D::Output);
    }
    if matches!(
        query.range,
        KeyedRange::ArbitraryEncryptedRanges
            | KeyedRange::PresentationRange
            | KeyedRange::IFrameRanges
    ) {
        reject(D::Range);
    }
    if query.range == KeyedRange::CompleteEncryptedResources {
        requirements.push(R::CompleteEncryptionBoundaries);
    }
    if query.output == KeyedOutput::FragmentedWriter {
        requirements.push(R::CallerOwnedWriterCompletion);
    }
    if query.resume {
        reject(D::Resume);
    }
    if query.multitrack {
        reject(D::TrackSelection);
    }
    if query.subtitles {
        reject(D::Subtitles);
    }
    if query.experimental {
        reject(D::Experimental);
    }
    if query.timeline_changes {
        reject(D::Timeline);
    }
    KeyedCapabilityDecision {
        rejections,
        requirements,
    }
}

/// Timeline execution has its own capability profile; old keyed queries remain unchanged.
#[derive(Debug, Clone)]
pub struct TimelineCapabilityQuery {
    media: KeyedCapabilityQuery,
    presentation_range: bool,
    gaps: crate::GapPolicy,
    changes: crate::TimelineChangePolicy,
    split_outputs: bool,
    known_internal_gaps: bool,
}
impl TimelineCapabilityQuery {
    pub fn new(media: KeyedCapabilityQuery) -> Self {
        Self {
            media,
            presentation_range: false,
            gaps: crate::GapPolicy::Preserve,
            changes: crate::TimelineChangePolicy::Fail,
            split_outputs: false,
            known_internal_gaps: false,
        }
    }
    pub fn with_presentation_range(mut self, value: bool) -> Self {
        self.presentation_range = value;
        self
    }
    pub fn with_gap_policy(mut self, value: crate::GapPolicy) -> Self {
        self.gaps = value;
        self
    }
    pub fn with_change_policy(mut self, value: crate::TimelineChangePolicy) -> Self {
        self.changes = value;
        self
    }
    pub fn with_split_outputs(mut self, value: bool) -> Self {
        self.split_outputs = value;
        self
    }
    pub fn with_known_internal_gaps(mut self, value: bool) -> Self {
        self.known_internal_gaps = value;
        self
    }
    pub fn media(&self) -> &KeyedCapabilityQuery {
        &self.media
    }
    pub fn presentation_range(&self) -> bool {
        self.presentation_range
    }
    pub fn gap_policy(&self) -> crate::GapPolicy {
        self.gaps
    }
    pub fn change_policy(&self) -> crate::TimelineChangePolicy {
        self.changes
    }
    pub fn split_outputs(&self) -> bool {
        self.split_outputs
    }
    pub fn known_internal_gaps(&self) -> bool {
        self.known_internal_gaps
    }
}
/// A declaration, not proof of RAP, clock alignment or common-gap coverage.
pub fn query_timeline_capability(query: &TimelineCapabilityQuery) -> KeyedCapabilityDecision {
    let mut base = query.media.clone().with_timeline_changes(false);
    if base.range == KeyedRange::PresentationRange {
        base.range = KeyedRange::WholeResources;
    }
    let mut decision = query_keyed_capability(&base);
    if query.changes == crate::TimelineChangePolicy::Split && !query.split_outputs {
        decision.rejections.push(CapabilityRejection {
            dimension: CapabilityDimension::Output,
            role: None,
        });
    }
    let classic = !matches!(
        query.media.output,
        KeyedOutput::FragmentedWriter | KeyedOutput::FragmentedFile
    );
    if classic
        && query.known_internal_gaps
        && query.gaps == crate::GapPolicy::Preserve
        && query.changes != crate::TimelineChangePolicy::Split
    {
        decision.rejections.push(CapabilityRejection {
            dimension: CapabilityDimension::Timeline,
            role: None,
        });
    }
    decision
}

/// Open-session declaration. Memory output requires an explicit capacity;
/// dual-input execution also requires a host-provided timeout implementation.
#[derive(Debug, Clone)]
pub struct ContinuousCapabilityQuery {
    timeline: TimelineCapabilityQuery,
    memory_capacity: Option<usize>,
    host_waiter: bool,
}
impl ContinuousCapabilityQuery {
    pub fn new(timeline: TimelineCapabilityQuery) -> Self {
        Self {
            timeline,
            memory_capacity: None,
            host_waiter: false,
        }
    }
    pub fn with_memory_capacity(mut self, capacity: usize) -> Self {
        self.memory_capacity = Some(capacity);
        self
    }
    pub fn with_host_waiter(mut self, available: bool) -> Self {
        self.host_waiter = available;
        self
    }
}
/// Incremental admission, key resolution and media validation remain runtime requirements.
pub fn query_continuous_capability(query: &ContinuousCapabilityQuery) -> KeyedCapabilityDecision {
    let mut timeline = query.timeline.clone();
    let rewritten = timeline.media.source_mode == KeyedSourceMode::RewrittenSnapshot;
    timeline.media.source_mode = KeyedSourceMode::FiniteVod;
    let mut result = query_timeline_capability(&timeline);
    result
        .requirements
        .retain(|r| *r != CapabilityRequirement::FiniteSnapshotValidation);
    result
        .requirements
        .push(CapabilityRequirement::IncrementalSnapshotValidation);
    if rewritten {
        result.rejections.push(CapabilityRejection {
            dimension: CapabilityDimension::SourceMode,
            role: None,
        });
    }
    if (timeline.media.output == KeyedOutput::Mp4Bytes
        && query.memory_capacity.is_none_or(|n| n == 0))
        || (timeline.media.audio.is_some() && !query.host_waiter)
    {
        result.rejections.push(CapabilityRejection {
            dimension: CapabilityDimension::Output,
            role: None,
        });
    }
    result
}

/// Container support is separate from a target player's rendering support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MultiTrackPlayback {
    Container,
    /// Extract a single wvtt track before the pinned Shaka parser/display adapter.
    /// Does not certify audio switching or direct mixed-mdat playback.
    ShakaAdapter,
    DirectBrowser,
    AvFoundation,
    Vlc,
    Iina,
    /// FFmpeg's default demux/decoding path, separate from the finalize backend.
    Ffmpeg,
}
/// Tested playback incompatibilities, distinct from invalid media or mux support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MultiTrackPlaybackRejection {
    /// The generic browser path cannot expose/select all embedded tracks.
    BrowserTrackSelection,
    /// The renderer cannot preserve the complete accepted WebVTT settings profile.
    SubtitleSettings,
    /// The player does not recognize MP4 wvtt subtitle tracks.
    WvttDecoder,
    /// Default classic edit-list playback can trim AAC or mis-handle interior gaps.
    ClassicEditTimeline,
    /// Decode-gap playback is not certified for this player adapter.
    DecodeGaps,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SubtitleProfile {
    WvttPlainText,
    StyledWebVtt,
}
#[derive(Debug, Clone)]
pub struct MultiTrackCapabilityQuery {
    inputs: Vec<(crate::playlist::InputId, KeyedInputCapability)>,
    output: KeyedOutput,
    embedded: crate::EmbeddedAudio,
    mode: KeyedSourceMode,
    range: KeyedRange,
    subtitles: bool,
    subtitle_profile: SubtitleProfile,
    decode_gaps: bool,
    playback: MultiTrackPlayback,
    capacity: Option<usize>,
    waiter: bool,
    resume: bool,
}
impl MultiTrackCapabilityQuery {
    pub fn new(
        id: crate::playlist::InputId,
        primary: KeyedInputCapability,
        output: KeyedOutput,
        embedded: crate::EmbeddedAudio,
    ) -> Self {
        Self {
            inputs: vec![(id, primary)],
            output,
            embedded,
            mode: KeyedSourceMode::FiniteVod,
            range: KeyedRange::WholeResources,
            subtitles: false,
            subtitle_profile: SubtitleProfile::WvttPlainText,
            decode_gaps: false,
            playback: MultiTrackPlayback::Container,
            capacity: None,
            waiter: false,
            resume: false,
        }
    }
    pub fn with_audio(mut self, id: crate::playlist::InputId, input: KeyedInputCapability) -> Self {
        self.inputs.push((id, input));
        self
    }
    pub fn with_source_mode(mut self, mode: KeyedSourceMode) -> Self {
        self.mode = mode;
        self
    }
    pub fn with_range(mut self, range: KeyedRange) -> Self {
        self.range = range;
        self
    }
    pub fn with_subtitles(mut self, value: bool) -> Self {
        self.subtitles = value;
        self
    }
    /// Declare interior decode gaps. Native classic output uses edit lists;
    /// video must resume at a sync sample with non-overlapping presentation runs.
    pub fn with_decode_gaps(mut self, value: bool) -> Self {
        self.decode_gaps = value;
        self
    }
    pub fn with_subtitle_profile(mut self, profile: SubtitleProfile) -> Self {
        self.subtitles = true;
        self.subtitle_profile = profile;
        self
    }
    pub fn with_playback(mut self, target: MultiTrackPlayback) -> Self {
        self.playback = target;
        self
    }
    pub fn with_memory_capacity(mut self, capacity: usize) -> Self {
        self.capacity = Some(capacity);
        self
    }
    pub fn with_host_waiter(mut self, available: bool) -> Self {
        self.waiter = available;
        self
    }
    pub fn with_resume(mut self, value: bool) -> Self {
        self.resume = value;
        self
    }
}
#[derive(Debug, Clone)]
pub struct MultiTrackCapabilityRejection {
    input: Option<crate::playlist::InputId>,
    dimension: CapabilityDimension,
}
impl MultiTrackCapabilityRejection {
    pub fn input_id(&self) -> Option<&crate::playlist::InputId> {
        self.input.as_ref()
    }
    pub fn dimension(&self) -> CapabilityDimension {
        self.dimension
    }
}
#[derive(Debug, Clone)]
pub struct MultiTrackCapabilityDecision {
    rejections: Vec<MultiTrackCapabilityRejection>,
    requirements: Vec<CapabilityRequirement>,
    playback_rejection: Option<MultiTrackPlaybackRejection>,
}
impl MultiTrackCapabilityDecision {
    pub fn supported(&self) -> bool {
        self.rejections.is_empty()
    }
    pub fn rejections(&self) -> &[MultiTrackCapabilityRejection] {
        &self.rejections
    }
    pub fn requirements(&self) -> &[CapabilityRequirement] {
        &self.requirements
    }
    pub fn playback_rejection(&self) -> Option<MultiTrackPlaybackRejection> {
        self.playback_rejection
    }
}
/// No I/O; media bytes, keys, mappings and budgets still require runtime validation.
pub fn query_multitrack_capability(
    query: &MultiTrackCapabilityQuery,
) -> MultiTrackCapabilityDecision {
    use CapabilityDimension as D;
    let mut result = MultiTrackCapabilityDecision {
        rejections: vec![],
        requirements: vec![],
        playback_rejection: None,
    };
    for (index, (id, input)) in query.inputs.iter().enumerate() {
        let mut selected = input.clone();
        if selected.container == KeyedContainer::PackedAac {
            selected.container = KeyedContainer::TransportStream;
            if selected.codecs != [KeyedCodec::AacLc] {
                result.rejections.push(MultiTrackCapabilityRejection {
                    input: Some(id.clone()),
                    dimension: D::Codec,
                });
            }
        }
        let q = KeyedCapabilityQuery::new(selected, query.output)
            .with_source_mode(query.mode)
            .with_range(query.range)
            .with_resume(query.resume);
        let q = ContinuousCapabilityQuery::new(TimelineCapabilityQuery::new(q))
            .with_host_waiter(query.waiter);
        let q = if let Some(capacity) = query.capacity {
            q.with_memory_capacity(capacity)
        } else {
            q
        };
        let decision = query_continuous_capability(&q);
        result
            .rejections
            .extend(
                decision
                    .rejections
                    .into_iter()
                    .map(|r| MultiTrackCapabilityRejection {
                        input: Some(id.clone()),
                        dimension: r.dimension,
                    }),
            );
        for requirement in decision.requirements {
            if !result.requirements.contains(&requirement) {
                result.requirements.push(requirement);
            }
        }
        if (index > 0 && !input.codecs.contains(&KeyedCodec::AacLc))
            || (index == 0
                && query.embedded == crate::EmbeddedAudio::Exclude
                && !input
                    .codecs
                    .iter()
                    .any(|c| matches!(c, KeyedCodec::Avc | KeyedCodec::Hevc)))
            || query.inputs[..index].iter().any(|(other, _)| other == id)
        {
            result.rejections.push(MultiTrackCapabilityRejection {
                input: Some(id.clone()),
                dimension: D::TrackSelection,
            });
        }
    }
    if query.inputs.len() > 32 || (query.inputs.len() > 1 && !query.waiter) {
        result.rejections.push(MultiTrackCapabilityRejection {
            input: None,
            dimension: D::TrackSelection,
        });
    }
    if query.output == KeyedOutput::FfmpegStreamingFile {
        result.rejections.push(MultiTrackCapabilityRejection {
            input: None,
            dimension: D::Output,
        });
    }
    use MultiTrackPlayback as P;
    use MultiTrackPlaybackRejection as PR;
    let classic = matches!(
        query.output,
        KeyedOutput::Mp4Bytes | KeyedOutput::Mp4File | KeyedOutput::NativeStreamingFile
    );
    // Negative player tests are a supported preflight outcome, not permission to
    // silently drop cues, trim samples, transcode, or compress the timeline.
    result.playback_rejection = match query.playback {
        P::Container | P::ShakaAdapter => None,
        P::DirectBrowser => Some(PR::BrowserTrackSelection),
        P::Ffmpeg if classic => Some(PR::ClassicEditTimeline),
        P::Iina | P::Ffmpeg if query.subtitles => Some(PR::WvttDecoder),
        P::Vlc if query.subtitles => Some(PR::SubtitleSettings),
        P::Ffmpeg => None,
        _ if query.decode_gaps => Some(PR::DecodeGaps),
        P::Iina if classic => Some(PR::ClassicEditTimeline),
        _ => None,
    };
    if result.playback_rejection.is_some()
        || (query.subtitles && query.subtitle_profile != SubtitleProfile::WvttPlainText)
    {
        result.rejections.push(MultiTrackCapabilityRejection {
            input: None,
            dimension: if query.subtitles {
                D::Subtitles
            } else {
                D::Output
            },
        });
    }
    result
}
