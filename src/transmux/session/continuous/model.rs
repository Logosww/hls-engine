use super::*;

pub type ContinuousResult<T> = std::result::Result<T, ContinuousError>;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContinuousErrorKind {
    AuthenticationFailed,
    ResumeConflict,
    ResumeCorruption,
    ReplayRequired,
    InvalidSubtitle,
    SubtitleOutput,
    UnsupportedSubtitleProfile,
    MissingSubtitleMapping,
    InvalidOptions,
    UnknownInput,
    UnsupportedPlaylist,
    InputRewrite,
    RevisionRollback,
    GenerationMismatch,
    NeedsReconciliation,
    MissingSegment,
    QueueLimit,
    WouldBlock,
    Closed,
    PauseUnsupported,
    Resource,
    Media,
    TimelineAmbiguous,
    MissingRandomAccess,
    MissingTailDuration,
    ConfigurationChanged,
    BudgetExceeded,
    SkewTimeout,
    Output,
    Cancelled,
    TimeOverflow,
    EmptyInput,
}
/// Structured causes retain their typed diagnostics and redacted Display policy.
#[non_exhaustive]
pub enum EngineCause<'a> {
    Resource(&'a ResourceError),
    Sample(&'a crate::crypto::sample::SampleError),
    Media(&'a Error),
}
impl std::fmt::Debug for EngineCause<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Resource(e) => f.debug_tuple("Resource").field(e).finish(),
            Self::Sample(e) => f.debug_tuple("Sample").field(e).finish(),
            Self::Media(_) => f.write_str("Media([REDACTED])"),
        }
    }
}
/// Redacted diagnostics. Inspect typed causes explicitly; they are never formatted here.
pub struct ContinuousError {
    pub(super) kind: ContinuousErrorKind,
    pub(super) slot: Option<SegmentSlot>,
    pub(super) resource: Option<Box<ResourceError>>,
    pub(super) sample: Option<Box<crate::crypto::sample::SampleError>>,
    pub(super) cause: Option<Box<Error>>,
    pub(super) completed: Vec<ContinuousOutputReport>,
}
impl ContinuousError {
    /// Report a host sink acquisition or write failure without exposing it in Display.
    pub fn output(cause: crate::Error) -> Self {
        output_error(cause)
    }
    pub fn completed_outputs(&self) -> &[ContinuousOutputReport] {
        &self.completed
    }
    pub fn kind(&self) -> ContinuousErrorKind {
        self.kind
    }
    pub fn slot(&self) -> Option<&SegmentSlot> {
        self.slot
            .as_ref()
            .or_else(|| self.resource().map(|r| r.slot()))
    }
    pub fn resource(&self) -> Option<&crate::crypto::key::KeyResource> {
        self.resource
            .as_ref()
            .and_then(|e| e.resource())
            .or_else(|| self.sample.as_ref().map(|e| e.resource()))
    }
    pub fn input_id(&self) -> Option<&InputId> {
        self.slot().map(|s| s.input_id())
    }
    pub fn generation(&self) -> Option<u64> {
        self.slot().map(|s| s.generation())
    }
    pub fn epoch(&self) -> Option<u64> {
        self.slot().map(|s| s.epoch())
    }
    /// Track ID in the encoded input resource, when supplied by sample decryption.
    pub fn source_track_id(&self) -> Option<u32> {
        self.sample.as_ref().and_then(|e| e.track_id())
    }
    pub fn sample_index(&self) -> Option<usize> {
        self.sample.as_ref().and_then(|e| e.sample_index())
    }
    pub fn cause(&self) -> Option<EngineCause<'_>> {
        self.resource
            .as_deref()
            .map(EngineCause::Resource)
            .or_else(|| self.sample.as_deref().map(EngineCause::Sample))
            .or_else(|| self.cause.as_deref().map(EngineCause::Media))
    }
    pub fn resource_error(&self) -> Option<&ResourceError> {
        self.resource.as_deref()
    }
    pub fn sample_error(&self) -> Option<&crate::crypto::sample::SampleError> {
        self.sample.as_deref()
    }
    pub fn raw_cause(&self) -> Option<&Error> {
        self.cause.as_deref()
    }
}
impl std::fmt::Debug for ContinuousError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContinuousError")
            .field("kind", &self.kind)
            .field("slot", &self.slot)
            .finish_non_exhaustive()
    }
}
impl std::fmt::Display for ContinuousError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "continuous session {:?}", self.kind)
    }
}
impl std::error::Error for ContinuousError {}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContinuousState {
    Preparing,
    Running,
    Paused,
    Draining,
    Finalizing,
    Completed,
    Failed,
    Cancelled,
}
impl ContinuousState {
    pub(super) fn terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContinuousEndReason {
    Eof,
    Stop,
    DurationLimit,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinuousMode {
    Vod,
    Open,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingSegmentPolicy {
    Fail,
    Skip,
    Split,
}
/// Host-provided timer, including a JS Promise on WASM. No core timer is required.
#[cfg(not(target_arch = "wasm32"))]
pub trait ContinuousWait: Send + Sync {
    fn wait(&self, duration: std::time::Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}
#[cfg(target_arch = "wasm32")]
pub trait ContinuousWait {
    fn wait(&self, duration: std::time::Duration) -> Pin<Box<dyn Future<Output = ()> + '_>>;
}
#[cfg(not(target_arch = "wasm32"))]
pub type ContinuousEventCallback = dyn Fn(ContinuousEvent) + Send + Sync;
#[cfg(target_arch = "wasm32")]
pub type ContinuousEventCallback = dyn Fn(ContinuousEvent);

/// All limits are per operation. Caller snapshots, output collectors and classic
/// finalization indexes are separate costs, not a process RSS guarantee.
#[derive(Debug, Clone)]
pub struct ContinuousLimits {
    pub(super) descriptors: usize,
    pub(super) metadata: usize,
    pub(super) history: usize,
    pub(super) samples: usize,
    pub(super) sample_bytes: usize,
    pub(super) probe: usize,
    pub(super) skew: MediaTime,
}
impl Default for ContinuousLimits {
    fn default() -> Self {
        Self {
            descriptors: 128,
            metadata: 4 * 1024 * 1024,
            history: 128,
            samples: 65_536,
            sample_bytes: 64 * 1024 * 1024,
            probe: 2,
            skew: MediaTime {
                ticks: 30,
                timescale: 1,
            },
        }
    }
}
impl ContinuousLimits {
    pub fn queued_descriptors(&self) -> usize {
        self.descriptors
    }
    pub fn queued_metadata_bytes(&self) -> usize {
        self.metadata
    }
    pub fn history_entries(&self) -> usize {
        self.history
    }
    pub fn samples(&self) -> usize {
        self.samples
    }
    pub fn sample_bytes(&self) -> usize {
        self.sample_bytes
    }
    pub fn probe_segments(&self) -> usize {
        self.probe
    }
    pub fn max_skew(&self) -> MediaTime {
        self.skew
    }

    pub fn with_queue(mut self, descriptors: usize, metadata_bytes: usize) -> Self {
        self.descriptors = descriptors;
        self.metadata = metadata_bytes;
        self
    }
    pub fn with_history(mut self, entries: usize) -> Self {
        self.history = entries;
        self
    }
    pub fn with_samples(mut self, samples: usize, bytes: usize) -> Self {
        self.samples = samples;
        self.sample_bytes = bytes;
        self
    }
    pub fn with_probe_segments(mut self, segments: usize) -> Self {
        self.probe = segments;
        self
    }
    pub fn with_max_skew(mut self, skew: MediaTime) -> Self {
        self.skew = skew;
        self
    }
}
#[derive(Clone)]
pub struct ContinuousOptions {
    pub(super) mode: ContinuousMode,
    pub(super) limits: ContinuousLimits,
    pub(super) resources: ResourceOptions,
    pub(super) missing: MissingSegmentPolicy,
    pub(super) gaps: GapPolicy,
    pub(super) changes: TimelineChangePolicy,
    pub(super) tail: TailDurationPolicy,
    pub(super) duration: Option<MediaTime>,
    pub(super) range: Option<PresentationRange>,
    pub(super) anchors: Vec<ContinuousAnchor>,
    pub(super) waiter: Option<Arc<dyn ContinuousWait>>,
    pub(super) timeout: std::time::Duration,
    pub(super) event: Option<Arc<ContinuousEventCallback>>,
}
impl Default for ContinuousOptions {
    fn default() -> Self {
        Self {
            mode: ContinuousMode::Open,
            limits: ContinuousLimits::default(),
            resources: ResourceOptions::default(),
            missing: MissingSegmentPolicy::Fail,
            gaps: GapPolicy::Preserve,
            changes: TimelineChangePolicy::Fail,
            tail: TailDurationPolicy::RequireEvidence,
            duration: None,
            range: None,
            anchors: vec![],
            waiter: None,
            timeout: std::time::Duration::from_secs(30),
            event: None,
        }
    }
}
impl ContinuousOptions {
    pub fn mode(&self) -> ContinuousMode {
        self.mode
    }
    pub fn limits(&self) -> &ContinuousLimits {
        &self.limits
    }
    pub fn resources(&self) -> &ResourceOptions {
        &self.resources
    }
    pub fn missing_segments(&self) -> MissingSegmentPolicy {
        self.missing
    }
    pub fn gap_policy(&self) -> GapPolicy {
        self.gaps
    }
    pub fn change_policy(&self) -> TimelineChangePolicy {
        self.changes
    }
    pub fn tail_policy(&self) -> TailDurationPolicy {
        self.tail
    }
    pub fn duration_limit(&self) -> Option<MediaTime> {
        self.duration
    }
    pub fn range(&self) -> Option<PresentationRange> {
        self.range
    }
    pub fn anchors(&self) -> &[ContinuousAnchor] {
        &self.anchors
    }
    pub fn input_timeout(&self) -> std::time::Duration {
        self.timeout
    }

    /// Opt in to HLS draft-22 GCM (also requires `experimental-gcm`).
    pub fn with_experimental_gcm(mut self, enabled: bool) -> Self {
        self.resources = self.resources.with_experimental_gcm(enabled);
        self
    }

    pub fn with_mode(mut self, mode: ContinuousMode) -> Self {
        self.mode = mode;
        self
    }
    pub fn with_limits(mut self, limits: ContinuousLimits) -> Self {
        self.limits = limits;
        self
    }
    pub fn with_resources(mut self, resources: ResourceOptions) -> Self {
        self.resources = resources;
        self
    }
    pub fn with_missing_segments(mut self, policy: MissingSegmentPolicy) -> Self {
        self.missing = policy;
        self
    }
    pub fn with_gap_policy(mut self, policy: GapPolicy) -> Self {
        self.gaps = policy;
        self
    }
    pub fn with_change_policy(mut self, policy: TimelineChangePolicy) -> Self {
        self.changes = policy;
        self
    }
    pub fn with_tail_policy(mut self, policy: TailDurationPolicy) -> Self {
        self.tail = policy;
        self
    }
    pub fn with_duration_limit(mut self, duration: MediaTime) -> Self {
        self.duration = Some(duration);
        self
    }
    pub fn with_range(mut self, range: PresentationRange) -> Self {
        self.range = Some(range);
        self
    }
    pub fn with_anchor(mut self, anchor: ContinuousAnchor) -> Self {
        self.anchors.push(anchor);
        self
    }
    pub fn with_waiter(
        mut self,
        waiter: Arc<dyn ContinuousWait>,
        timeout: std::time::Duration,
    ) -> Self {
        self.waiter = Some(waiter);
        self.timeout = timeout;
        self
    }
    pub fn with_on_event(mut self, event: Arc<ContinuousEventCallback>) -> Self {
        self.event = Some(event);
        self
    }
    pub(super) fn validate(&self, inputs: usize) -> ContinuousResult<()> {
        let b = &self.limits;
        if b.descriptors == 0
            || b.metadata == 0
            || b.history == 0
            || b.samples == 0
            || b.sample_bytes == 0
            || b.probe == 0
            || b.skew.ticks <= 0
            || b.skew.timescale == 0
            || self.timeout.is_zero()
            || self
                .duration
                .is_some_and(|v| v.ticks <= 0 || v.timescale == 0)
            || (inputs > 1 && self.waiter.is_none())
            || self
                .anchors
                .iter()
                .any(|a| a.source.timescale == 0 || a.presentation.timescale == 0)
            || matches!(self.tail, TailDurationPolicy::Explicit(v) if v.ticks <= 0 || v.timescale == 0)
        {
            return Err(fail(ContinuousErrorKind::InvalidOptions));
        }
        self.resources.validate().map_err(resource_error)
    }
}
/// Explicit cross-epoch time evidence, scoped to a source generation.
#[derive(Debug, Clone)]
pub struct ContinuousAnchor {
    pub(super) input: InputId,
    pub(super) generation: u64,
    pub(super) epoch: u64,
    pub(super) source: MediaTime,
    pub(super) presentation: MediaTime,
}
impl ContinuousAnchor {
    pub fn input_id(&self) -> &InputId {
        &self.input
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn source_time(&self) -> MediaTime {
        self.source
    }
    pub fn presentation_time(&self) -> MediaTime {
        self.presentation
    }

    pub fn new(
        input: InputId,
        generation: u64,
        epoch: u64,
        source: MediaTime,
        presentation: MediaTime,
    ) -> Self {
        Self {
            input,
            generation,
            epoch,
            source,
            presentation,
        }
    }
}
#[derive(Clone)]
pub struct ContinuousInput {
    pub(super) id: InputId,
    pub(super) source: Arc<dyn Source>,
}
impl ContinuousInput {
    pub fn input_id(&self) -> &InputId {
        &self.id
    }

    pub fn new(id: InputId, source: Arc<dyn Source>) -> Self {
        Self { id, source }
    }
}
pub struct ContinuousInputs {
    pub(super) inputs: Vec<ContinuousInput>,
}
impl ContinuousInputs {
    pub fn new(primary: ContinuousInput) -> Self {
        Self {
            inputs: vec![primary],
        }
    }
    /// Replace the primary's embedded audio, as in the finite prepared API.
    pub fn with_audio(mut self, audio: ContinuousInput) -> Self {
        self.inputs.truncate(1);
        self.inputs.push(audio);
        self
    }
}
#[derive(Debug, Clone)]
pub struct ContinuousInputProgress {
    pub(super) input: InputId,
    pub(super) discovered: u64,
    pub(super) accepted: u64,
    pub(super) downloaded: u64,
    pub(super) decrypted: u64,
    pub(super) committed: u64,
    pub(super) watermark: Option<SegmentSlot>,
}
impl ContinuousInputProgress {
    pub fn input_id(&self) -> &InputId {
        &self.input
    }
    pub fn total(&self) -> Option<u64> {
        None
    }
    pub fn discovered(&self) -> u64 {
        self.discovered
    }
    pub fn accepted(&self) -> u64 {
        self.accepted
    }
    pub fn downloaded(&self) -> u64 {
        self.downloaded
    }
    pub fn decrypted(&self) -> u64 {
        self.decrypted
    }
    pub fn committed(&self) -> u64 {
        self.committed
    }
    pub fn committed_slot(&self) -> Option<&SegmentSlot> {
        self.watermark.as_ref()
    }
}
#[derive(Debug, Clone)]
pub struct ContinuousMapping {
    pub(super) input: InputId,
    pub(super) generation: u64,
    pub(super) epoch: u64,
    pub(super) track: u32,
    pub(super) source: MediaTime,
    pub(super) presentation: MediaTime,
    pub(super) output: u64,
    pub(super) output_start: MediaTime,
    pub(super) configuration: [u8; 32],
    pub(super) pdt: Option<String>,
}
impl ContinuousMapping {
    pub fn output_start(&self) -> MediaTime {
        self.output_start
    }
    pub fn configuration_id(&self) -> &[u8; 32] {
        &self.configuration
    }
    pub fn program_date_time(&self) -> Option<&str> {
        self.pdt.as_deref()
    }
    pub fn map_time(&self, source: MediaTime) -> ContinuousResult<MediaTime> {
        add(sub(source, self.source)?, self.presentation)
    }
    pub fn input_id(&self) -> &InputId {
        &self.input
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn track_id(&self) -> u32 {
        self.track
    }
    pub fn source_start(&self) -> MediaTime {
        self.source
    }
    pub fn presentation_start(&self) -> MediaTime {
        self.presentation
    }
    pub fn output_index(&self) -> u64 {
        self.output
    }
}
#[derive(Debug, Clone)]
pub struct ContinuousOutputReport {
    pub(super) index: u64,
    pub(super) media: TransmuxReport,
    pub(super) collected_bytes: u64,
    pub(super) classic_index_samples: u64,
}
impl ContinuousOutputReport {
    /// Bytes retained by the memory collector before optional classic conversion.
    pub fn collected_bytes(&self) -> u64 {
        self.collected_bytes
    }
    /// Number of sample index entries built by classic finalization.
    pub fn classic_index_samples(&self) -> u64 {
        self.classic_index_samples
    }
    pub fn index(&self) -> u64 {
        self.index
    }
    pub fn media(&self) -> &TransmuxReport {
        &self.media
    }
}
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ContinuousEvent {
    State(ContinuousState),
    Mapping(ContinuousMapping),
    Gap {
        slot: SegmentSlot,
        duration: MediaTime,
        presentation_start: MediaTime,
    },
    Committed {
        input: ContinuousInputProgress,
        bytes: u64,
    },
    Output(ContinuousOutputReport),
}
#[derive(Debug, Clone, Default)]
pub struct ContinuousPeaks {
    pub(super) queued: usize,
    pub(super) metadata: usize,
    pub(super) samples: usize,
    pub(super) sample_bytes: usize,
}
impl ContinuousPeaks {
    pub fn queued_descriptors(&self) -> usize {
        self.queued
    }
    pub fn queued_metadata_bytes(&self) -> usize {
        self.metadata
    }
    pub fn samples(&self) -> usize {
        self.samples
    }
    pub fn sample_bytes(&self) -> usize {
        self.sample_bytes
    }
}
#[derive(Debug, Clone)]
pub struct ContinuousReport {
    pub(super) reason: ContinuousEndReason,
    pub(super) inputs: Vec<ContinuousInputProgress>,
    pub(super) bytes: u64,
    pub(super) duration: MediaTime,
    pub(super) requested: Option<PresentationRange>,
    pub(super) actual: Option<PresentationRange>,
    pub(super) gaps: u64,
    pub(super) outputs: Vec<ContinuousOutputReport>,
    pub(super) mappings: Vec<ContinuousMapping>,
    pub(super) truncated: bool,
    pub(super) peaks: ContinuousPeaks,
}
impl ContinuousReport {
    pub fn requested_range(&self) -> Option<PresentationRange> {
        self.requested
    }
    pub fn actual_range(&self) -> Option<PresentationRange> {
        self.actual
    }
    pub fn end_reason(&self) -> ContinuousEndReason {
        self.reason
    }
    pub fn inputs(&self) -> &[ContinuousInputProgress] {
        &self.inputs
    }
    pub fn bytes_written(&self) -> u64 {
        self.bytes
    }
    pub fn duration(&self) -> MediaTime {
        self.duration
    }
    pub fn gap_count(&self) -> u64 {
        self.gaps
    }
    pub fn outputs(&self) -> &[ContinuousOutputReport] {
        &self.outputs
    }
    pub fn mappings(&self) -> &[ContinuousMapping] {
        &self.mappings
    }
    pub fn history_truncated(&self) -> bool {
        self.truncated
    }
    pub fn peaks(&self) -> &ContinuousPeaks {
        &self.peaks
    }
}
#[derive(Debug, Clone, Copy)]
pub struct SnapshotAcceptance {
    pub(super) accepted: usize,
    pub(super) duplicates: usize,
}
impl SnapshotAcceptance {
    pub fn accepted(&self) -> usize {
        self.accepted
    }
    pub fn duplicates(&self) -> usize {
        self.duplicates
    }
}

use crate::state_codec::{state_enum, state_struct};
state_enum!(ContinuousEndReason { 0 => Eof, 1 => Stop, 2 => DurationLimit });
state_struct!(ContinuousInputProgress {
    input,
    discovered,
    accepted,
    downloaded,
    decrypted,
    committed,
    watermark
});
state_struct!(ContinuousOutputReport {
    index,
    collected_bytes,
    classic_index_samples,
    media
});
state_struct!(ContinuousMapping {
    input,
    generation,
    epoch,
    track,
    source,
    presentation,
    output,
    output_start,
    configuration,
    pdt
});
state_struct!(ContinuousPeaks {
    queued,
    metadata,
    samples,
    sample_bytes
});
