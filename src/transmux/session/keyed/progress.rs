use super::*;
use std::sync::Mutex;

/// Counters for bounded completed reads, successful clear validation, and MAP reuse.
/// Downloads count returned wire bytes; decrypt counters include encrypted resources only.
#[derive(Debug, Clone, Default)]
pub struct KeyedResourceProgress {
    downloaded: usize,
    downloaded_bytes: u64,
    decrypted: usize,
    decrypted_bytes: u64,
    ready: usize,
    clear_bytes: u64,
    reused: usize,
}
impl KeyedResourceProgress {
    pub fn downloaded_resources(&self) -> usize {
        self.downloaded
    }
    pub fn downloaded_bytes(&self) -> u64 {
        self.downloaded_bytes
    }
    pub fn decrypted_resources(&self) -> usize {
        self.decrypted
    }
    pub fn decrypted_bytes(&self) -> u64 {
        self.decrypted_bytes
    }
    pub fn ready_resources(&self) -> usize {
        self.ready
    }
    pub fn clear_bytes(&self) -> u64 {
        self.clear_bytes
    }
    pub fn cache_reuses(&self) -> usize {
        self.reused
    }
    fn record(&mut self, observation: &ResourceObservation<'_>) -> ResourceResult<()> {
        let overflow = || ResourceError::new(ResourceErrorKind::CounterOverflow);
        // Update a copy so failure cannot leave half a counter update.
        let mut next = self.clone();
        match observation.stage {
            ResourceStage::Downloaded => {
                next.downloaded = next.downloaded.checked_add(1).ok_or_else(overflow)?;
                next.downloaded_bytes = next
                    .downloaded_bytes
                    .checked_add(observation.bytes)
                    .ok_or_else(overflow)?;
            }
            ResourceStage::Ready => {
                next.ready = next.ready.checked_add(1).ok_or_else(overflow)?;
                next.clear_bytes = next
                    .clear_bytes
                    .checked_add(observation.bytes)
                    .ok_or_else(overflow)?;
            }
            ResourceStage::Decrypted => {
                next.decrypted = next.decrypted.checked_add(1).ok_or_else(overflow)?;
                next.decrypted_bytes = next
                    .decrypted_bytes
                    .checked_add(observation.bytes)
                    .ok_or_else(overflow)?;
            }
            ResourceStage::MapReused => {
                next.reused = next.reused.checked_add(1).ok_or_else(overflow)?
            }
        }
        *self = next;
        Ok(())
    }
}
#[derive(Debug, Clone)]
pub struct KeyedInputProgress {
    input_id: InputId,
    generation: u64,
    progress: InputProgress,
    media: KeyedResourceProgress,
    maps: KeyedResourceProgress,
}
impl KeyedInputProgress {
    pub fn input_id(&self) -> &InputId {
        &self.input_id
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn role(&self) -> InputRole {
        self.progress.role
    }
    pub fn total_segments(&self) -> usize {
        self.progress.total
    }
    pub fn discovered_segments(&self) -> usize {
        self.progress.total
    }
    pub fn downloaded_segments(&self) -> usize {
        self.media.downloaded
    }
    pub fn downloaded_bytes(&self) -> u64 {
        self.media.downloaded_bytes
    }
    pub fn decrypted_segments(&self) -> usize {
        self.media.decrypted
    }
    pub fn decrypted_bytes(&self) -> u64 {
        self.media.decrypted_bytes
    }
    pub fn ready_segments(&self) -> usize {
        self.media.ready
    }
    /// Same committed boundary as processed_segments(); never a durable checkpoint.
    pub fn committed_segments(&self) -> usize {
        self.progress.processed
    }
    pub fn processed_segments(&self) -> usize {
        self.progress.processed
    }
    pub fn media(&self) -> &KeyedResourceProgress {
        &self.media
    }
    pub fn maps(&self) -> &KeyedResourceProgress {
        &self.maps
    }
}
#[derive(Default)]
pub(super) struct ProgressState {
    inputs: Vec<KeyedInputProgress>,
    bytes: u64,
}
pub(super) type SharedProgress = Arc<Mutex<ProgressState>>;
impl ProgressState {
    pub(super) fn new(snapshots: &[PlaylistSnapshot]) -> SharedProgress {
        Arc::new(Mutex::new(Self {
            inputs: snapshots
                .iter()
                .enumerate()
                .map(|(i, s)| KeyedInputProgress {
                    input_id: s.context().input_id().clone(),
                    generation: s.context().generation(),
                    progress: InputProgress {
                        role: if i == 0 {
                            InputRole::Primary
                        } else {
                            InputRole::Audio
                        },
                        total: s.segments().len(),
                        downloaded: 0,
                        downloaded_bytes: 0,
                        processed: 0,
                    },
                    media: KeyedResourceProgress::default(),
                    maps: KeyedResourceProgress::default(),
                })
                .collect(),
            bytes: 0,
        }))
    }
    pub(super) fn inputs(&self) -> Vec<KeyedInputProgress> {
        self.inputs.clone()
    }
    pub(super) fn observe(&mut self, observation: &ResourceObservation<'_>) -> ResourceResult<()> {
        let input = self
            .inputs
            .iter_mut()
            .find(|i| {
                &i.input_id == observation.resource.slot().input_id()
                    && i.generation == observation.resource.slot().generation()
            })
            .ok_or_else(|| ResourceError::new(ResourceErrorKind::InvalidIndex))?;
        match observation.resource.kind() {
            crate::crypto::key::KeyResourceKind::Media => input.media.record(observation),
            crate::crypto::key::KeyResourceKind::Map => input.maps.record(observation),
        }
    }
    pub(super) fn update_core(&mut self, event: &SessionEvent) {
        for (input, progress) in self.inputs.iter_mut().zip(&event.inputs) {
            input.progress = progress.clone();
        }
        self.bytes = event.bytes;
    }
    pub(super) fn event(
        &self,
        phase: KeyedSessionPhase,
        resource: Option<KeyResource>,
    ) -> KeyedSessionEvent {
        KeyedSessionEvent {
            phase,
            slot: resource.as_ref().map(|r| r.slot().clone()),
            resource,
            inputs: self.inputs(),
            bytes: self.bytes,
        }
    }
}
