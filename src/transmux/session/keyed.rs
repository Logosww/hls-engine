//! Typed finite-playlist adapter for the shared prepared media/output core.
#![doc = include_str!("../../../docs/keyed-sessions.md")]
use super::*;
use crate::capabilities::*;
use crate::crypto::{
    key::{KeyErrorKind, KeyResource, KeySession},
    resource::*,
};
use crate::playlist::{InputId, PlaylistRejection, PlaylistSnapshot, SegmentSlot};

mod progress;
pub use progress::{KeyedInputProgress, KeyedResourceProgress};
use progress::{ProgressState, SharedProgress};

/// A selected immutable media snapshot and the source used for its resources.
pub struct KeyedInput {
    snapshot: PlaylistSnapshot,
    source: Arc<dyn Source>,
}
impl KeyedInput {
    pub fn new(snapshot: PlaylistSnapshot, source: Arc<dyn Source>) -> Self {
        Self { snapshot, source }
    }
    pub fn snapshot(&self) -> &PlaylistSnapshot {
        &self.snapshot
    }
}
pub struct KeyedInputs {
    primary: KeyedInput,
    audio: Option<KeyedInput>,
}
impl KeyedInputs {
    pub fn new(primary: KeyedInput) -> Self {
        Self {
            primary,
            audio: None,
        }
    }
    /// External audio replaces embedded primary audio, as in `prepare_hls`.
    pub fn with_audio(mut self, audio: KeyedInput) -> Self {
        self.audio = Some(audio);
        self
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeyedSessionPhase {
    Preparing,
    Playlist,
    Initialization,
    Downloading,
    Decrypting,
    Validating,
    Processing,
    Writing,
    Finalizing,
    Completed,
}
impl From<SessionPhase> for KeyedSessionPhase {
    fn from(value: SessionPhase) -> Self {
        match value {
            SessionPhase::Preparing => Self::Preparing,
            SessionPhase::Playlist => Self::Playlist,
            SessionPhase::Initialization => Self::Initialization,
            SessionPhase::Downloading => Self::Downloading,
            SessionPhase::Processing => Self::Processing,
            SessionPhase::Writing => Self::Writing,
            SessionPhase::Finalizing => Self::Finalizing,
            SessionPhase::Completed => Self::Completed,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeyedSessionErrorKind {
    InvalidInputs,
    UnsupportedPlaylist,
    Resource,
    Media,
    Output,
    Cancelled,
}
/// Stable diagnostic categories; raw transport/provider/mux messages remain explicit access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeyedFailure {
    InvalidInputs,
    UnsupportedCombination,
    ProviderUnavailable,
    ProviderFailure,
    InvalidKey,
    InvalidIv,
    InvalidEncryptionMetadata,
    KeyExpired,
    Decrypt,
    MediaValidation,
    Read,
    BudgetExceeded,
    Output,
    Cancelled,
}
impl std::fmt::Display for KeyedFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "keyed failure {self:?}")
    }
}
impl std::error::Error for KeyedFailure {}
fn resource_failure(error: &ResourceError) -> KeyedFailure {
    use KeyedFailure as F;
    if let Some(key) = error.key_error() {
        return match key.kind() {
            KeyErrorKind::Cancelled => F::Cancelled,
            KeyErrorKind::BudgetExceeded => F::BudgetExceeded,
            KeyErrorKind::Unsupported => F::UnsupportedCombination,
            KeyErrorKind::Unavailable => F::ProviderUnavailable,
            KeyErrorKind::Provider => F::ProviderFailure,
            KeyErrorKind::InvalidKey => F::InvalidKey,
            KeyErrorKind::ConflictingMetadata => F::InvalidEncryptionMetadata,
            KeyErrorKind::Expired => F::KeyExpired,
            KeyErrorKind::InvalidOptions | KeyErrorKind::RevisionOverflow => F::InvalidInputs,
        };
    }
    match error.kind() {
        ResourceErrorKind::Cancelled => F::Cancelled,
        ResourceErrorKind::UnsupportedPlaylist | ResourceErrorKind::UnconfirmedRange => {
            F::UnsupportedCombination
        }
        ResourceErrorKind::InvalidIv => F::InvalidIv,
        ResourceErrorKind::InvalidRange | ResourceErrorKind::MissingMap => {
            F::InvalidEncryptionMetadata
        }
        ResourceErrorKind::ResourceTooLarge
        | ResourceErrorKind::BudgetExceeded
        | ResourceErrorKind::CounterOverflow => F::BudgetExceeded,
        ResourceErrorKind::Read => F::Read,
        ResourceErrorKind::InvalidCiphertextLength | ResourceErrorKind::Decrypt => F::Decrypt,
        ResourceErrorKind::MediaValidation => {
            if matches!(error.raw_cause(), Some(Error::Unsupported(_))) {
                F::UnsupportedCombination
            } else {
                F::MediaValidation
            }
        }
        ResourceErrorKind::Key => F::InvalidKey,
        ResourceErrorKind::InvalidOptions | ResourceErrorKind::InvalidIndex => F::InvalidInputs,
    }
}
/// Safe default formatting. Inspect raw causes explicitly when needed.
pub struct KeyedSessionError {
    kind: KeyedSessionErrorKind,
    failure: KeyedFailure,
    context: Option<Box<KeyResource>>,
    phase: KeyedSessionPhase,
    input_id: Option<InputId>,
    slot: Option<SegmentSlot>,
    rejection: Option<PlaylistRejection>,
    cause: Option<Box<SessionError>>,
    resource: Option<Box<ResourceError>>,
}
impl KeyedSessionError {
    fn new(kind: KeyedSessionErrorKind) -> Self {
        Self {
            kind,
            failure: match kind {
                KeyedSessionErrorKind::InvalidInputs => KeyedFailure::InvalidInputs,
                KeyedSessionErrorKind::UnsupportedPlaylist => KeyedFailure::UnsupportedCombination,
                KeyedSessionErrorKind::Resource => KeyedFailure::Read,
                KeyedSessionErrorKind::Media => KeyedFailure::MediaValidation,
                KeyedSessionErrorKind::Output => KeyedFailure::Output,
                KeyedSessionErrorKind::Cancelled => KeyedFailure::Cancelled,
            },
            context: None,
            phase: KeyedSessionPhase::Preparing,
            input_id: None,
            slot: None,
            rejection: None,
            cause: None,
            resource: None,
        }
    }
    fn from_core(mut cause: SessionError, snapshots: &[PlaylistSnapshot]) -> Self {
        let snapshot = match cause.role {
            Some(InputRole::Primary) => snapshots.first(),
            Some(InputRole::Audio) => snapshots.get(1),
            None => None,
        };
        let resource = cause.keyed_cause.take();
        let kind = if matches!(cause.error, Error::Cancelled)
            || resource
                .as_ref()
                .is_some_and(|r| r.kind() == ResourceErrorKind::Cancelled)
        {
            KeyedSessionErrorKind::Cancelled
        } else if resource.is_some() {
            KeyedSessionErrorKind::Resource
        } else if matches!(
            cause.phase,
            SessionPhase::Writing | SessionPhase::Finalizing
        ) {
            KeyedSessionErrorKind::Output
        } else {
            KeyedSessionErrorKind::Media
        };
        let context = resource
            .as_ref()
            .and_then(|e| e.resource())
            .cloned()
            .or_else(|| {
                snapshot
                    .and_then(|s| cause.segment_index.and_then(|i| s.segments().get(i)))
                    .map(|s| {
                        if cause.phase == SessionPhase::Initialization {
                            KeyResource::map(s).unwrap_or_else(|| KeyResource::media(s))
                        } else {
                            KeyResource::media(s)
                        }
                    })
            })
            .map(Box::new);
        let failure = resource
            .as_ref()
            .map(|e| resource_failure(e))
            .unwrap_or(match cause.error {
                Error::Cancelled => KeyedFailure::Cancelled,
                _ if kind == KeyedSessionErrorKind::Output => KeyedFailure::Output,
                Error::Unsupported(_) => KeyedFailure::UnsupportedCombination,
                Error::Io(_) | Error::Http(_) => KeyedFailure::Read,
                _ => KeyedFailure::MediaValidation,
            });
        Self {
            kind,
            failure,
            context,
            phase: cause.phase.into(),
            input_id: snapshot.map(|s| s.context().input_id().clone()),
            slot: snapshot
                .and_then(|s| cause.segment_index.and_then(|i| s.segments().get(i)))
                .map(|s| s.slot().clone()),
            rejection: None,
            cause: Some(Box::new(cause)),
            resource,
        }
    }
    pub fn failure(&self) -> KeyedFailure {
        self.failure
    }
    /// Resource slot/kind/range and immutable encryption candidates. Selected reference, when known, is in resource_error().key_error().
    pub fn resource_context(&self) -> Option<&KeyResource> {
        self.context.as_deref()
    }
    /// The finite resource API has no reliable failing sample/track attribution.
    pub fn track_id(&self) -> Option<u32> {
        None
    }
    pub fn sample_index(&self) -> Option<usize> {
        None
    }
    pub fn kind(&self) -> KeyedSessionErrorKind {
        self.kind
    }
    pub fn phase(&self) -> KeyedSessionPhase {
        self.phase
    }
    pub fn input_id(&self) -> Option<&InputId> {
        self.input_id.as_ref()
    }
    pub fn slot(&self) -> Option<&SegmentSlot> {
        self.slot.as_ref()
    }
    pub fn playlist_rejection(&self) -> Option<PlaylistRejection> {
        self.rejection
    }
    pub fn resource_error(&self) -> Option<&ResourceError> {
        self.resource.as_deref()
    }
    pub fn raw_cause(&self) -> Option<&SessionError> {
        self.cause.as_deref()
    }
}
impl std::fmt::Debug for KeyedSessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyedSessionError")
            .field("kind", &self.kind)
            .field("failure", &self.failure)
            .field("context", &self.context)
            .field("phase", &self.phase)
            .field("input_id", &self.input_id)
            .field("slot", &self.slot)
            .field("rejection", &self.rejection)
            .field("resource", &self.resource)
            .finish_non_exhaustive()
    }
}
impl std::fmt::Display for KeyedSessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "keyed session {:?}: {:?}", self.phase, self.failure)
    }
}
impl std::error::Error for KeyedSessionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(
            self.resource
                .as_deref()
                .map_or(&self.failure as &dyn std::error::Error, |e| e as _),
        )
    }
}
pub type KeyedSessionResult<T> = std::result::Result<T, KeyedSessionError>;

