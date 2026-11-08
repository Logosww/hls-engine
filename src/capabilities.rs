//! Engine capability queries. Media validation occurs during execution.
#![doc = include_str!("../docs/support.md")]

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum InputContainer {
    TransportStream,
    FragmentedMp4,
    PackedAac,
    Other,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum InputEncryption {
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
pub enum InputCodec {
    Avc,
    Hevc,
    AacLc,
    Other,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum InputProtectionScheme {
    None,
    Cenc,
    Cbcs,
    Other,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum InputKeySource {
    None,
    ExternalProvider,
    BuiltInHttp,
    Cdm,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SourceMode {
    FiniteVod,
    Event,
    Live,
    RewrittenSnapshot,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OutputTarget {
    Mp4Bytes,
    FragmentedWriter,
    Mp4File,
    FragmentedFile,
    NativeStreamingFile,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RangeCapability {
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
    GcmDraft22Authentication,
    CallerOwnedWriterCompletion,
    NativeCheckpointPersistence,
    ReplayIdentityValidation,
    StableKeyVersion,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputCapability {
    container: InputContainer,
    encryption: InputEncryption,
    codecs: Vec<InputCodec>,
    scheme: InputProtectionScheme,
    key_source: InputKeySource,
}
impl InputCapability {
    pub fn new(
        container: InputContainer,
        encryption: InputEncryption,
        codecs: Vec<InputCodec>,
    ) -> Self {
        Self {
            container,
            encryption,
            codecs,
            scheme: InputProtectionScheme::None,
            key_source: if encryption == InputEncryption::Clear {
                InputKeySource::None
            } else {
                InputKeySource::ExternalProvider
            },
        }
    }
    pub fn with_scheme(mut self, value: InputProtectionScheme) -> Self {
        self.scheme = value;
        self
    }
    pub fn with_key_source(mut self, value: InputKeySource) -> Self {
        self.key_source = value;
        self
    }
    pub fn container(&self) -> InputContainer {
        self.container
    }
    pub fn encryption(&self) -> InputEncryption {
        self.encryption
    }
    pub fn codecs(&self) -> &[InputCodec] {
        &self.codecs
    }
    pub fn scheme(&self) -> InputProtectionScheme {
        self.scheme
    }
    pub fn key_source(&self) -> InputKeySource {
        self.key_source
    }
}
fn validate_input(
    input: &InputCapability,
    query: &CapabilityQuery,
    native_recovery: bool,
) -> (Vec<CapabilityDimension>, Vec<CapabilityRequirement>) {
    use CapabilityDimension as D;
    use CapabilityRequirement as R;
    let mut rejections = Vec::new();
    let mut requirements = vec![
        R::IncrementalSnapshotValidation,
        R::ContainerAndCodecValidation,
        R::CompatibleTimelineAndConfiguration,
    ];
    {
        let mut reject = |dimension| rejections.push(dimension);
        if !matches!(
            input.container,
            InputContainer::TransportStream | InputContainer::FragmentedMp4
        ) {
            reject(D::Container);
        }
        if !matches!(
            input.encryption,
            InputEncryption::Clear
                | InputEncryption::Aes128
                | InputEncryption::ClearAndAes128
                | InputEncryption::SampleAes
                | InputEncryption::SampleAesCtr
        ) {
            reject(D::Encryption);
        }
        if input.codecs.is_empty() || input.codecs.contains(&InputCodec::Other) {
            reject(D::Codec);
        }
        let video = input
            .codecs
            .iter()
            .filter(|c| matches!(c, InputCodec::Avc | InputCodec::Hevc))
            .count();
        let audio = input
            .codecs
            .iter()
            .filter(|c| **c == InputCodec::AacLc)
            .count();
        if video > 1 || audio > 1 {
            reject(D::TrackSelection);
        }
        let expected_scheme = match (input.container, input.encryption) {
            (InputContainer::FragmentedMp4, InputEncryption::SampleAes) => {
                InputProtectionScheme::Cbcs
            }
            (InputContainer::FragmentedMp4, InputEncryption::SampleAesCtr) => {
                InputProtectionScheme::Cenc
            }
            _ => InputProtectionScheme::None,
        };
        if input.scheme != expected_scheme {
            reject(D::ProtectionScheme);
        }
        if input.container == InputContainer::TransportStream {
            if input.encryption == InputEncryption::SampleAesCtr {
                reject(D::Encryption);
            }
            if input.encryption == InputEncryption::SampleAes
                && input.codecs.contains(&InputCodec::Hevc)
            {
                reject(D::Codec);
            }
        }
        if !matches!(
            input.key_source,
            InputKeySource::None | InputKeySource::ExternalProvider
        ) || (input.encryption != InputEncryption::Clear
            && input.key_source != InputKeySource::ExternalProvider)
        {
            reject(D::KeySource);
        }
        if input.encryption != InputEncryption::Clear
            && !requirements.contains(&R::ProviderResolution)
        {
            requirements.push(R::ProviderResolution);
        }
    }
    let mut reject = |dimension| rejections.push(dimension);
    if query.mode == SourceMode::RewrittenSnapshot {
        reject(D::SourceMode);
    }
    if matches!(
        query.output,
        OutputTarget::Mp4File | OutputTarget::FragmentedFile | OutputTarget::NativeStreamingFile
    ) && cfg!(target_arch = "wasm32")
    {
        reject(D::Output);
    }
    if matches!(
        query.range,
        RangeCapability::ArbitraryEncryptedRanges | RangeCapability::IFrameRanges
    ) {
        reject(D::Range);
    }
    if query.range == RangeCapability::CompleteEncryptedResources {
        requirements.push(R::CompleteEncryptionBoundaries);
    }
    if query.output == OutputTarget::FragmentedWriter {
        requirements.push(R::CallerOwnedWriterCompletion);
    }
    if query.resume && !native_recovery {
        reject(D::Resume);
    }
    if query.output == OutputTarget::Mp4Bytes && query.capacity.is_none_or(|n| n == 0) {
        reject(D::Output);
    }
    (rejections, requirements)
}

/// Container support is separate from a target player's rendering support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PlaybackTarget {
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
pub enum PlaybackRejection {
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
pub struct CapabilityQuery {
    inputs: Vec<(crate::playlist::InputId, InputCapability)>,
    output: OutputTarget,
    embedded: crate::EmbeddedAudio,
    mode: SourceMode,
    range: RangeCapability,
    subtitles: bool,
    subtitle_profile: SubtitleProfile,
    decode_gaps: bool,
    playback: PlaybackTarget,
    capacity: Option<usize>,
    waiter: bool,
    resume: bool,
    experimental_gcm: bool,
}
impl CapabilityQuery {
    pub fn new(
        id: crate::playlist::InputId,
        primary: InputCapability,
        output: OutputTarget,
        embedded: crate::EmbeddedAudio,
    ) -> Self {
        Self {
            inputs: vec![(id, primary)],
            output,
            embedded,
            mode: SourceMode::FiniteVod,
            range: RangeCapability::WholeResources,
            subtitles: false,
            subtitle_profile: SubtitleProfile::WvttPlainText,
            decode_gaps: false,
            playback: PlaybackTarget::Container,
            capacity: None,
            waiter: false,
            resume: false,
            experimental_gcm: false,
        }
    }
    pub fn with_audio(mut self, id: crate::playlist::InputId, input: InputCapability) -> Self {
        self.inputs.push((id, input));
        self
    }
    pub fn with_source_mode(mut self, mode: SourceMode) -> Self {
        self.mode = mode;
        self
    }
    pub fn with_range(mut self, range: RangeCapability) -> Self {
        self.range = range;
        self
    }
    pub fn with_subtitles(mut self, value: bool) -> Self {
        self.subtitles = value;
        self
    }
    /// Declare interior decode gaps. Native classic AAC uses composition offsets;
    /// other tracks use edit lists, and playback capability checks remain conservative.
    /// Video must resume at a sync sample with non-overlapping presentation runs.
    pub fn with_decode_gaps(mut self, value: bool) -> Self {
        self.decode_gaps = value;
        self
    }
    pub fn with_subtitle_profile(mut self, profile: SubtitleProfile) -> Self {
        self.subtitles = true;
        self.subtitle_profile = profile;
        self
    }
    pub fn with_playback(mut self, target: PlaybackTarget) -> Self {
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
    pub fn with_experimental_gcm(mut self, enabled: bool) -> Self {
        self.experimental_gcm = enabled;
        self
    }
    pub fn with_resume(mut self, value: bool) -> Self {
        self.resume = value;
        self
    }
}
#[derive(Debug, Clone)]
pub struct InputCapabilityRejection {
    input: Option<crate::playlist::InputId>,
    dimension: CapabilityDimension,
}
impl InputCapabilityRejection {
    pub fn input_id(&self) -> Option<&crate::playlist::InputId> {
        self.input.as_ref()
    }
    pub fn dimension(&self) -> CapabilityDimension {
        self.dimension
    }
}
#[derive(Debug, Clone)]
pub struct CapabilityDecision {
    rejections: Vec<InputCapabilityRejection>,
    container_rejections: Vec<InputCapabilityRejection>,
    requirements: Vec<CapabilityRequirement>,
    playback_rejection: Option<PlaybackRejection>,
}
impl CapabilityDecision {
    pub fn supported(&self) -> bool {
        self.rejections.is_empty()
    }
    /// Whether the engine can preserve the requested media in its output container,
    /// independently of limitations in the requested player.
    pub fn container_supported(&self) -> bool {
        self.container_rejections.is_empty()
    }
    pub fn container_rejections(&self) -> &[InputCapabilityRejection] {
        &self.container_rejections
    }
    /// Whether both engine output and the selected playback target support the request.
    pub fn playback_supported(&self) -> bool {
        self.container_supported() && self.playback_rejection.is_none()
    }
    pub fn rejections(&self) -> &[InputCapabilityRejection] {
        &self.rejections
    }
    pub fn requirements(&self) -> &[CapabilityRequirement] {
        &self.requirements
    }
    pub fn playback_rejection(&self) -> Option<PlaybackRejection> {
        self.playback_rejection
    }
}
/// No I/O; media bytes, keys, mappings and budgets still require runtime validation.
pub fn query_capability(query: &CapabilityQuery) -> CapabilityDecision {
    use CapabilityDimension as D;
    let mut result = CapabilityDecision {
        rejections: vec![],
        container_rejections: vec![],
        requirements: vec![],
        playback_rejection: None,
    };
    let native_recovery = query.resume
        && cfg!(not(target_arch = "wasm32"))
        && matches!(
            query.output,
            OutputTarget::FragmentedFile
                | OutputTarget::Mp4File
                | OutputTarget::NativeStreamingFile
        );
    if native_recovery {
        result.requirements.extend([
            CapabilityRequirement::NativeCheckpointPersistence,
            CapabilityRequirement::ReplayIdentityValidation,
        ]);
        if query
            .inputs
            .iter()
            .any(|(_, input)| input.encryption != InputEncryption::Clear)
        {
            result
                .requirements
                .push(CapabilityRequirement::StableKeyVersion);
        }
    }
    for (index, (id, input)) in query.inputs.iter().enumerate() {
        let mut selected = input.clone();
        if selected.encryption == InputEncryption::Aes256Gcm
            && query.experimental_gcm
            && cfg!(feature = "experimental-gcm")
        {
            selected.encryption = InputEncryption::Aes128;
            if !result
                .requirements
                .contains(&CapabilityRequirement::GcmDraft22Authentication)
            {
                result
                    .requirements
                    .push(CapabilityRequirement::GcmDraft22Authentication);
            }
        }
        if selected.container == InputContainer::PackedAac {
            selected.container = InputContainer::TransportStream;
            if selected.codecs != [InputCodec::AacLc] {
                result.rejections.push(InputCapabilityRejection {
                    input: Some(id.clone()),
                    dimension: D::Codec,
                });
            }
        }
        let (rejections, requirements) = validate_input(&selected, query, native_recovery);
        result
            .rejections
            .extend(
                rejections
                    .into_iter()
                    .map(|dimension| InputCapabilityRejection {
                        input: Some(id.clone()),
                        dimension,
                    }),
            );
        for requirement in requirements {
            if !result.requirements.contains(&requirement) {
                result.requirements.push(requirement);
            }
        }
        if (index > 0 && !input.codecs.contains(&InputCodec::AacLc))
            || (index == 0
                && query.embedded == crate::EmbeddedAudio::Exclude
                && !input
                    .codecs
                    .iter()
                    .any(|c| matches!(c, InputCodec::Avc | InputCodec::Hevc)))
            || query.inputs[..index].iter().any(|(other, _)| other == id)
        {
            result.rejections.push(InputCapabilityRejection {
                input: Some(id.clone()),
                dimension: D::TrackSelection,
            });
        }
    }
    if query.inputs.len() > 32 || (query.inputs.len() > 1 && !query.waiter) {
        result.rejections.push(InputCapabilityRejection {
            input: None,
            dimension: D::TrackSelection,
        });
    }
    use PlaybackRejection as PR;
    use PlaybackTarget as P;
    if query.subtitles && query.subtitle_profile != SubtitleProfile::WvttPlainText {
        result.rejections.push(InputCapabilityRejection {
            input: None,
            dimension: D::Subtitles,
        });
    }
    result.container_rejections = result.rejections.clone();
    let classic = matches!(
        query.output,
        OutputTarget::Mp4Bytes | OutputTarget::Mp4File | OutputTarget::NativeStreamingFile
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
    if result.playback_rejection.is_some() {
        result.rejections.push(InputCapabilityRejection {
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
