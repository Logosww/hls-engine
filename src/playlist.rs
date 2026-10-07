//! Immutable playlist metadata. Parsing is not an execution capability.
//!
//! The old prepared and legacy APIs continue using their original parser.
//! See the guide below for identity, transport serialization and limits.
#![doc = include_str!("../docs/typed-playlists.md")]
use crate::{SourceLocation, TextResource};
use std::{collections::BTreeMap, fmt};
mod parser;
#[cfg(feature = "serde")]
mod wire;
pub use parser::{parse_playlist_snapshot, parse_session_keys};
pub(crate) mod reconcile;

/// Parser failures never embed raw input, credentials, or signed URLs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PlaylistErrorKind {
    Syntax,
    DuplicateTag,
    InvalidAttribute,
    InvalidInteger,
    Overflow,
    InvalidDuration,
    InvalidDateTime,
    InvalidRange,
    InvalidIv,
    InvalidKey,
    InvalidLocation,
    MixedPlaylist,
    MasterPlaylist,
    SessionKeyConflict,
    InvalidIdentity,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistError {
    kind: PlaylistErrorKind,
    line: Option<usize>,
}
impl PlaylistError {
    pub fn kind(&self) -> PlaylistErrorKind {
        self.kind
    }
    pub fn line(&self) -> Option<usize> {
        self.line
    }
    fn new(kind: PlaylistErrorKind) -> Self {
        Self { kind, line: None }
    }
    fn at(mut self, line: usize) -> Self {
        if self.line.is_none() {
            self.line = Some(line)
        }
        self
    }
}
impl fmt::Display for PlaylistError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "playlist {:?} at line {:?}", self.kind, self.line)
    }
}
impl std::error::Error for PlaylistError {}
pub type PlaylistResult<T> = Result<T, PlaylistError>;

