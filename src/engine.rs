//! Unified selected-input engine. VOD and open input use the same bounded core.
//!
//! Submit immutable snapshots through the handle, then end each finite input.
//! Sources, key providers and borrowed writers retain caller ownership.

pub use crate::capabilities::{
    MultiTrackCapabilityDecision as CapabilityDecision,
    MultiTrackCapabilityQuery as CapabilityQuery, query_multitrack_capability as query_capability,
};
#[cfg(not(target_arch = "wasm32"))]
pub use crate::legacy::ContinuousFileProvider as EngineFileProvider;
pub use crate::legacy::{
    ByteRange, CancelToken, CheckpointDurability, CommittedSubtitleCue, EmbeddedAudio, EngineCause,
    FileOutputOptions, FinalizeBackend, GapPolicy, MediaTime, MemorySource, MissingSegmentPolicy,
    OutputFormat, OutputTrackCodec, OutputTrackId, OutputTrackInfo, OutputTrackKind,
    PresentationRange, Source, SourceLocation, SourceSessionOptions, SubtitleAcceptance,
    SubtitleCommit, SubtitleCue, SubtitleCueReport, SubtitleDisposition, SubtitleFrontier,
    SubtitleSink, SubtitleSinkFuture, SubtitleTrack, TailDurationPolicy, TextResource,
    TimelineChangePolicy, TrackMetadata,
};
pub use crate::legacy::{
    ContinuousAnchor as EngineAnchor, ContinuousEndReason as EngineEndReason,
    ContinuousError as EngineError, ContinuousErrorKind as EngineErrorKind,
    ContinuousEvent as EngineEvent, ContinuousEventCallback as EngineEventCallback,
    ContinuousInput as EngineInput, ContinuousInputProgress as EngineInputProgress,
    ContinuousLimits as EngineLimits, ContinuousMapping as EngineMapping,
    ContinuousMode as EngineMode, ContinuousOptions as EngineOptions,
    ContinuousOutputReport as EngineOutputReport, ContinuousOutputRequest as EngineOutputRequest,
    ContinuousPeaks as EnginePeaks, ContinuousReport as EngineMediaReport,
    ContinuousResult as EngineResult, ContinuousState as EngineState, ContinuousWait as EngineWait,
    ContinuousWriterProvider as EngineWriterProvider, MultiTrackHandle as EngineHandle,
    MultiTrackInputs as EngineInputs, MultiTrackReport as EngineReport,
    MultiTrackSession as EngineSession,
};
#[cfg(feature = "default-source")]
pub use crate::legacy::{HttpRequestPolicy, ReqwestSource};

#[cfg(not(target_arch = "wasm32"))]
pub use crate::legacy::{
    CheckpointCallback, RecoverableSubtitleSink, RecoveryOptions, SubtitleDestination,
};
pub use crate::legacy::{ENGINE_CHECKPOINT_SCHEMA_VERSION, EngineCheckpoint};
