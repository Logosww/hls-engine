use super::*;
use crate::playlist::reconcile::{Declarations, metadata_bytes};

pub(super) struct LaneState {
    pub id: InputId,
    pub queue: VecDeque<SegmentDescriptor>,
    pub history: VecDeque<SegmentDescriptor>,
    pub generation: Option<u64>,
    pub revision: Option<u64>,
    pub media_sequence: Option<u64>,
    pub last: Option<u64>,
    pub outstanding: usize,
    pub ended: bool,
    pub restart: Option<u64>,
    pub progress: ContinuousInputProgress,
}
impl LaneState {
    pub fn new(id: InputId) -> Self {
        Self {
            progress: ContinuousInputProgress {
                input: id.clone(),
                discovered: 0,
                accepted: 0,
                downloaded: 0,
                decrypted: 0,
                committed: 0,
                watermark: None,
            },
            id,
            queue: VecDeque::new(),
            history: VecDeque::new(),
            generation: None,
            revision: None,
            media_sequence: None,
            last: None,
            outstanding: 0,
            ended: false,
            restart: None,
        }
    }
}
pub(super) struct State {
    pub lanes: Vec<LaneState>,
    pub global_history: bool,
    pub state: ContinuousState,
    pub reason: Option<ContinuousEndReason>,
    pub paused: bool,
    pub blocked: bool,
    pub queued: usize,
    pub metadata: usize,
    pub peaks: ContinuousPeaks,
    pub completed: VecDeque<ContinuousOutputReport>,
    // Publication wins cancellation, but a fallible checkpoint callback still
    // determines the terminal result. Do not expose Completed before it returns.
    pub published: bool,
}
pub(super) struct Shared {
    pub inner: std::sync::Mutex<State>,
    pub signal: Arc<Signal>,
    pub options: ContinuousOptions,
}
#[derive(Debug)]
pub(super) struct Signal {
    cancelled: std::sync::atomic::AtomicBool,
    pub changed: tokio::sync::watch::Sender<()>,
}
impl Signal {
    pub fn new() -> Self {
        Self {
            cancelled: std::sync::atomic::AtomicBool::new(false),
            changed: tokio::sync::watch::channel(()).0,
        }
    }
    pub fn wake(&self) {
        self.changed.send_replace(());
    }
    pub fn mark_cancelled(&self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Release);
    }
}
impl CancelToken for Signal {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(std::sync::atomic::Ordering::Acquire)
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            let mut wake = self.changed.subscribe();
            while !self.is_cancelled() {
                if wake.changed().await.is_err() {
                    break;
                }
            }
        })
    }
}
/// A clonable admission/control endpoint; it never owns or closes a sink.
#[derive(Clone)]
pub struct ContinuousHandle {
    pub(super) shared: Arc<Shared>,
}
impl ContinuousHandle {
    pub fn state(&self) -> ContinuousState {
        self.shared.inner.lock().unwrap().state
    }
    pub fn progress(&self) -> Vec<ContinuousInputProgress> {
        self.shared
            .inner
            .lock()
            .unwrap()
            .lanes
            .iter()
            .map(|l| l.progress.clone())
            .collect()
    }
    pub fn accept_snapshot(
        &self,
        input: &InputId,
        snapshot: &PlaylistSnapshot,
    ) -> ContinuousResult<SnapshotAcceptance> {
        snapshot
            .validate_engine(self.shared.options.resources.experimental_gcm())
            .map_err(|_| fail(ContinuousErrorKind::UnsupportedPlaylist))?;
        if snapshot.context().input_id() != input {
            return Err(fail(ContinuousErrorKind::UnknownInput));
        }
        if self.shared.options.mode == ContinuousMode::Vod
            && (!snapshot.end_list()
                || snapshot.playlist_type() == Some(&crate::playlist::PlaylistType::Event))
        {
            return Err(fail(ContinuousErrorKind::UnsupportedPlaylist));
        }
        let mut state = self.shared.inner.lock().unwrap();
        if self.shared.signal.is_cancelled() {
            return Err(fail(ContinuousErrorKind::Cancelled));
        }
        if state.reason.is_some() || state.state.terminal() {
            return Err(fail(ContinuousErrorKind::Closed));
        }
        let index = state
            .lanes
            .iter()
            .position(|l| &l.id == input)
            .ok_or_else(|| fail(ContinuousErrorKind::UnknownInput))?;
        let lane = &state.lanes[index];
        let generation = snapshot.context().generation();
        let restarting = lane.restart == Some(generation);
        if restarting && lane.outstanding != 0 {
            return Err(fail(ContinuousErrorKind::WouldBlock));
        }
        if !restarting && lane.generation.is_some_and(|g| g != generation) {
            return Err(fail(ContinuousErrorKind::GenerationMismatch));
        }
        if lane.ended && !restarting {
            return Err(fail(ContinuousErrorKind::Closed));
        }
        let revision = snapshot.context().revision();
        if !restarting
            && (lane.revision.is_some_and(|r| revision < r)
                || lane
                    .media_sequence
                    .is_some_and(|s| snapshot.media_sequence() < s))
        {
            return Err(fail(ContinuousErrorKind::RevisionRollback));
        }
        let last = if restarting { None } else { lane.last };
        let limits = &self.shared.options.limits;
        let mut declarations = Declarations::default();
        let mut overlap = false;
        let mut added = Vec::new();
        let mut bytes = 0usize;
        let mut duplicates = 0usize;
        // Check all retained overlap before canonicalizing any new declarations.
        for segment in snapshot.segments() {
            if !restarting
                && let Some(old) = lane
                    .history
                    .iter()
                    .chain(lane.queue.iter())
                    .find(|s| s.slot().sequence() == segment.slot().sequence())
            {
                overlap = true;
                if !declarations.observe(segment, old) {
                    return Err(at(ContinuousErrorKind::InputRewrite, segment));
                }
            }
            if last.is_some_and(|n| segment.slot().sequence() <= n) {
                duplicates += 1;
                continue;
            }
            bytes = bytes
                .checked_add(metadata_bytes(segment))
                .ok_or_else(|| fail(ContinuousErrorKind::QueueLimit))?;
            if added.len() >= limits.descriptors || bytes > limits.metadata {
                return Err(fail(ContinuousErrorKind::QueueLimit));
            }
            added.push(segment.clone());
        }
        if lane.revision == Some(revision)
            && !restarting
            && (!added.is_empty() || lane.media_sequence != Some(snapshot.media_sequence()))
        {
            return Err(fail(ContinuousErrorKind::InputRewrite));
        }
        if let (Some(last), Some(first)) = (last, added.first()) {
            if last.checked_add(1) != Some(first.slot().sequence()) {
                // Missing unannounced slots have no reliable duration: caller must
                // submit a restart/anchor rather than inventing a time gap.
                return Err(at(ContinuousErrorKind::MissingSegment, first));
            }
            if !overlap && (!first.keys().is_clear() || first.map().is_some()) {
                return Err(at(ContinuousErrorKind::NeedsReconciliation, first));
            }
        }
        for segment in &mut added {
            declarations.apply(segment);
        }
        if state.blocked
            || state.paused
            || state.queued + added.len() > limits.descriptors
            || state.metadata + bytes > limits.metadata
        {
            return Err(fail(ContinuousErrorKind::WouldBlock));
        }
        let accepted = added.len();
        let lane = &mut state.lanes[index];
        if restarting {
            lane.history.clear();
            lane.last = None;
            lane.ended = false;
            lane.restart = None;
        }
        lane.generation = Some(generation);
        lane.revision = Some(revision);
        lane.media_sequence = Some(snapshot.media_sequence());
        lane.progress.discovered = lane
            .progress
            .discovered
            .checked_add(accepted as u64)
            .ok_or_else(|| fail(ContinuousErrorKind::TimeOverflow))?;
        lane.progress.accepted = lane.progress.discovered;
        for segment in added {
            lane.last = Some(segment.slot().sequence());
            lane.history.push_back(segment.clone());
            lane.queue.push_back(segment);
            lane.outstanding += 1;
        }
        // Both a count and a byte bound apply to retained authentication metadata.
        while lane.history.len() > limits.history
            || lane.history.iter().map(metadata_bytes).sum::<usize>() > limits.metadata
        {
            lane.history.pop_front();
        }
        lane.ended = snapshot.end_list();
        if state.global_history {
            while state.lanes.iter().map(|l| l.history.len()).sum::<usize>() > limits.history
                || state
                    .lanes
                    .iter()
                    .flat_map(|l| &l.history)
                    .map(metadata_bytes)
                    .sum::<usize>()
                    > limits.metadata
            {
                let lane = state
                    .lanes
                    .iter_mut()
                    .max_by_key(|l| l.history.len())
                    .unwrap();
                lane.history.pop_front();
            }
        }
        state.queued += accepted;
        state.metadata += bytes;
        state.peaks.queued = state.peaks.queued.max(state.queued);
        state.peaks.metadata = state.peaks.metadata.max(state.metadata);
        drop(state);
        self.shared.signal.wake();
        Ok(SnapshotAcceptance {
            accepted,
            duplicates,
        })
    }
    /// Retry an entire snapshot only when state changes, without a capacity race
    /// or busy loop when the available capacity is smaller than this update.
    pub async fn accept_when_ready(
        &self,
        input: &InputId,
        snapshot: &PlaylistSnapshot,
    ) -> ContinuousResult<SnapshotAcceptance> {
        let mut wake = self.shared.signal.changed.subscribe();
        loop {
            match self.accept_snapshot(input, snapshot) {
                Err(error) if error.kind() == ContinuousErrorKind::WouldBlock => {
                    let _ = wake.changed().await;
                }
                result => return result,
            }
        }
    }
    /// Wait for an admission opportunity, then retry the entire snapshot.
    /// The snapshot's exact incremental size is checked atomically by acceptance.
    pub async fn wait_capacity(&self) -> ContinuousResult<()> {
        let mut wake = self.shared.signal.changed.subscribe();
        loop {
            {
                let state = self.shared.inner.lock().unwrap();
                if self.shared.signal.is_cancelled() {
                    return Err(fail(ContinuousErrorKind::Cancelled));
                }
                if state.reason.is_some() || state.state.terminal() {
                    return Err(fail(ContinuousErrorKind::Closed));
                }
                if !state.blocked
                    && !state.paused
                    && state.queued < self.shared.options.limits.descriptors
                    && state.metadata < self.shared.options.limits.metadata
                {
                    return Ok(());
                }
            }
            let _ = wake.changed().await;
        }
    }
    /// End only one input. Already accepted descriptors are still drained.
    pub fn end_input(&self, input: &InputId) -> ContinuousResult<()> {
        let mut state = self.shared.inner.lock().unwrap();
        if state.state.terminal() || state.reason.is_some() {
            return Err(fail(ContinuousErrorKind::Closed));
        }
        let lane = state
            .lanes
            .iter_mut()
            .find(|l| &l.id == input)
            .ok_or_else(|| fail(ContinuousErrorKind::UnknownInput))?;
        lane.ended = true;
        lane.restart = None;
        drop(state);
        self.shared.signal.wake();
        Ok(())
    }
    /// Seal the old generation. New-generation acceptance waits for its committed drain.
    pub fn restart(&self, input: &InputId, generation: u64) -> ContinuousResult<()> {
        let mut state = self.shared.inner.lock().unwrap();
        if state.state.terminal() || state.reason.is_some() {
            return Err(fail(ContinuousErrorKind::Closed));
        }
        let lane = state
            .lanes
            .iter_mut()
            .find(|l| &l.id == input)
            .ok_or_else(|| fail(ContinuousErrorKind::UnknownInput))?;
        if lane.generation.is_none_or(|old| generation <= old) {
            return Err(fail(ContinuousErrorKind::GenerationMismatch));
        }
        lane.restart = Some(generation);
        lane.ended = true;
        drop(state);
        self.shared.signal.wake();
        Ok(())
    }
    pub fn stop(&self) {
        self.drain(ContinuousEndReason::Stop);
    }
    pub(super) fn drain(&self, reason: ContinuousEndReason) {
        let mut state = self.shared.inner.lock().unwrap();
        if !state.state.terminal() && !state.published && state.reason.is_none() {
            state.reason = Some(reason);
            state.paused = false;
            state.state = ContinuousState::Draining;
            for lane in &mut state.lanes {
                lane.ended = true;
                lane.restart = None;
            }
        }
        drop(state);
        self.shared.signal.wake();
    }
    pub fn cancel(&self) {
        let state = self.shared.inner.lock().unwrap();
        if !state.state.terminal() && !state.published {
            self.shared.signal.mark_cancelled();
        }
        drop(state);
        self.shared.signal.wake();
    }
    pub fn pause(&self) -> ContinuousResult<()> {
        if self.shared.options.mode != ContinuousMode::Vod {
            return Err(fail(ContinuousErrorKind::PauseUnsupported));
        }
        let mut state = self.shared.inner.lock().unwrap();
        if state.state.terminal() || state.reason.is_some() {
            return Err(fail(ContinuousErrorKind::Closed));
        }
        state.paused = true;
        drop(state);
        self.shared.signal.wake();
        Ok(())
    }
    pub fn resume(&self) -> ContinuousResult<()> {
        let mut state = self.shared.inner.lock().unwrap();
        if state.state.terminal() || state.reason.is_some() {
            return Err(fail(ContinuousErrorKind::Closed));
        }
        state.paused = false;
        drop(state);
        self.shared.signal.wake();
        Ok(())
    }
    pub async fn wait_paused(&self) -> ContinuousResult<()> {
        let mut wake = self.shared.signal.changed.subscribe();
        loop {
            match self.state() {
                ContinuousState::Paused => return Ok(()),
                s if s.terminal() || s == ContinuousState::Draining => {
                    return Err(fail(ContinuousErrorKind::Closed));
                }
                _ => {}
            }
            if self.shared.signal.is_cancelled() {
                return Err(fail(ContinuousErrorKind::Cancelled));
            }
            let _ = wake.changed().await;
        }
    }
}
