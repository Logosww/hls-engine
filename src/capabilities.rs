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
            KeyedEncryption::Clear | KeyedEncryption::Aes128 | KeyedEncryption::ClearAndAes128
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
        if input.scheme != KeyedProtectionScheme::None {
            reject(D::ProtectionScheme);
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
