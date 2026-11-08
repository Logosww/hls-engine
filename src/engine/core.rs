//! Shared Engine execution core.
use super::*;
use crate::crypto::{key::KeySession, resource::*};
use crate::playlist::{InputId, PlaylistSnapshot, SegmentDescriptor, SegmentSlot};
use std::{future::Future, pin::Pin};
mod aac;
use aac::AacRepairState;
pub use aac::{
    AacRepairPosition, AacRepairSummary, AacTimestampOverlapError, AacTimestampPolicy,
    AacTimestampRepairError,
};
mod control;
mod engine;
mod model;
mod multitrack;
mod sidecar;
mod subtitles;
pub use multitrack::*;
pub use sidecar::*;
pub use subtitles::*;
mod output;
mod recovery;
pub use recovery::*;
#[cfg(feature = "serde")]
mod wire;
pub(crate) use control::ControlHandle;
use control::*;
use engine::*;
pub use model::*;
#[cfg(not(target_arch = "wasm32"))]
pub use output::EngineFileProvider;
pub use output::{EngineOutputRequest, EngineWriterProvider};

pub(crate) struct SessionCore {
    subtitle_sink: Option<Arc<dyn SubtitleSink>>,
    #[cfg(not(target_arch = "wasm32"))]
    recoverable_subtitles: Option<Arc<dyn RecoverableSubtitleSink>>,
    recovery: Option<EngineCheckpoint>,
    keep_embedded: bool,
    multi: Option<Arc<std::sync::Mutex<multitrack::MultiState>>>,
    shared: Arc<Shared>,
    resources: Arc<ResourceSession>,
    sources: Vec<SessionSource>,
    options: EngineOptions,
}
impl SessionCore {
    #[cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]
    pub fn new(inputs: CoreInputs, keys: KeySession, options: EngineOptions) -> EngineResult<Self> {
        options.validate(inputs.inputs.len())?;
        if inputs
            .inputs
            .iter()
            .enumerate()
            .any(|(n, i)| inputs.inputs[..n].iter().any(|j| j.id == i.id))
        {
            return Err(fail(EngineErrorKind::InvalidOptions));
        }
        let resources = Arc::new(
            ResourceSession::new(keys, options.resources.clone()).map_err(resource_error)?,
        );
        // Control state must not retain user callbacks: callbacks commonly own a
        // handle, and retaining them here would create an operation reference cycle.
        let mut control_options = options.clone();
        control_options.event = None;
        control_options.waiter = None;
        let shared = Arc::new(Shared {
            inner: std::sync::Mutex::new(State {
                lanes: inputs
                    .inputs
                    .iter()
                    .map(|i| LaneState::new(i.id.clone()))
                    .collect(),
                global_history: false,
                state: EngineState::Preparing,
                reason: None,
                paused: false,
                blocked: false,
                checkpointing: false,
                queued: 0,
                metadata: 0,
                peaks: EnginePeaks::default(),
                completed: VecDeque::new(),
                published: false,
            }),
            signal: Arc::new(Signal::new()),
            options: control_options,
        });
        let settings = SourceSessionOptions {
            demand_driven: true,
            max_resource_bytes: Some(options.resources.max_resource_bytes()),
        };
        let keep_embedded = inputs.inputs.len() == 1;
        let sources = inputs
            .inputs
            .into_iter()
            .map(|i| {
                SessionSource(
                    i.source
                        .create_session_with_options(&settings)
                        .unwrap_or(i.source),
                )
            })
            .collect();
        Ok(Self {
            subtitle_sink: None,
            recovery: None,
            #[cfg(not(target_arch = "wasm32"))]
            recoverable_subtitles: None,
            keep_embedded,
            multi: None,
            shared,
            resources,
            sources,
            options,
        })
    }
    pub fn handle(&self) -> ControlHandle {
        ControlHandle {
            shared: self.shared.clone(),
        }
    }
    fn check(&self) -> EngineResult<()> {
        if self.shared.signal.is_cancelled() {
            Err(fail(EngineErrorKind::Cancelled))
        } else {
            Ok(())
        }
    }
    async fn wait<T>(&self, future: impl Future<Output = EngineResult<T>>) -> EngineResult<T> {
        self.check()?;
        tokio::select! { biased;
            _ = self.shared.signal.cancelled() => Err(fail(EngineErrorKind::Cancelled)),
            value = future => { self.check()?; value }
        }
    }
    fn emit(&self, event: EngineEvent) -> EngineResult<()> {
        self.check()?;
        if let Some(callback) = &self.options.event {
            callback(event);
        }
        self.check()
    }
    fn state(&self, value: EngineState) -> EngineResult<()> {
        let changed = {
            let mut state = self.shared.inner.lock().unwrap();
            let changed = state.state != value;
            state.state = value;
            changed
        };
        self.shared.signal.wake();
        if changed {
            self.emit(EngineEvent::State(value))?;
        }
        Ok(())
    }
    async fn pause_boundary(&self) -> EngineResult<()> {
        let mut wake = self.shared.signal.changed.subscribe();
        loop {
            self.check()?;
            let (paused, draining) = {
                let state = self.shared.inner.lock().unwrap();
                (state.paused, state.reason.is_some())
            };
            if draining {
                self.state(EngineState::Draining)?;
                return Ok(());
            }
            if !paused {
                self.state(EngineState::Running)?;
                return Ok(());
            }
            self.state(EngineState::Paused)?;
            self.wait(async {
                let _ = wake.changed().await;
                Ok(())
            })
            .await?;
        }
    }
    fn blocked(&self, blocked: bool) {
        self.shared.inner.lock().unwrap().blocked = blocked;
        self.shared.signal.wake();
    }
    fn finish<T>(&self, result: EngineResult<T>) -> EngineResult<T> {
        let mut state = self.shared.inner.lock().unwrap();
        let mut result = if self.shared.signal.is_cancelled() {
            Err(fail(EngineErrorKind::Cancelled))
        } else {
            result
        };
        if let Err(error) = &mut result {
            error.completed = state.completed.iter().cloned().collect();
        }
        state.state = match &result {
            Ok(_) => EngineState::Completed,
            Err(e) if e.kind == EngineErrorKind::Cancelled => EngineState::Cancelled,
            Err(_) => EngineState::Failed,
        };
        let terminal = state.state;
        for lane in &mut state.lanes {
            lane.queue.clear();
            lane.history.clear();
        }
        state.queued = 0;
        state.metadata = 0;
        state.blocked = false;
        state.checkpointing = false;
        drop(state);
        if let Some(state) = &self.multi {
            state.lock().unwrap().clear_subtitles();
        }
        self.resources.cancel();
        self.shared.signal.wake();
        // This is the linearized terminal event; later cancel/stop is inert.
        if let Some(callback) = &self.options.event {
            callback(EngineEvent::State(terminal));
        }
        result
    }
}
impl Drop for SessionCore {
    fn drop(&mut self) {
        let mut state = self.shared.inner.lock().unwrap();
        if !state.state.terminal() {
            state.state = EngineState::Cancelled;
            self.shared.signal.mark_cancelled();
        }
        for lane in &mut state.lanes {
            lane.queue.clear();
            lane.history.clear();
        }
        drop(state);
        if let Some(state) = &self.multi {
            state.lock().unwrap().clear_subtitles();
        }
        self.resources.cancel();
        self.shared.signal.wake();
    }
}
pub(super) fn fail(kind: EngineErrorKind) -> EngineError {
    EngineError {
        kind,
        slot: None,
        resource: None,
        sample: None,
        cause: None,
        completed: vec![],
        aac: None,
        aac_overlap: None,
    }
}
fn at(kind: EngineErrorKind, segment: &SegmentDescriptor) -> EngineError {
    EngineError {
        slot: Some(segment.slot().clone()),
        ..fail(kind)
    }
}
pub(super) fn media_error(cause: Error) -> EngineError {
    let kind = if matches!(cause, Error::Cancelled) {
        EngineErrorKind::Cancelled
    } else {
        EngineErrorKind::Media
    };
    EngineError {
        cause: Some(Box::new(cause)),
        ..fail(kind)
    }
}
fn output_error(cause: Error) -> EngineError {
    let capacity = matches!(&cause, Error::Io(e) if e.kind() == std::io::ErrorKind::OutOfMemory);
    let mut e = media_error(cause);
    if e.kind != EngineErrorKind::Cancelled {
        e.kind = if capacity {
            EngineErrorKind::BudgetExceeded
        } else {
            EngineErrorKind::Output
        };
    }
    e
}
fn resource_error(resource: ResourceError) -> EngineError {
    if resource
        .key_error()
        .is_some_and(|e| e.kind() == crate::crypto::key::KeyErrorKind::ResumeConflict)
    {
        return EngineError {
            resource: Some(Box::new(resource)),
            ..fail(EngineErrorKind::ResumeConflict)
        };
    }
    let kind = match resource.kind() {
        ResourceErrorKind::Cancelled => EngineErrorKind::Cancelled,
        ResourceErrorKind::AuthenticationFailed => EngineErrorKind::AuthenticationFailed,
        _ => EngineErrorKind::Resource,
    };
    EngineError {
        resource: Some(Box::new(resource)),
        ..fail(kind)
    }
}
fn zero() -> MediaTime {
    MediaTime {
        ticks: 0,
        timescale: 1,
    }
}
fn max(a: MediaTime, b: MediaTime) -> EngineResult<MediaTime> {
    Ok(if cmp(a, b)?.is_lt() { b } else { a })
}
fn min(a: MediaTime, b: MediaTime) -> EngineResult<MediaTime> {
    Ok(if cmp(a, b)?.is_gt() { b } else { a })
}