#[derive(Clone, PartialEq, Eq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(try_from = "String", into = "String")
)]
pub struct InputId(String);
impl InputId {
    pub fn new(value: impl Into<String>) -> PlaylistResult<Self> {
        let value = value.into();
        if value.is_empty() || value.chars().any(char::is_control) {
            return Err(PlaylistError::new(PlaylistErrorKind::InvalidIdentity));
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for InputId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InputId(..)")
    }
}
impl TryFrom<String> for InputId {
    type Error = PlaylistError;
    fn try_from(v: String) -> PlaylistResult<Self> {
        Self::new(v)
    }
}
impl From<InputId> for String {
    fn from(v: InputId) -> Self {
        v.0
    }
}

/// Caller-owned finite snapshot namespace. Reuse a revision only for the same snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(deny_unknown_fields)
)]
pub struct PlaylistContext {
    input_id: InputId,
    #[cfg_attr(feature = "serde", serde(with = "wire::u64_string"))]
    generation: u64,
    #[cfg_attr(feature = "serde", serde(with = "wire::u64_string"))]
    revision: u64,
}
impl PlaylistContext {
    pub fn new(input_id: InputId, generation: u64) -> Self {
        Self {
            input_id,
            generation,
            revision: 0,
        }
    }
    pub fn with_revision(mut self, revision: u64) -> Self {
        self.revision = revision;
        self
    }
    pub fn input_id(&self) -> &InputId {
        &self.input_id
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
}

/// Location with safe Debug; access/serialization deliberately retain transport credentials.
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(try_from = "wire::Location", into = "wire::Location")
)]
pub struct ResourceLocation(SourceLocation);
impl ResourceLocation {
    pub fn location(&self) -> &SourceLocation {
        &self.0
    }
    pub fn diagnostic(&self) -> String {
        match &self.0 {
            SourceLocation::Url(u) if !matches!(u.scheme(), "http" | "https") => {
                "[REDACTED]".to_owned()
            }
            _ => crate::source::safe_location(&self.0),
        }
    }
    fn resolve(&self, uri: &str) -> PlaylistResult<Self> {
        if uri.is_empty() || uri.chars().any(char::is_control) {
            return Err(PlaylistError::new(PlaylistErrorKind::InvalidLocation));
        }
        // URL bases accept non-HTTP key schemes for caller providers; media execution checks schemes later.
        let result = match &self.0 {
            SourceLocation::Url(base) => base
                .join(uri)
                .map(SourceLocation::Url)
                .map_err(|_| PlaylistError::new(PlaylistErrorKind::InvalidLocation)),
            SourceLocation::File(_) if std::path::Path::new(uri).is_absolute() => {
                Ok(SourceLocation::File(uri.into()))
            }
            SourceLocation::File(_) => match url::Url::parse(uri) {
                Ok(url) => Ok(SourceLocation::Url(url)),
                Err(_) => self
                    .0
                    .resolve(uri)
                    .map_err(|_| PlaylistError::new(PlaylistErrorKind::InvalidLocation)),
            },
        }?;
        Ok(Self(result))
    }
}
impl fmt::Debug for ResourceLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Opaque/non-HTTP provider URI paths can themselves be credentials.
        match &self.0 {
            SourceLocation::Url(u) if !matches!(u.scheme(), "http" | "https") => {
                f.write_str("ResourceLocation([REDACTED])")
            }
            _ => f
                .debug_tuple("ResourceLocation")
                .field(&self.diagnostic())
                .finish(),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(deny_unknown_fields)
)]
pub struct ResourceRange {
    #[cfg_attr(feature = "serde", serde(with = "wire::u64_string"))]
    offset: u64,
    #[cfg_attr(feature = "serde", serde(with = "wire::u64_string"))]
    length: u64,
}
impl ResourceRange {
    pub fn offset(&self) -> u64 {
        self.offset
    }
    pub fn length(&self) -> u64 {
        self.length
    }
    pub fn byte_range(&self) -> crate::ByteRange {
        crate::ByteRange {
            offset: self.offset,
            length: self.length,
        }
    }
}
/// Exact decimal duration; at most nanosecond precision, with checked u64 ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(deny_unknown_fields)
)]
pub struct PlaylistDuration {
    #[cfg_attr(feature = "serde", serde(with = "wire::u64_string"))]
    ticks: u64,
    timescale: u32,
}
impl PlaylistDuration {
    pub(crate) fn from_time(value: crate::MediaTime) -> crate::Result<Self> {
        Ok(Self {
            ticks: u64::try_from(value.ticks())
                .map_err(|_| crate::Error::InvalidInput("negative gap duration".into()))?,
            timescale: value.timescale(),
        })
    }
    pub fn ticks(&self) -> u64 {
        self.ticks
    }
    pub fn timescale(&self) -> u32 {
        self.timescale
    }
    pub fn media_time(&self) -> crate::Result<crate::MediaTime> {
        crate::MediaTime::new(i128::from(self.ticks), self.timescale)
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum PlaylistType {
    Vod,
    Event,
}
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum EncryptionMethod {
    Aes128,
    SampleAes,
    SampleAesCtr,
    Aes256Gcm,
    Other(String),
}
impl EncryptionMethod {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Aes128 => "AES-128",
            Self::SampleAes => "SAMPLE-AES",
            Self::SampleAesCtr => "SAMPLE-AES-CTR",
            Self::Aes256Gcm => "AES-256-GCM",
            Self::Other(v) => v,
        }
    }
}
impl fmt::Debug for EncryptionMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Other(_) => f.write_str("Other(..)"),
            _ => f.write_str(self.as_str()),
        }
    }
}
/// A declaration within an immutable snapshot, NOT a reconciled live key epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(deny_unknown_fields)
)]
pub struct DeclarationId {
    #[cfg_attr(feature = "serde", serde(with = "wire::u64_string"))]
    revision: u64,
    #[cfg_attr(feature = "serde", serde(with = "wire::u64_string"))]
    ordinal: u64,
}
impl DeclarationId {
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn ordinal(&self) -> u64 {
        self.ordinal
    }
}
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(deny_unknown_fields)
)]
pub struct KeyReference {
    declaration: DeclarationId,
    method: EncryptionMethod,
    location: ResourceLocation,
    format: String,
    versions: Vec<u32>,
    iv: Option<[u8; 16]>,
    extensions: BTreeMap<String, String>,
}
impl KeyReference {
    pub fn declaration(&self) -> DeclarationId {
        self.declaration
    }
    pub fn method(&self) -> &EncryptionMethod {
        &self.method
    }
    pub fn location(&self) -> &ResourceLocation {
        &self.location
    }
    pub fn format(&self) -> &str {
        &self.format
    }
    pub fn versions(&self) -> &[u32] {
        &self.versions
    }
    pub fn explicit_iv(&self) -> Option<[u8; 16]> {
        self.iv
    }
    pub fn extensions(&self) -> &BTreeMap<String, String> {
        &self.extensions
    }
}
impl fmt::Debug for KeyReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyReference")
            .field("declaration", &self.declaration)
            .field("method", &self.method)
            .field("location", &self.location)
            .finish_non_exhaustive()
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(deny_unknown_fields)
)]
pub struct KeyContext {
    candidates: Vec<KeyReference>,
}
impl KeyContext {
    pub fn candidates(&self) -> &[KeyReference] {
        &self.candidates
    }
    pub fn is_clear(&self) -> bool {
        self.candidates.is_empty()
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(deny_unknown_fields)
)]
pub struct MapDescriptor {
    declaration: DeclarationId,
    location: ResourceLocation,
    range: Option<ResourceRange>,
    keys: KeyContext,
}
impl MapDescriptor {
    pub fn declaration(&self) -> DeclarationId {
        self.declaration
    }
    pub fn location(&self) -> &ResourceLocation {
        &self.location
    }
    pub fn range(&self) -> Option<ResourceRange> {
        self.range
    }
    pub fn keys(&self) -> &KeyContext {
        &self.keys
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(deny_unknown_fields)
)]
pub struct SegmentSlot {
    input_id: InputId,
    #[cfg_attr(feature = "serde", serde(with = "wire::u64_string"))]
    generation: u64,
    #[cfg_attr(feature = "serde", serde(with = "wire::u64_string"))]
    sequence: u64,
    #[cfg_attr(feature = "serde", serde(with = "wire::u64_string"))]
    epoch: u64,
}
impl SegmentSlot {
    pub fn input_id(&self) -> &InputId {
        &self.input_id
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(deny_unknown_fields)
)]
pub struct SegmentDescriptor {
    slot: SegmentSlot,
    location: ResourceLocation,
    range: Option<ResourceRange>,
    duration: PlaylistDuration,
    program_date_time: Option<String>,
    gap: bool,
    discontinuity: bool,
    map: Option<MapDescriptor>,
    keys: KeyContext,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResourceComparison {
    DifferentSlot,
    Duplicate,
    Rewritten,
    NeedsReconciliation,
}
impl SegmentDescriptor {
    pub(crate) fn recovery_gap(slot: SegmentSlot, duration: PlaylistDuration) -> Self {
        Self {
            slot,
            location: ResourceLocation(SourceLocation::File("hls-engine-evicted".into())),
            range: None,
            duration,
            program_date_time: None,
            gap: true,
            discontinuity: false,
            map: None,
            keys: KeyContext { candidates: vec![] },
        }
    }
    pub fn slot(&self) -> &SegmentSlot {
        &self.slot
    }
    pub fn location(&self) -> &ResourceLocation {
        &self.location
    }
    pub fn range(&self) -> Option<ResourceRange> {
        self.range
    }
    pub fn duration(&self) -> PlaylistDuration {
        self.duration
    }
    /// An explicit source annotation only; no interpolated or inferred wall clock.
    pub fn program_date_time(&self) -> Option<&str> {
        self.program_date_time.as_deref()
    }
    pub fn gap(&self) -> bool {
        self.gap
    }
    pub fn discontinuity(&self) -> bool {
        self.discontinuity
    }
    pub fn map(&self) -> Option<&MapDescriptor> {
        self.map.as_ref()
    }
    pub fn keys(&self) -> &KeyContext {
        &self.keys
    }
    pub fn compare_resource(&self, other: &Self) -> ResourceComparison {
        if self.slot != other.slot {
            return ResourceComparison::DifferentSlot;
        }
        let revision = |s: &Self| {
            s.keys
                .candidates
                .first()
                .map(|k| k.declaration.revision)
                .or_else(|| s.map.as_ref().map(|m| m.declaration.revision))
        };
        if revision(self) != revision(other)
            && revision(self).is_some()
            && revision(other).is_some()
        {
            return ResourceComparison::NeedsReconciliation;
        }
        if self == other {
            ResourceComparison::Duplicate
        } else {
            ResourceComparison::Rewritten
        }
    }
}
/// A retained tag. Its raw text is explicit transport data and omitted from Debug.
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(deny_unknown_fields)
)]
pub struct RetainedTag {
    text: String,
}
impl RetainedTag {
    pub fn text(&self) -> &str {
        &self.text
    }
}
impl fmt::Debug for RetainedTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RetainedTag(..)")
    }
}
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(deny_unknown_fields)
)]
struct SnapshotData {
    schema_version: u32,
    #[cfg_attr(feature = "serde", serde(with = "wire::optional_u64_string"))]
    version: Option<u64>,
    context: PlaylistContext,
    location: ResourceLocation,
    // Retain source for validated, lossless reconstruction across an untrusted serde boundary.
    source: String,
    playlist_type: Option<PlaylistType>,
    end_list: bool,
    #[cfg_attr(feature = "serde", serde(with = "wire::optional_u64_string"))]
    target_duration: Option<u64>,
    #[cfg_attr(feature = "serde", serde(with = "wire::u64_string"))]
    media_sequence: u64,
    #[cfg_attr(feature = "serde", serde(with = "wire::u64_string"))]
    discontinuity_sequence: u64,
    independent_segments: bool,
    iframe_only: bool,
    segments: Vec<SegmentDescriptor>,
    retained_tags: Vec<RetainedTag>,
}
/// Immutable parsed metadata. Serde reconstructs and verifies the projection against its source.
#[derive(Clone, PartialEq, Eq)]
pub struct PlaylistSnapshot(SnapshotData);
impl fmt::Debug for PlaylistSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlaylistSnapshot")
            .field("context", &self.0.context)
            .field("location", &self.0.location)
            .field("segments", &self.0.segments.len())
            .finish_non_exhaustive()
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PlaylistRejection {
    OpenInput,
    Event,
    Empty,
    MissingTargetDuration,
    DurationExceedsTarget,
    Gap,
    Discontinuity,
    IFrameOnly,
    UnvalidatedTag,
    SampleEncryption,
    UnknownEncryption,
    MissingMapIv,
    ConflictingKeyMethods,
}
impl PlaylistSnapshot {
    pub fn version(&self) -> Option<u64> {
        self.0.version
    }
    pub fn context(&self) -> &PlaylistContext {
        &self.0.context
    }
    pub fn location(&self) -> &ResourceLocation {
        &self.0.location
    }
    pub fn playlist_type(&self) -> Option<&PlaylistType> {
        self.0.playlist_type.as_ref()
    }
    pub fn end_list(&self) -> bool {
        self.0.end_list
    }
    pub fn target_duration(&self) -> Option<u64> {
        self.0.target_duration
    }
    pub fn media_sequence(&self) -> u64 {
        self.0.media_sequence
    }
    pub fn discontinuity_sequence(&self) -> u64 {
        self.0.discontinuity_sequence
    }
    pub fn independent_segments(&self) -> bool {
        self.0.independent_segments
    }
    pub fn iframe_only(&self) -> bool {
        self.0.iframe_only
    }
    pub fn segments(&self) -> &[SegmentDescriptor] {
        &self.0.segments
    }
    pub fn retained_tags(&self) -> &[RetainedTag] {
        &self.0.retained_tags
    }
    /// Manifest-only preflight for the planned v0.6 finite profile. Does NOT enable execution,
    /// validate provider/codec/container support, or fetch resources.
    pub fn validate_finite_vod(&self) -> Result<(), PlaylistRejection> {
        self.validate_finite_profile(false, false)
    }
    /// Validate the finite timeline profile. GAP and discontinuity are retained
    /// for the timeline executor; this is not a codec or timeline validation.
    pub fn validate_timeline_vod(&self) -> Result<(), PlaylistRejection> {
        self.validate_finite_profile(true, false)
    }
    /// Finite sample-encryption manifest profile; container validation is deferred to demux.
    pub fn validate_finite_sample_vod(&self) -> Result<(), PlaylistRejection> {
        self.validate_finite_profile(false, true)
    }
    /// Timeline sample-encryption profile, retaining GAP and discontinuity.
    pub fn validate_timeline_sample_vod(&self) -> Result<(), PlaylistRejection> {
        self.validate_finite_profile(true, true)
    }
    /// Open-input manifest validation. Execution and incremental identity checks
    /// are performed by `ContinuousSession`; this does not change finite profiles.
    pub fn validate_continuous(&self) -> Result<(), PlaylistRejection> {
        self.validate_media_profile(true, true, false)
    }
    fn validate_finite_profile(
        &self,
        timeline: bool,
        samples: bool,
    ) -> Result<(), PlaylistRejection> {
        if !self.0.end_list {
            return Err(PlaylistRejection::OpenInput);
        }
        if self.0.playlist_type == Some(PlaylistType::Event) {
            return Err(PlaylistRejection::Event);
        }
        if self.0.segments.is_empty() {
            return Err(PlaylistRejection::Empty);
        }
        self.validate_media_profile(timeline, samples, false)
    }
    pub(crate) fn validate_engine(&self, gcm: bool) -> Result<(), PlaylistRejection> {
        self.validate_media_profile(true, true, gcm && cfg!(feature = "experimental-gcm"))
    }
    fn validate_media_profile(
        &self,
        timeline: bool,
        samples: bool,
        gcm: bool,
    ) -> Result<(), PlaylistRejection> {
        let target = self
            .0
            .target_duration
            .ok_or(PlaylistRejection::MissingTargetDuration)?;
        if self.0.iframe_only {
            return Err(PlaylistRejection::IFrameOnly);
        }
        if !self.0.retained_tags.is_empty() {
            return Err(PlaylistRejection::UnvalidatedTag);
        }
        for segment in &self.0.segments {
            let scale = u64::from(segment.duration.timescale);
            let rounded = segment.duration.ticks / scale
                + u64::from((segment.duration.ticks % scale) * 2 >= scale);
            if rounded > target {
                return Err(PlaylistRejection::DurationExceedsTarget);
            }
            if segment.gap && !timeline {
                return Err(PlaylistRejection::Gap);
            }
            if segment.discontinuity && !timeline {
                return Err(PlaylistRejection::Discontinuity);
            }
            for (keys, map) in std::iter::once((&segment.keys, false))
                .chain(segment.map.iter().map(|m| (&m.keys, true)))
            {
                if let Some(first) = keys.candidates.first() {
                    if keys.candidates.iter().any(|k| k.method != first.method) {
                        return Err(PlaylistRejection::ConflictingKeyMethods);
                    }
                    match first.method {
                        EncryptionMethod::Aes128 => {}
                        EncryptionMethod::Aes256Gcm
                            if gcm && keys.candidates.iter().all(|k| k.iv.is_none()) => {}
                        EncryptionMethod::SampleAes | EncryptionMethod::SampleAesCtr => {
                            if !samples {
                                return Err(PlaylistRejection::SampleEncryption);
                            }
                        }
                        _ => return Err(PlaylistRejection::UnknownEncryption),
                    }
                }
                if keys.candidates.iter().any(|k| !k.extensions.is_empty()) {
                    return Err(PlaylistRejection::UnvalidatedTag);
                }
                if map
                    && keys
                        .candidates
                        .iter()
                        .any(|k| k.method == EncryptionMethod::Aes128 && k.iv.is_none())
                {
                    return Err(PlaylistRejection::MissingMapIv);
                }
            }
        }
        Ok(())
    }
    /// Check master SESSION-KEY hints against media KEYs, including keys frozen on MAPs.
    /// Matching hints never replace media encryption state or cause fetching.
    pub fn validate_session_keys(&self, hints: &[KeyReference]) -> PlaylistResult<()> {
        for key in self.0.segments.iter().flat_map(|s| {
            s.keys
                .candidates
                .iter()
                .chain(s.map.iter().flat_map(|m| m.keys.candidates.iter()))
        }) {
            for hint in hints.iter().filter(|h| h.location == key.location) {
                if hint.method != key.method
                    || hint.format != key.format
                    || hint.versions != key.versions
                {
                    return Err(PlaylistError::new(PlaylistErrorKind::SessionKeyConflict));
                }
            }
        }
        Ok(())
    }
}

