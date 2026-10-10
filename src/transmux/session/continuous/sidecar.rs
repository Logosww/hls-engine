//! Acknowledged, bounded delivery of subtitle portions after media writes.
#![doc = include_str!("../../../../docs/subtitle-sidecars.md")]
use super::*;

/// A sink acknowledgement covers the entire batch. Implementations must await
/// all required writes; `finish` must await close. Futures are cancellation-safe:
/// cancellation drops them, and a partial external write is not rolled back.
#[cfg(not(target_arch = "wasm32"))]
pub trait SubtitleSink: Send + Sync {
    fn commit<'a>(&'a self, batch: &'a SubtitleCommit) -> SubtitleSinkFuture<'a>;
    fn finish(&self) -> SubtitleSinkFuture<'_>;
}
#[cfg(target_arch = "wasm32")]
pub trait SubtitleSink {
    fn commit<'a>(&'a self, batch: &'a SubtitleCommit) -> SubtitleSinkFuture<'a>;
    fn finish(&self) -> SubtitleSinkFuture<'_>;
}
pub type SubtitleSinkFuture<'a> = Pin<Box<dyn Future<Output = std::io::Result<()>> + 'a>>;

/// One immutable cue portion. Receipt distinguishes even identical cue IDs and
/// bodies. All portions of an admitted cue have the same operation-local receipt.
#[derive(Debug, Clone)]
pub struct CommittedSubtitleCue {
    pub(super) receipt: u64,
    pub(super) input: InputId,
    pub(super) track: OutputTrackId,
    pub(super) cue: SubtitleCue,
    pub(super) disposition: SubtitleDisposition,
    pub(super) output: u64,
    pub(super) start: MediaTime,
    pub(super) end: MediaTime,
}
impl CommittedSubtitleCue {
    pub fn receipt(&self) -> u64 {
        self.receipt
    }
    pub fn input_id(&self) -> &InputId {
        &self.input
    }
    pub fn track_id(&self) -> OutputTrackId {
        self.track
    }
    pub fn cue(&self) -> &SubtitleCue {
        &self.cue
    }
    pub fn disposition(&self) -> SubtitleDisposition {
        self.disposition
    }
    pub fn output_index(&self) -> u64 {
        self.output
    }
    /// Output-local presentation interval, quantized to the embedded wvtt clock.
    /// Rejections retain their proposed interval and must not be written.
    pub fn start(&self) -> MediaTime {
        self.start
    }
    pub fn end(&self) -> MediaTime {
        self.end
    }
    pub(super) fn bytes(&self) -> usize {
        self.cue
            .bytes()
            .saturating_add(self.input.as_str().len())
            .saturating_add(std::mem::size_of::<Self>())
    }
}
/// No subsequently accepted cue may alter output before this frontier.
#[derive(Debug, Clone)]
pub struct SubtitleFrontier {
    pub(super) input: InputId,
    pub(super) track: OutputTrackId,
    pub(super) generation: u64,
    pub(super) epoch: u64,
    pub(super) output: u64,
    pub(super) end: MediaTime,
}
impl SubtitleFrontier {
    pub fn input_id(&self) -> &InputId {
        &self.input
    }
    pub fn track_id(&self) -> OutputTrackId {
        self.track
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn output_index(&self) -> u64 {
        self.output
    }
    pub fn end(&self) -> MediaTime {
        self.end
    }
}
#[derive(Debug, Default)]
pub struct SubtitleCommit {
    pub(super) cues: Vec<CommittedSubtitleCue>,
    pub(super) frontiers: Vec<SubtitleFrontier>,
}
impl SubtitleCommit {
    pub fn cues(&self) -> &[CommittedSubtitleCue] {
        &self.cues
    }
    pub fn frontiers(&self) -> &[SubtitleFrontier] {
        &self.frontiers
    }
}
impl MultiState {
    pub(super) fn stage_subtitle(
        &mut self,
        cue: CommittedSubtitleCue,
        limits: &ContinuousLimits,
    ) -> ContinuousResult<()> {
        if !self.sidecar {
            return Ok(());
        }
        let bytes = self
            .pending_subtitles
            .cues
            .iter()
            .map(CommittedSubtitleCue::bytes)
            .fold(0usize, usize::saturating_add)
            .saturating_add(cue.bytes());
        if self.pending_subtitles.cues.len() >= limits.samples || bytes > limits.sample_bytes {
            return Err(fail(ContinuousErrorKind::BudgetExceeded));
        }
        self.pending_subtitles.cues.push(cue);
        Ok(())
    }
}
impl ContinuousSession {
    pub(super) async fn commit_subtitles(&self) -> ContinuousResult<()> {
        let Some(sink) = &self.subtitle_sink else {
            return Ok(());
        };
        let batch = std::mem::take(
            &mut self
                .multi
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .pending_subtitles,
        );
        if batch.cues.is_empty() && batch.frontiers.is_empty() {
            return Ok(());
        }
        self.blocked(true);
        let result = self
            .wait(async { sink.commit(&batch).await.map_err(sidecar_error) })
            .await;
        self.blocked(false);
        result
    }
    pub(super) async fn finish_subtitle_sink(&self) -> ContinuousResult<()> {
        if let Some(sink) = &self.subtitle_sink {
            self.wait(async { sink.finish().await.map_err(sidecar_error) })
                .await?;
        }
        Ok(())
    }
}
pub(super) fn sidecar_error(error: std::io::Error) -> ContinuousError {
    ContinuousError {
        cause: Some(Box::new(error.into())),
        ..fail(ContinuousErrorKind::SubtitleOutput)
    }
}

/// Native fixed-path adapter. The SDK writes UTF-8 WEBVTT to each destination's
/// `partial_path(output)` and flushes before acknowledging `commit`/`finish`.
/// It must open lazily (after Engine recovery validation), and close the relevant
/// writer before `publish`. Publication must not overwrite an existing file and
/// must retain the partial so an older sealing checkpoint can recover.
#[cfg(not(target_arch = "wasm32"))]
pub trait RecoverableSubtitleSink: SubtitleSink {
    /// Exactly one destination per selected subtitle input, in any order.
    /// Called once before output I/O; must not create/open output files.
    fn destinations(&self) -> Vec<SubtitleDestination>;
    /// Stable SDK serialization/version identity. Changes reject recovery.
    fn format_identity(&self) -> &str;
    /// Called after a sealing/finalizing checkpoint is durably acknowledged.
    /// Publish every track for this output; idempotently accept an existing final
    /// only after Engine's prefix validation. Must await all required I/O.
    fn publish(&self, output: u64) -> SubtitleSinkFuture<'_>;
}
/// Fixed root for one subtitle input. Split children use `.part-NNNNNN.vtt`.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug)]
pub struct SubtitleDestination {
    pub(super) input: InputId,
    pub(super) path: std::path::PathBuf,
}
#[cfg(not(target_arch = "wasm32"))]
impl SubtitleDestination {
    pub fn new(input: InputId, path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            input,
            path: path.into(),
        }
    }
    pub fn input_id(&self) -> &InputId {
        &self.input
    }
    pub fn final_path(&self, output: u64) -> std::path::PathBuf {
        if output == 0 {
            return self.path.clone();
        }
        let mut path = self.path.as_os_str().to_os_string();
        path.push(format!(".part-{output:06}.vtt"));
        path.into()
    }
    pub fn partial_path(&self, output: u64) -> std::path::PathBuf {
        let mut path = self.final_path(output).into_os_string();
        path.push(".hls-partial");
        path.into()
    }
}
impl ContinuousSession {
    pub(super) fn durable_subtitles(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.recoverable_subtitles.is_some()
        }
        #[cfg(target_arch = "wasm32")]
        {
            false
        }
    }
}
