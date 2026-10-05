use super::*;

pub type TimelineResult<T> = std::result::Result<T, TimelineSessionError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TimelineErrorKind {
    InvalidOptions,
    UnsupportedPlaylist,
    InvalidRange,
    OutOfBounds,
    EmptyRange,
    NoRandomAccessPoint,
    TimelineAmbiguous,
    TimeOverflow,
    MissingTailDuration,
    ConfigurationChanged,
    UnrepresentableGap,
    SplitRequiresProvider,
    ResourceChanged,
    PlanningBudgetExceeded,
    Resource,
    Media,
    Output,
    Cancelled,
}
/// Default formatting never formats an underlying transport/provider error.
pub struct TimelineSessionError {
    pub(super) kind: TimelineErrorKind,
    pub(super) slot: Option<SegmentSlot>,
    pub(super) cause: Option<Error>,
    pub(super) resource: Option<Box<ResourceError>>,
    pub(super) completed: Vec<TimelineOutputReport>,
}
impl TimelineSessionError {
    /// Construct a provider acquisition failure without exposing its cause in Debug.
    pub fn output(error: impl Into<Error>) -> Self {
        let mut result = fail(TimelineErrorKind::Output);
        result.cause = Some(error.into());
        result
    }
    pub fn kind(&self) -> TimelineErrorKind {
        self.kind
    }
    pub fn slot(&self) -> Option<&SegmentSlot> {
        self.slot.as_ref()
    }
    pub fn raw_cause(&self) -> Option<&Error> {
        self.cause.as_ref()
    }
    pub fn resource_error(&self) -> Option<&ResourceError> {
        self.resource.as_deref()
    }
    pub fn completed_outputs(&self) -> &[TimelineOutputReport] {
        &self.completed
    }
}
impl std::fmt::Debug for TimelineSessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TimelineSessionError")
            .field("kind", &self.kind)
            .field("slot", &self.slot)
            .field("completed_outputs", &self.completed.len())
            .finish_non_exhaustive()
    }
}
impl std::fmt::Display for TimelineSessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "timeline {:?}", self.kind)
    }
}
impl std::error::Error for TimelineSessionError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationRange {
    pub(super) start: MediaTime,
    pub(super) end: MediaTime,
}
impl PresentationRange {
    pub fn new(start: MediaTime, end: MediaTime) -> TimelineResult<Self> {
        if start.ticks < 0 || cmp(start, end)? != Ordering::Less {
            return Err(fail(TimelineErrorKind::InvalidRange));
        }
        Ok(Self { start, end })
    }
    pub fn start(&self) -> MediaTime {
        self.start
    }
    pub fn end(&self) -> MediaTime {
        self.end
    }
    pub(super) fn intersects(&self, other: &Self) -> TimelineResult<bool> {
        Ok(cmp(self.start, other.end)? == Ordering::Less
            && cmp(other.start, self.end)? == Ordering::Less)
    }
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum GapPolicy {
    #[default]
    Preserve,
    Collapse,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum TimelineChangePolicy {
    #[default]
    Fail,
    Split,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum TailDurationPolicy {
    #[default]
    RequireEvidence,
    Explicit(MediaTime),
}
#[derive(Debug, Clone)]
pub struct EpochAnchor {
    pub(super) input: InputId,
    pub(super) epoch: u64,
    pub(super) source: MediaTime,
    pub(super) public: MediaTime,
}
impl EpochAnchor {
    pub fn new(input: InputId, epoch: u64, source: MediaTime, public: MediaTime) -> Self {
        Self {
            input,
            epoch,
            source,
            public,
        }
    }
    pub fn input_id(&self) -> &InputId {
        &self.input
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn source(&self) -> MediaTime {
        self.source
    }
    pub fn presentation(&self) -> MediaTime {
        self.public
    }
}

/// Admission limits for live sample cursors and loaded resources.
/// Finite resource catalogs, snapshots, mappings and output indexes are separate.
/// Exceeding a limit fails before acquiring a writer or publishing an output.
#[derive(Debug, Clone, Copy)]
pub struct TimelinePlanningLimits {
    samples: usize,
    resources: usize,
}
impl Default for TimelinePlanningLimits {
    fn default() -> Self {
        Self {
            samples: 65_536,
            resources: 4_096,
        }
    }
}
impl TimelinePlanningLimits {
    pub fn new(samples: usize, resources: usize) -> TimelineResult<Self> {
        if samples == 0 || resources == 0 {
            return Err(fail(TimelineErrorKind::InvalidOptions));
        }
        Ok(Self { samples, resources })
    }
    pub fn samples(&self) -> usize {
        self.samples
    }
    pub fn resources(&self) -> usize {
        self.resources
    }
    pub(super) fn admit(&self, samples: usize, resources: usize) -> TimelineResult<()> {
        if samples > self.samples || resources > self.resources {
            Err(fail(TimelineErrorKind::PlanningBudgetExceeded))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone)]
pub struct TimelinePrepareOptions {
    pub(super) range: Option<PresentationRange>,
    pub(super) gaps: GapPolicy,
    pub(super) changes: TimelineChangePolicy,
    pub(super) tail: TailDurationPolicy,
    pub(super) anchors: Vec<EpochAnchor>,
    pub(super) resources: ResourceOptions,
    pub(super) planning: TimelinePlanningLimits,
    pub(super) cancel: Option<Arc<dyn CancelToken>>,
    pub(super) on_event: Option<Arc<TimelineEventCallback>>,
}
impl Default for TimelinePrepareOptions {
    fn default() -> Self {
        Self {
            range: None,
            gaps: GapPolicy::Preserve,
            changes: TimelineChangePolicy::Fail,
            tail: TailDurationPolicy::RequireEvidence,
            anchors: Vec::new(),
            resources: ResourceOptions::default(),
            planning: TimelinePlanningLimits::default(),
            cancel: None,
            on_event: None,
        }
    }
}
impl TimelinePrepareOptions {
    pub fn range(&self) -> Option<PresentationRange> {
        self.range
    }
    pub fn gap_policy(&self) -> GapPolicy {
        self.gaps
    }
    pub fn change_policy(&self) -> TimelineChangePolicy {
        self.changes
    }
    pub fn tail_duration_policy(&self) -> TailDurationPolicy {
        self.tail
    }
    pub fn epoch_anchors(&self) -> &[EpochAnchor] {
        &self.anchors
    }
    pub fn resource_options(&self) -> &ResourceOptions {
        &self.resources
    }
    pub fn planning_limits(&self) -> TimelinePlanningLimits {
        self.planning
    }
    pub fn with_planning_limits(mut self, limits: TimelinePlanningLimits) -> Self {
        self.planning = limits;
        self
    }
    pub fn with_range(mut self, range: PresentationRange) -> Self {
        self.range = Some(range);
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
    pub fn with_tail_duration(mut self, policy: TailDurationPolicy) -> Self {
        self.tail = policy;
        self
    }
    pub fn with_epoch_anchor(mut self, anchor: EpochAnchor) -> Self {
        self.anchors.push(anchor);
        self
    }
    pub fn with_resources(mut self, resources: ResourceOptions) -> Self {
        self.resources = resources;
        self
    }
    pub fn with_cancel(mut self, token: Arc<dyn CancelToken>) -> Self {
        self.cancel = Some(token);
        self
    }
    pub fn with_on_event(mut self, callback: Arc<TimelineEventCallback>) -> Self {
        self.on_event = Some(callback);
        self
    }
    pub(super) fn check(&self) -> TimelineResult<()> {
        check_cancel(self.cancel.as_ref()).map_err(media_error)?;
        self.resources.validate().map_err(resource_error)?;
        if let TailDurationPolicy::Explicit(time) = self.tail
            && time.ticks <= 0
        {
            return Err(fail(TimelineErrorKind::InvalidOptions));
        }
        for (i, anchor) in self.anchors.iter().enumerate() {
            if anchor.public.ticks < 0
                || self.anchors[..i]
                    .iter()
                    .any(|a| a.input == anchor.input && a.epoch == anchor.epoch)
            {
                return Err(fail(TimelineErrorKind::InvalidOptions));
            }
        }
        Ok(())
    }
    pub(super) fn emit(&self, event: TimelineSessionEvent) -> TimelineResult<()> {
        check_cancel(self.cancel.as_ref()).map_err(media_error)?;
        if let Some(callback) = &self.on_event {
            callback(event);
        }
        check_cancel(self.cancel.as_ref()).map_err(media_error)
    }
}
#[cfg(not(target_arch = "wasm32"))]
pub type TimelineEventCallback = dyn Fn(TimelineSessionEvent) + Send + Sync;
#[cfg(target_arch = "wasm32")]
pub type TimelineEventCallback = dyn Fn(TimelineSessionEvent);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TimelineEventKind {
    MappingCommitted,
    OutputCompleted,
    Completed,
}
#[derive(Debug, Clone)]
pub struct TimelineSessionEvent {
    pub(super) kind: TimelineEventKind,
    pub(super) output: usize,
    pub(super) mappings: Vec<EpochMapping>,
}
impl TimelineSessionEvent {
    pub fn kind(&self) -> TimelineEventKind {
        self.kind
    }
    pub fn output_index(&self) -> usize {
        self.output
    }
    pub fn mappings(&self) -> &[EpochMapping] {
        &self.mappings
    }
}
/// One immutable interval. Source timestamps retain their own timescale and CTS.
#[derive(Debug, Clone)]
pub struct EpochMapping {
    pub(super) input: InputId,
    pub(super) track: u32,
    pub(super) epoch: u64,
    pub(super) source_origin: MediaTime,
    pub(super) source_decode_start: MediaTime,
    pub(super) config_id: [u8; 32],
    pub(super) public: PresentationRange,
    pub(super) output_start: MediaTime,
    pub(super) output: usize,
    pub(super) wrap_anchor: Option<MediaTime>,
    pub(super) pdt: Option<String>,
}
impl EpochMapping {
    pub fn input_id(&self) -> &InputId {
        &self.input
    }
    pub fn track_id(&self) -> u32 {
        self.track
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn source_origin(&self) -> MediaTime {
        self.source_origin
    }
    pub fn source_decode_start(&self) -> MediaTime {
        self.source_decode_start
    }
    pub fn configuration_id(&self) -> &[u8; 32] {
        &self.config_id
    }
    pub fn presentation_range(&self) -> PresentationRange {
        self.public
    }
    pub fn output_start(&self) -> MediaTime {
        self.output_start
    }
    pub fn output_index(&self) -> usize {
        self.output
    }
    pub fn wrap_anchor(&self) -> Option<MediaTime> {
        self.wrap_anchor
    }
    pub fn program_date_time(&self) -> Option<&str> {
        self.pdt.as_deref()
    }
    pub fn source_to_presentation(&self, time: MediaTime) -> TimelineResult<MediaTime> {
        add(sub(time, self.source_origin)?, self.public.start)
    }
    pub fn presentation_to_output(&self, time: MediaTime) -> TimelineResult<MediaTime> {
        add(sub(time, self.public.start)?, self.output_start)
    }
}
#[derive(Debug, Clone)]
pub struct MappedInterval {
    pub(super) output: usize,
    pub(super) track: u32,
    pub(super) epoch: u64,
    pub(super) source: PresentationRange,
    pub(super) output_range: PresentationRange,
}
impl MappedInterval {
    pub fn output_index(&self) -> usize {
        self.output
    }
    pub fn track_id(&self) -> u32 {
        self.track
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn presentation_range(&self) -> PresentationRange {
        self.source
    }
    pub fn output_range(&self) -> PresentationRange {
        self.output_range
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TimelineSplitReason {
    Initial,
    ConfigurationChanged,
    Gap,
}
#[derive(Debug, Clone)]
pub struct TimelineOutputReport {
    pub(super) index: usize,
    pub(super) range: PresentationRange,
    pub(super) reason: TimelineSplitReason,
    pub(super) media: TransmuxReport,
    pub(super) mappings: Vec<EpochMapping>,
}
impl TimelineOutputReport {
    pub fn index(&self) -> usize {
        self.index
    }
    pub fn actual_range(&self) -> PresentationRange {
        self.range
    }
    pub fn reason(&self) -> TimelineSplitReason {
        self.reason
    }
    pub fn media(&self) -> &TransmuxReport {
        &self.media
    }
    pub fn mappings(&self) -> &[EpochMapping] {
        &self.mappings
    }
}
#[derive(Debug, Clone)]
pub struct TimelineAccessPoint {
    pub(super) slot: SegmentSlot,
    pub(super) sample: usize,
    pub(super) source: MediaTime,
    pub(super) presentation: MediaTime,
    pub(super) output: usize,
}
impl TimelineAccessPoint {
    pub fn slot(&self) -> &SegmentSlot {
        &self.slot
    }
    pub fn sample_index(&self) -> usize {
        self.sample
    }
    pub fn source_time(&self) -> MediaTime {
        self.source
    }
    pub fn presentation_time(&self) -> MediaTime {
        self.presentation
    }
    pub fn output_index(&self) -> usize {
        self.output
    }
}
/// Immutable resource dependencies; credentials and raw keys are deliberately absent.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct TimelineDependency {
    pub(super) slot: SegmentSlot,
    pub(super) map: Option<crate::playlist::DeclarationId>,
    pub(super) keys: Vec<crate::playlist::DeclarationId>,
    pub(super) map_keys: Vec<crate::playlist::DeclarationId>,
}
impl TimelineDependency {
    pub fn slot(&self) -> &SegmentSlot {
        &self.slot
    }
    pub fn map_declaration(&self) -> Option<crate::playlist::DeclarationId> {
        self.map
    }
    pub fn key_declarations(&self) -> &[crate::playlist::DeclarationId] {
        &self.keys
    }
    pub fn map_key_declarations(&self) -> &[crate::playlist::DeclarationId] {
        &self.map_keys
    }
}
#[derive(Debug, Clone)]
pub struct TimelineSessionReport {
    pub(super) requested: Option<PresentationRange>,
    pub(super) actual: PresentationRange,
    pub(super) outputs: Vec<TimelineOutputReport>,
    pub(super) gaps: Vec<PresentationRange>,
    pub(super) dependencies: Vec<TimelineDependency>,
    pub(super) access_points: Vec<TimelineAccessPoint>,
    pub(super) resource_reads: u64,
    pub(super) source_bytes: u64,
    pub(super) peak_planned_samples: usize,
    pub(super) peak_planned_resources: usize,
    pub(super) indexed_resources: usize,
}
impl TimelineSessionReport {
    pub fn requested_range(&self) -> Option<PresentationRange> {
        self.requested
    }
    pub fn actual_range(&self) -> PresentationRange {
        self.actual
    }
    pub fn outputs(&self) -> &[TimelineOutputReport] {
        &self.outputs
    }
    pub fn gaps(&self) -> &[PresentationRange] {
        &self.gaps
    }
    pub fn random_access_points(&self) -> &[TimelineAccessPoint] {
        &self.access_points
    }
    pub fn dependencies(&self) -> &[TimelineDependency] {
        &self.dependencies
    }
    /// Presentation samples included before the requested start for decoding.
    pub fn preroll(&self) -> TimelineResult<Option<PresentationRange>> {
        match self.requested {
            Some(range) if cmp(self.actual.start, range.start)? == Ordering::Less => {
                Ok(Some(PresentationRange {
                    start: self.actual.start,
                    end: range.start,
                }))
            }
            _ => Ok(None),
        }
    }
    /// Tail samples retained after the requested end for decoding.
    pub fn postroll(&self) -> TimelineResult<Option<PresentationRange>> {
        match self.requested {
            Some(range) if cmp(self.actual.end, range.end)? == Ordering::Greater => {
                Ok(Some(PresentationRange {
                    start: range.end,
                    end: self.actual.end,
                }))
            }
            _ => Ok(None),
        }
    }
    pub fn resource_reads(&self) -> u64 {
        self.resource_reads
    }
    pub fn source_bytes(&self) -> u64 {
        self.source_bytes
    }
    /// High-water mark of retained sample records during selection, not RSS.
    pub fn peak_planned_samples(&self) -> usize {
        self.peak_planned_samples
    }
    /// High-water mark of resources in live sample windows/caches, excluding the catalog.
    pub fn peak_planned_resources(&self) -> usize {
        self.peak_planned_resources
    }
    /// Resource/clock summaries retained by finite selection (separate from live cursors).
    pub fn indexed_resources(&self) -> usize {
        self.indexed_resources
    }
    /// Split a public interval at committed media/epoch/output boundaries.
    pub fn map_interval(
        &self,
        input: &InputId,
        track: u32,
        range: PresentationRange,
    ) -> TimelineResult<Vec<MappedInterval>> {
        let mut result = Vec::new();
        for mapping in self
            .outputs
            .iter()
            .flat_map(|o| &o.mappings)
            .filter(|m| &m.input == input && m.track == track)
        {
            if !mapping.public.intersects(&range)? {
                continue;
            }
            let start = if cmp(range.start, mapping.public.start)? == Ordering::Greater {
                range.start
            } else {
                mapping.public.start
            };
            let end = if cmp(range.end, mapping.public.end)? == Ordering::Less {
                range.end
            } else {
                mapping.public.end
            };
            result.push(MappedInterval {
                output: mapping.output,
                track,
                epoch: mapping.epoch,
                source: PresentationRange { start, end },
                output_range: PresentationRange {
                    start: mapping.presentation_to_output(start)?,
                    end: mapping.presentation_to_output(end)?,
                },
            });
        }
        Ok(result)
    }
}