#[derive(Debug, Clone)]
pub struct KeyedSessionEvent {
    phase: KeyedSessionPhase,
    slot: Option<SegmentSlot>,
    resource: Option<KeyResource>,
    inputs: Vec<KeyedInputProgress>,
    bytes: u64,
}
impl KeyedSessionEvent {
    pub fn resource_context(&self) -> Option<&KeyResource> {
        self.resource.as_ref()
    }
    pub fn phase(&self) -> KeyedSessionPhase {
        self.phase
    }
    pub fn slot(&self) -> Option<&SegmentSlot> {
        self.slot.as_ref()
    }
    pub fn inputs(&self) -> &[KeyedInputProgress] {
        &self.inputs
    }
    pub fn bytes_written(&self) -> u64 {
        self.bytes
    }
}
#[derive(Debug, Clone)]
pub struct KeyedSessionReport {
    media: TransmuxReport,
    timeline: TimelineMapping,
    inputs: Vec<KeyedInputProgress>,
    capability: KeyedCapabilityQuery,
}
impl KeyedSessionReport {
    /// Observed inputs/codecs and actual output mode; successful preparation is not a validation of future bytes.
    pub fn capability_query(&self) -> &KeyedCapabilityQuery {
        &self.capability
    }
    fn from_core(
        report: SessionReport,
        state: &SharedProgress,
        capability: KeyedCapabilityQuery,
    ) -> Self {
        Self {
            media: report.media,
            timeline: report.timeline,
            inputs: state.lock().unwrap().inputs(),
            capability,
        }
    }
    pub fn media(&self) -> &TransmuxReport {
        &self.media
    }
    pub fn timeline(&self) -> &TimelineMapping {
        &self.timeline
    }
    pub fn inputs(&self) -> &[KeyedInputProgress] {
        &self.inputs
    }
}
/// Native callbacks are thread-safe; WASM callbacks may own JavaScript values.
#[cfg(not(target_arch = "wasm32"))]
pub type KeyedEventCallback = dyn Fn(KeyedSessionEvent) + Send + Sync;
#[cfg(target_arch = "wasm32")]
pub type KeyedEventCallback = dyn Fn(KeyedSessionEvent);
#[derive(Clone, Default)]
pub struct KeyedPrepareOptions {
    core: PrepareOptions,
    resources: ResourceOptions,
    on_event: Option<Arc<KeyedEventCallback>>,
}
impl KeyedPrepareOptions {
    pub fn with_cancel(mut self, cancel: Arc<dyn CancelToken>) -> Self {
        self.core.cancel = Some(cancel);
        self
    }
    pub fn with_on_event(mut self, callback: Arc<KeyedEventCallback>) -> Self {
        self.on_event = Some(callback);
        self
    }
    pub fn with_resources(mut self, resources: ResourceOptions) -> Self {
        self.resources = resources;
        self
    }
    pub fn with_probe_segments(mut self, count: usize) -> Self {
        self.core.budget.probe = count;
        self
    }
    pub fn with_parallel_reads(mut self, count: usize) -> Self {
        self.core.budget.reads = count;
        self
    }
    pub fn with_write_mfra(mut self, value: bool) -> Self {
        self.core.write_mfra = value;
        self
    }
}
/// Owns one key/resource session. Dropping it cancels outstanding key work.
pub struct KeyedPreparedTransmux {
    core: PreparedTransmux,
    snapshots: Arc<Vec<PlaylistSnapshot>>,
    resources: Arc<ResourceSession>,
    progress: SharedProgress,
}
/// Prepare finite selected clear/AES-128 playlists, preserving original sequence IVs.
/// All snapshots are preflighted before resource/provider I/O. No master selection or resume.
// WASM callbacks/providers are local, while the shared session API retains Arc ownership.
#[cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]
pub async fn prepare_hls_with_keys(
    inputs: KeyedInputs,
    keys: KeySession,
    mut options: KeyedPrepareOptions,
) -> KeyedSessionResult<KeyedPreparedTransmux> {
    let mut selected = vec![inputs.primary];
    if let Some(audio) = inputs.audio {
        selected.push(audio);
    }
    if options.core.budget.probe == 0
        || options.core.budget.reads == 0
        || (selected.len() == 2
            && selected[0].snapshot.context().input_id()
                == selected[1].snapshot.context().input_id())
    {
        return Err(KeyedSessionError::new(KeyedSessionErrorKind::InvalidInputs));
    }
    options.resources.validate().map_err(|resource| {
        let mut error = KeyedSessionError::new(KeyedSessionErrorKind::InvalidInputs);
        error.resource = Some(Box::new(resource));
        error
    })?;
    for input in &selected {
        if let Err(rejection) = input.snapshot.validate_finite_vod() {
            let mut error = KeyedSessionError::new(KeyedSessionErrorKind::UnsupportedPlaylist);
            error.input_id = Some(input.snapshot.context().input_id().clone());
            error.rejection = Some(rejection);
            error.context = preflight_context(&input.snapshot, rejection).map(Box::new);
            error.slot = error.context.as_ref().map(|r| r.slot().clone());
            error.failure = match rejection {
                PlaylistRejection::MissingMapIv => KeyedFailure::InvalidIv,
                PlaylistRejection::ConflictingKeyMethods => KeyedFailure::InvalidEncryptionMetadata,
                _ => KeyedFailure::UnsupportedCombination,
            };
            return Err(error);
        }
        if input
            .snapshot
            .segments()
            .iter()
            .any(|s| s.map().is_some() != input.snapshot.segments()[0].map().is_some())
        {
            let mut error = KeyedSessionError::new(KeyedSessionErrorKind::UnsupportedPlaylist);
            error.input_id = Some(input.snapshot.context().input_id().clone());
            return Err(error);
        }
    }
    // All manifest-known resource restrictions must fail before either input reads.
    for input in &selected {
        for (index, segment) in input.snapshot.segments().iter().enumerate() {
            for map in [true, false] {
                if map && segment.map().is_none() {
                    continue;
                }
                let request = ResourceRequest::from_validated(&input.snapshot, index, map)
                    .expect("validated selected resource");
                if let Err(resource) = options.resources.preflight(&keys, &request) {
                    let mut error = KeyedSessionError::new(KeyedSessionErrorKind::Resource);
                    error.failure = resource_failure(&resource);
                    error.input_id = Some(input.snapshot.context().input_id().clone());
                    error.slot = Some(segment.slot().clone());
                    error.context = Some(Box::new(request.resource().clone()));
                    error.resource = Some(Box::new(resource));
                    return Err(error);
                }
            }
        }
    }
    let snapshots = Arc::new(
        selected
            .iter()
            .map(|i| i.snapshot.clone())
            .collect::<Vec<_>>(),
    );
    // Unknown lengths reserve the full resource cap. Serialize when two such reads cannot fit.
    let capacity = options
        .resources
        .max_waiting_bytes()
        .checked_div(options.resources.max_resource_bytes())
        .unwrap_or(0);
    options.core.budget.reads = options
        .core
        .budget
        .reads
        .min(options.resources.max_resources())
        .min(usize::try_from(capacity).unwrap_or(usize::MAX));
    let progress = ProgressState::new(&snapshots);
    let observer_state = progress.clone();
    let callback = options.on_event.clone();
    let cancellation = options.core.cancel.clone();
    let resources = Arc::new(
        ResourceSession::new(keys, options.resources)
            .map_err(|resource| {
                let mut error = KeyedSessionError::new(KeyedSessionErrorKind::InvalidInputs);
                error.failure = resource_failure(&resource);
                error.resource = Some(Box::new(resource));
                error
            })?
            .with_observer(Arc::new(move |observation| {
                if cancellation.as_ref().is_some_and(|c| c.is_cancelled()) {
                    return Err(ResourceError::new(ResourceErrorKind::Cancelled));
                }
                let event = {
                    let mut state = observer_state.lock().unwrap();
                    state.observe(&observation)?;
                    state.event(
                        match observation.stage {
                            ResourceStage::Downloaded => KeyedSessionPhase::Downloading,
                            ResourceStage::Decrypted => KeyedSessionPhase::Decrypting,
                            ResourceStage::Ready => KeyedSessionPhase::Validating,
                            ResourceStage::MapReused => KeyedSessionPhase::Initialization,
                        },
                        Some(observation.resource.clone()),
                    )
                };
                // User callbacks run outside all resource/progress/key locks.
                if let Some(callback) = &callback {
                    callback(event);
                }
                if cancellation.as_ref().is_some_and(|c| c.is_cancelled()) {
                    return Err(ResourceError::new(ResourceErrorKind::Cancelled));
                }
                Ok(())
            })),
    );
    let event_snapshots = snapshots.clone();
    let event_progress = progress.clone();
    let callback = options.on_event;
    options.core.on_event = Some(Arc::new(move |event| {
        let input = match event.role {
            Some(InputRole::Primary) => event_snapshots.first(),
            Some(InputRole::Audio) => event_snapshots.get(1),
            None => None,
        };
        let resource = input
            .and_then(|s| event.index.and_then(|i| s.segments().get(i)))
            .map(KeyResource::media);
        let event = {
            let mut state = event_progress.lock().unwrap();
            state.update_core(&event);
            state.event(event.phase.into(), resource)
        };
        if let Some(callback) = &callback {
            callback(event);
        }
    }));
    let external = selected.len() == 2;
    let cursors = selected
        .into_iter()
        .enumerate()
        .map(|(index, input)| {
            let role = if index == 0 {
                InputRole::Primary
            } else {
                InputRole::Audio
            };
            let total = input.snapshot.segments().len();
            InputCursor {
                role,
                resources: InputResources::Keyed(Box::new(KeyedCursor {
                    snapshot: input.snapshot,
                    source: input.source,
                    resources: resources.clone(),
                    map: None,
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
            }
        })
        .collect();
    let core = prepare_cursors(cursors, options.core)
        .await
        .map_err(|e| KeyedSessionError::from_core(e, &snapshots))?;
    Ok(KeyedPreparedTransmux {
        core,
        snapshots,
        resources,
        progress,
    })
}
impl KeyedPreparedTransmux {
    pub fn progress(&self) -> Vec<KeyedInputProgress> {
        self.progress.lock().unwrap().inputs()
    }
    pub fn capability_query(&self, output: KeyedOutput) -> KeyedCapabilityQuery {
        observed_capability(&self.core.info, &self.snapshots, output)
    }
    pub fn info(&self) -> &PreparedInfo {
        self.core.info()
    }
    /// Input order matches timeline roles: primary, then optional replacement audio.
    pub fn snapshots(&self) -> &[PlaylistSnapshot] {
        &self.snapshots
    }
    pub fn invalidate_keys(&self) -> ResourceResult<()> {
        self.resources.invalidate_keys()
    }
    pub async fn into_mp4_bytes(self) -> KeyedSessionResult<(Vec<u8>, KeyedSessionReport)> {
        let capability = self.capability_query(KeyedOutput::Mp4Bytes);
        let (bytes, report) = self
            .core
            .into_mp4_bytes()
            .await
            .map_err(|e| KeyedSessionError::from_core(e, &self.snapshots))?;
        Ok((
            bytes,
            KeyedSessionReport::from_core(report, &self.progress, capability),
        ))
    }
    /// Flushes the caller-owned writer without closing it; supports non-Send writers.
    pub async fn write_to<W: AsyncWrite + Unpin>(
        self,
        writer: &mut W,
    ) -> KeyedSessionResult<KeyedSessionReport> {
        let capability = self.capability_query(KeyedOutput::FragmentedWriter);
        let report = self
            .core
            .write_to(writer)
            .await
            .map_err(|e| KeyedSessionError::from_core(e, &self.snapshots))?;
        Ok(KeyedSessionReport::from_core(
            report,
            &self.progress,
            capability,
        ))
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn write_to_file(
        self,
        path: impl AsRef<Path>,
        output: FileOutputOptions,
    ) -> KeyedSessionResult<KeyedSessionReport> {
        let mode = match output.format {
            OutputFormat::Mp4 => KeyedOutput::Mp4File,
            OutputFormat::FragmentedMp4 => KeyedOutput::FragmentedFile,
            OutputFormat::StreamingMp4 => match output.backend {
                FinalizeBackend::Native => KeyedOutput::NativeStreamingFile,
                #[cfg(feature = "ffmpeg-finalize")]
                FinalizeBackend::Ffmpeg => KeyedOutput::FfmpegStreamingFile,
            },
        };
        let capability = self.capability_query(mode);
        let report = self
            .core
            .write_to_file(path, output)
            .await
            .map_err(|e| KeyedSessionError::from_core(e, &self.snapshots))?;
        Ok(KeyedSessionReport::from_core(
            report,
            &self.progress,
            capability,
        ))
    }
}

pub(super) struct KeyedCursor {
    pub(super) snapshot: PlaylistSnapshot,
    source: Arc<dyn Source>,
    resources: Arc<ResourceSession>,
    map: Option<ClearResource>,
}
impl KeyedCursor {
    pub(super) async fn read(
        &mut self,
        index: usize,
        role: InputRole,
        options: &PrepareOptions,
    ) -> SessionResult<(DemuxOutput, SourceLocation, Option<ByteRange>, u64)> {
        let segment = &self.snapshot.segments()[index];
        let location = segment.location().location().clone();
        let range = segment.range().map(|r| r.byte_range());
        let context = |error, phase| SessionError {
            error,
            role: Some(role),
            phase,
            segment_index: Some(index),
            resource: Some(safe_location(&location)),
            byte_range: range,
            keyed_cause: None,
        };
        let resource_error = |error: ResourceError, phase| {
            let mut result = context(Error::invalid("keyed resource preparation failed"), phase);
            if let Some(resource) = error.resource() {
                result.resource = Some(resource.location().diagnostic().to_string());
                result.byte_range = resource.range().map(|r| r.byte_range());
            }
            result.keyed_cause = Some(Box::new(error));
            result
        };
        let read = async {
            if segment.map().is_some() {
                let request = ResourceRequest::from_validated(&self.snapshot, index, true)
                    .map_err(|e| resource_error(e, SessionPhase::Initialization))?;
                self.resources
                    .read_map_cached(self.source.clone(), request, &mut self.map)
                    .await
                    .map_err(|e| resource_error(e, SessionPhase::Initialization))?;
            }
            let request = ResourceRequest::from_validated(&self.snapshot, index, false)
                .map_err(|e| resource_error(e, SessionPhase::Downloading))?;
            self.resources
                .read(self.source.clone(), request)
                .await
                .map_err(|e| resource_error(e, SessionPhase::Downloading))
        };
        let bytes = if let Some(cancel) = &options.cancel {
            if cancel.is_cancelled() {
                return Err(context(Error::Cancelled, SessionPhase::Downloading));
            }
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(context(Error::Cancelled, SessionPhase::Downloading)),
                result = read => result?,
            }
        } else {
            read.await?
        };
        let data = if let Some(map) = &self.map {
            demux_isobmff(map.bytes(), bytes.bytes())
        } else {
            demux_ts(bytes.bytes())
        }
        .map_err(|e| context(e, SessionPhase::Processing))?;
        Ok((data, location, range, bytes.bytes().len() as u64))
    }
}

fn observed_capability(
    info: &PreparedInfo,
    snapshots: &[PlaylistSnapshot],
    output: KeyedOutput,
) -> KeyedCapabilityQuery {
    let mut inputs = snapshots.iter().enumerate().map(|(i, s)| {
        let keys = s
            .segments()
            .iter()
            .flat_map(|s| std::iter::once(s.keys()).chain(s.map().map(|m| m.keys())))
            .collect::<Vec<_>>();
        let encrypted = keys.iter().any(|k| !k.is_clear());
        let clear = keys.iter().any(|k| k.is_clear());
        let role = if i == 0 {
            InputRole::Primary
        } else {
            InputRole::Audio
        };
        KeyedInputCapability::new(
            if s.segments()[0].map().is_some() {
                KeyedContainer::FragmentedMp4
            } else {
                KeyedContainer::TransportStream
            },
            if !encrypted {
                KeyedEncryption::Clear
            } else if clear {
                KeyedEncryption::ClearAndAes128
            } else {
                KeyedEncryption::Aes128
            },
            info.tracks
                .iter()
                .zip(&info.timeline.tracks)
                .filter(|(_, t)| t.role == role)
                .map(|(t, _)| match t.codec {
                    crate::Codec::Avc => KeyedCodec::Avc,
                    crate::Codec::Hevc => KeyedCodec::Hevc,
                    crate::Codec::Aac => KeyedCodec::AacLc,
                })
                .collect(),
        )
    });
    let mut query = KeyedCapabilityQuery::new(inputs.next().unwrap(), output);
    if let Some(audio) = inputs.next() {
        query = query.with_audio(audio);
    }
    if snapshots
        .iter()
        .flat_map(|s| s.segments())
        .any(|s| s.range().is_some() || s.map().is_some_and(|m| m.range().is_some()))
    {
        let encrypted = snapshots.iter().flat_map(|s| s.segments()).any(|s| {
            (s.range().is_some() && !s.keys().is_clear())
                || s.map()
                    .is_some_and(|m| m.range().is_some() && !m.keys().is_clear())
        });
        query = query.with_range(if encrypted {
            KeyedRange::CompleteEncryptedResources
        } else {
            KeyedRange::ClearByteRanges
        });
    }
    query
}

fn preflight_context(
    snapshot: &PlaylistSnapshot,
    rejection: PlaylistRejection,
) -> Option<KeyResource> {
    use crate::playlist::EncryptionMethod as M;
    for segment in snapshot.segments() {
        if rejection == PlaylistRejection::Gap && segment.gap()
            || rejection == PlaylistRejection::Discontinuity && segment.discontinuity()
        {
            return Some(KeyResource::media(segment));
        }
        for resource in
            std::iter::once(KeyResource::media(segment)).chain(KeyResource::map(segment))
        {
            let keys = resource.keys().candidates();
            let matches = match rejection {
                PlaylistRejection::MissingMapIv => {
                    resource.kind() == crate::crypto::key::KeyResourceKind::Map
                        && keys.iter().any(|k| k.explicit_iv().is_none())
                }
                PlaylistRejection::ConflictingKeyMethods => keys
                    .first()
                    .is_some_and(|first| keys.iter().any(|k| k.method() != first.method())),
                PlaylistRejection::SampleEncryption => keys
                    .iter()
                    .any(|k| matches!(k.method(), M::SampleAes | M::SampleAesCtr)),
                PlaylistRejection::UnknownEncryption => keys
                    .iter()
                    .any(|k| !matches!(k.method(), M::Aes128 | M::SampleAes | M::SampleAesCtr)),
                _ => false,
            };
            if matches {
                return Some(resource);
            }
        }
    }
    None
}
