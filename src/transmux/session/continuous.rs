#![doc = include_str!("../../../docs/continuous-sessions.md")]
use super::*;
use crate::crypto::{key::KeySession, resource::*};
use crate::playlist::{InputId, PlaylistSnapshot, SegmentDescriptor, SegmentSlot};
use std::{future::Future, pin::Pin};
mod control;
mod engine;
mod model;
mod multitrack;
mod subtitles;
pub use multitrack::*;
pub use subtitles::*;
mod output;
#[cfg(feature = "serde")]
mod wire;
pub use control::ContinuousHandle;
use control::*;
use engine::*;
pub use model::*;
#[cfg(not(target_arch = "wasm32"))]
pub use output::ContinuousFileProvider;
pub use output::{ContinuousOutputRequest, ContinuousWriterProvider};

pub struct ContinuousSession {
    keep_embedded: bool,
    multi: Option<Arc<std::sync::Mutex<multitrack::MultiState>>>,
    shared: Arc<Shared>,
    resources: Arc<ResourceSession>,
    sources: Vec<SessionSource>,
    options: ContinuousOptions,
}
impl ContinuousSession {
    #[cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]
    pub fn new(
        inputs: ContinuousInputs,
        keys: KeySession,
        options: ContinuousOptions,
    ) -> ContinuousResult<Self> {
        options.validate(inputs.inputs.len())?;
        if inputs
            .inputs
            .iter()
            .enumerate()
            .any(|(n, i)| inputs.inputs[..n].iter().any(|j| j.id == i.id))
        {
            return Err(fail(ContinuousErrorKind::InvalidOptions));
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
                state: ContinuousState::Preparing,
                reason: None,
                paused: false,
                blocked: false,
                queued: 0,
                metadata: 0,
                peaks: ContinuousPeaks::default(),
                completed: VecDeque::new(),
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
            keep_embedded,
            multi: None,
            shared,
            resources,
            sources,
            options,
        })
    }
    pub fn handle(&self) -> ContinuousHandle {
        ContinuousHandle {
            shared: self.shared.clone(),
        }
    }
    fn check(&self) -> ContinuousResult<()> {
        if self.shared.signal.is_cancelled() {
            Err(fail(ContinuousErrorKind::Cancelled))
        } else {
            Ok(())
        }
    }
    async fn wait<T>(
        &self,
        future: impl Future<Output = ContinuousResult<T>>,
    ) -> ContinuousResult<T> {
        self.check()?;
        tokio::select! { biased;
            _ = self.shared.signal.cancelled() => Err(fail(ContinuousErrorKind::Cancelled)),
            value = future => { self.check()?; value }
        }
    }
    fn emit(&self, event: ContinuousEvent) -> ContinuousResult<()> {
        self.check()?;
        if let Some(callback) = &self.options.event {
            callback(event);
        }
        self.check()
    }
    fn state(&self, value: ContinuousState) -> ContinuousResult<()> {
        let changed = {
            let mut state = self.shared.inner.lock().unwrap();
            let changed = state.state != value;
            state.state = value;
            changed
        };
        self.shared.signal.wake();
        if changed {
            self.emit(ContinuousEvent::State(value))?;
        }
        Ok(())
    }
    async fn pause_boundary(&self) -> ContinuousResult<()> {
        let mut wake = self.shared.signal.changed.subscribe();
        loop {
            self.check()?;
            let (paused, draining) = {
                let state = self.shared.inner.lock().unwrap();
                (state.paused, state.reason.is_some())
            };
            if draining {
                self.state(ContinuousState::Draining)?;
                return Ok(());
            }
            if !paused {
                self.state(ContinuousState::Running)?;
                return Ok(());
            }
            self.state(ContinuousState::Paused)?;
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
    fn finish<T>(&self, result: ContinuousResult<T>) -> ContinuousResult<T> {
        let mut state = self.shared.inner.lock().unwrap();
        let mut result = if self.shared.signal.is_cancelled() {
            Err(fail(ContinuousErrorKind::Cancelled))
        } else {
            result
        };
        if let Err(error) = &mut result {
            error.completed = state.completed.iter().cloned().collect();
        }
        state.state = match &result {
            Ok(_) => ContinuousState::Completed,
            Err(e) if e.kind == ContinuousErrorKind::Cancelled => ContinuousState::Cancelled,
            Err(_) => ContinuousState::Failed,
        };
        let terminal = state.state;
        for lane in &mut state.lanes {
            lane.queue.clear();
            lane.history.clear();
        }
        state.queued = 0;
        state.metadata = 0;
        state.blocked = false;
        drop(state);
        if let Some(state) = &self.multi {
            state.lock().unwrap().clear_subtitles();
        }
        self.resources.cancel();
        self.shared.signal.wake();
        // This is the linearized terminal event; later cancel/stop is inert.
        if let Some(callback) = &self.options.event {
            callback(ContinuousEvent::State(terminal));
        }
        result
    }
}
impl Drop for ContinuousSession {
    fn drop(&mut self) {
        let mut state = self.shared.inner.lock().unwrap();
        if !state.state.terminal() {
            state.state = ContinuousState::Cancelled;
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
fn fail(kind: ContinuousErrorKind) -> ContinuousError {
    ContinuousError {
        kind,
        slot: None,
        resource: None,
        sample: None,
        cause: None,
        completed: vec![],
    }
}
fn at(kind: ContinuousErrorKind, segment: &SegmentDescriptor) -> ContinuousError {
    ContinuousError {
        slot: Some(segment.slot().clone()),
        ..fail(kind)
    }
}
fn media_error(cause: Error) -> ContinuousError {
    let kind = if matches!(cause, Error::Cancelled) {
        ContinuousErrorKind::Cancelled
    } else {
        ContinuousErrorKind::Media
    };
    ContinuousError {
        cause: Some(Box::new(cause)),
        ..fail(kind)
    }
}
fn output_error(cause: Error) -> ContinuousError {
    let capacity = matches!(&cause, Error::Io(e) if e.kind() == std::io::ErrorKind::OutOfMemory);
    let mut e = media_error(cause);
    if e.kind != ContinuousErrorKind::Cancelled {
        e.kind = if capacity {
            ContinuousErrorKind::BudgetExceeded
        } else {
            ContinuousErrorKind::Output
        };
    }
    e
}
fn resource_error(resource: ResourceError) -> ContinuousError {
    let kind = if resource.kind() == ResourceErrorKind::Cancelled {
        ContinuousErrorKind::Cancelled
    } else {
        ContinuousErrorKind::Resource
    };
    ContinuousError {
        resource: Some(Box::new(resource)),
        ..fail(kind)
    }
}
fn timeline_error(e: TimelineSessionError) -> ContinuousError {
    fail(match e.kind() {
        TimelineErrorKind::TimeOverflow => ContinuousErrorKind::TimeOverflow,
        _ => ContinuousErrorKind::TimelineAmbiguous,
    })
}
fn add(a: MediaTime, b: MediaTime) -> ContinuousResult<MediaTime> {
    super::timeline::add(a, b).map_err(timeline_error)
}
fn sub(a: MediaTime, b: MediaTime) -> ContinuousResult<MediaTime> {
    super::timeline::sub(a, b).map_err(timeline_error)
}
fn cmp(a: MediaTime, b: MediaTime) -> ContinuousResult<std::cmp::Ordering> {
    super::timeline::cmp(a, b).map_err(timeline_error)
}
fn scale(a: MediaTime, b: u32) -> ContinuousResult<i128> {
    super::timeline::rescale(a, b).map_err(timeline_error)
}
fn zero() -> MediaTime {
    MediaTime {
        ticks: 0,
        timescale: 1,
    }
}
fn max(a: MediaTime, b: MediaTime) -> ContinuousResult<MediaTime> {
    Ok(if cmp(a, b)?.is_lt() { b } else { a })
}
fn min(a: MediaTime, b: MediaTime) -> ContinuousResult<MediaTime> {
    Ok(if cmp(a, b)?.is_gt() { b } else { a })
}