impl crate::state_codec::StateCodec for InputId {
    fn put(&self, out: &mut Vec<u8>) {
        self.0.put(out);
    }
    fn get(r: &mut crate::state_codec::Reader<'_>) -> crate::state_codec::DecodeResult<Self> {
        Self::new(<String as crate::state_codec::StateCodec>::get(r)?).map_err(|_| ())
    }
}
crate::state_codec::state_struct!(SegmentSlot {
    input_id,
    generation,
    sequence,
    epoch
});

impl KeyReference {
    pub(crate) fn checkpoint_identity(&self) -> [u8; 32] {
        use crate::state_codec::StateCodec;
        let mut bytes = Vec::new();
        self.method.as_str().to_owned().put(&mut bytes);
        location_identity(&self.location).put(&mut bytes);
        self.format.put(&mut bytes);
        self.versions.put(&mut bytes);
        self.iv.put(&mut bytes);
        self.extensions.len().put(&mut bytes);
        for (name, value) in &self.extensions {
            name.put(&mut bytes);
            value.put(&mut bytes);
        }
        crate::resume::digest(&bytes)
    }
}
fn location_identity(location: &ResourceLocation) -> [u8; 32] {
    let bytes = match location.location() {
        SourceLocation::Url(u) => u.as_str().as_bytes(),
        SourceLocation::File(p) => p.as_os_str().as_encoded_bytes(),
    };
    crate::resume::digest(bytes)
}
impl SegmentDescriptor {
    pub(crate) fn checkpoint_identity(&self) -> [u8; 32] {
        use crate::state_codec::StateCodec;
        let mut bytes = Vec::new();
        self.slot.put(&mut bytes);
        location_identity(&self.location).put(&mut bytes);
        self.range
            .map(|r| (r.byte_range().offset, r.byte_range().length))
            .put(&mut bytes);
        self.duration.ticks.put(&mut bytes);
        self.duration.timescale.put(&mut bytes);
        self.program_date_time.put(&mut bytes);
        self.gap.put(&mut bytes);
        // Absolute epoch is already part of the slot. A rolling manifest can
        // replace the leading discontinuity marker with DISCONTINUITY-SEQUENCE.
        self.keys
            .candidates
            .iter()
            .map(KeyReference::checkpoint_identity)
            .collect::<Vec<_>>()
            .put(&mut bytes);
        self.map
            .as_ref()
            .map(|m| {
                let mut b = Vec::new();
                location_identity(&m.location).put(&mut b);
                m.range
                    .map(|r| (r.byte_range().offset, r.byte_range().length))
                    .put(&mut b);
                m.keys
                    .candidates
                    .iter()
                    .map(KeyReference::checkpoint_identity)
                    .collect::<Vec<_>>()
                    .put(&mut b);
                crate::resume::digest(&b)
            })
            .put(&mut bytes);
        crate::resume::digest(&bytes)
    }
}

impl crate::state_codec::StateCodec for PlaylistDuration {
    fn put(&self, out: &mut Vec<u8>) {
        self.ticks.put(out);
        self.timescale.put(out);
    }
    fn get(r: &mut crate::state_codec::Reader<'_>) -> crate::state_codec::DecodeResult<Self> {
        let value = Self {
            ticks: u64::get(r)?,
            timescale: u32::get(r)?,
        };
        if value.timescale == 0 {
            return Err(());
        }
        Ok(value)
    }
}
