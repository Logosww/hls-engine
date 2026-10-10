//! Bounded cue admission and nonblocking wvtt interval mux.
use super::multitrack::MultiState;
use super::*;
const SCALE: u32 = 90_000;

#[derive(Debug, Clone)]
pub struct SubtitleTrack {
    pub(super) id: InputId,
    pub(super) timeline_input: InputId,
    pub(super) metadata: TrackMetadata,
    pub(super) embedded: bool,
}
impl SubtitleTrack {
    pub fn new(id: InputId, timeline_input: InputId, metadata: TrackMetadata) -> Self {
        Self {
            id,
            timeline_input,
            metadata,
            embedded: true,
        }
    }
    /// Disable embedded wvtt while retaining the same media-derived mapping.
    /// A subtitle sink must be attached before starting the session.
    pub fn with_embedded(mut self, embedded: bool) -> Self {
        self.embedded = embedded;
        self
    }
}
#[derive(Debug, Clone)]
pub struct SubtitleCue {
    generation: u64,
    epoch: u64,
    start: MediaTime,
    end: MediaTime,
    identifier: String,
    payload: String,
    settings: String,
    source: Option<[u8; 32]>,
    prepared_by: Option<Arc<()>>,
}
impl SubtitleCue {
    pub fn new(
        generation: u64,
        epoch: u64,
        start: MediaTime,
        end: MediaTime,
        payload: impl Into<String>,
    ) -> Self {
        Self {
            generation,
            epoch,
            start,
            end,
            payload: payload.into(),
            identifier: String::new(),
            settings: String::new(),
            source: None,
            prepared_by: None,
        }
    }
    pub fn with_identifier(mut self, id: impl Into<String>) -> Self {
        self.identifier = id.into();
        self
    }
    pub fn with_settings(mut self, settings: impl Into<String>) -> Self {
        self.settings = settings.into();
        self
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn start(&self) -> MediaTime {
        self.start
    }
    pub fn end(&self) -> MediaTime {
        self.end
    }
    pub fn identifier(&self) -> &str {
        &self.identifier
    }
    pub fn payload(&self) -> &str {
        &self.payload
    }
    pub fn settings(&self) -> &str {
        &self.settings
    }
    pub fn source_identity(&self) -> Option<&[u8; 32]> {
        self.source.as_ref()
    }
    /// Bind the cue to freshly prepared resource/key evidence without retaining
    /// bytes, URLs or raw keys. Stable provider versions are required for keys.
    pub fn with_resource(mut self, resource: &ClearResource) -> ContinuousResult<Self> {
        if !matches!(
            resource.container(),
            ClearContainer::WebVtt | ClearContainer::WebVttHeader
        ) || resource.request().resource().slot().generation() != self.generation
            || resource.request().resource().slot().epoch() != self.epoch
        {
            return Err(fail(ContinuousErrorKind::InvalidSubtitle));
        }
        if self.source.is_some()
            && self
                .prepared_by
                .as_ref()
                .is_none_or(|operation| !Arc::ptr_eq(operation, resource.provenance()))
        {
            return Err(fail(ContinuousErrorKind::InvalidSubtitle));
        }
        let mut identity = resource.request().segment().checkpoint_identity().to_vec();
        if let Some(previous) = self.source {
            identity.extend_from_slice(&previous);
        }
        if let Some(reference) = resource.key_reference() {
            identity.extend_from_slice(&reference.checkpoint_identity());
            match resource.key_version() {
                Some(crate::crypto::key::KeyVersion::Provider(version)) => {
                    version.put(&mut identity)
                }
                _ => return Err(fail(ContinuousErrorKind::ResumeConflict)),
            }
        }
        identity.extend_from_slice(&crate::resume::digest(resource.bytes()));
        self.source = Some(crate::resume::digest(&identity));
        self.prepared_by = Some(resource.provenance().clone());
        Ok(self)
    }
    pub(super) fn bytes(&self) -> usize {
        self.payload.len()
            + self.identifier.len()
            + self.settings.len()
            + std::mem::size_of::<QueuedCue>()
    }
    fn validate(&self) -> ContinuousResult<()> {
        if self.start.timescale == 0
            || self.end.timescale == 0
            || !cmp(self.start, self.end)?.is_lt()
            || self.payload.contains('\0')
            || self.identifier.contains(['\0', '\n', '\r'])
        {
            return Err(fail(ContinuousErrorKind::InvalidSubtitle));
        }
        // v0.10 text profile: plain UTF-8 plus standard positioning/alignment.
        // CSS, regions and markup require explicit future profiles, never silent stripping.
        if self.payload.contains(['<', '>'])
            || self.payload.starts_with("STYLE\n")
            || self.payload.starts_with("REGION\n")
        {
            return Err(fail(ContinuousErrorKind::UnsupportedSubtitleProfile));
        }
        if self
            .settings
            .chars()
            .any(|c| c.is_whitespace() && c != ' ' && c != '\t')
        {
            return Err(fail(ContinuousErrorKind::UnsupportedSubtitleProfile));
        }
        let mut seen = std::collections::HashSet::new();
        for setting in self.settings.split_ascii_whitespace() {
            let Some((key, value)) = setting.split_once(':') else {
                return Err(fail(ContinuousErrorKind::UnsupportedSubtitleProfile));
            };
            let percent = |s: &str| {
                s.strip_suffix('%').is_some_and(|s| {
                    let digits = |v: &str| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit());
                    let syntax = s
                        .split_once('.')
                        .map_or_else(|| digits(s), |(a, b)| digits(a) && digits(b));
                    syntax && s.parse::<f64>().is_ok_and(|n| (0.0..=100.0).contains(&n))
                })
            };
            let line_number = |s: &str| {
                let digits = s.strip_prefix('-').unwrap_or(s);
                !digits.is_empty()
                    && digits.bytes().all(|b| b.is_ascii_digit())
                    && s.parse::<i32>().is_ok()
            };
            let valid = match key {
                "align" => matches!(value, "start" | "center" | "end" | "left" | "right"),
                "vertical" => matches!(value, "rl" | "lr"),
                "size" => percent(value),
                "position" => value.split_once(',').map_or_else(
                    || percent(value),
                    |(p, a)| percent(p) && matches!(a, "line-left" | "center" | "line-right"),
                ),
                "line" => value.split_once(',').map_or_else(
                    || percent(value) || line_number(value),
                    |(p, a)| {
                        (percent(p) || line_number(p)) && matches!(a, "start" | "center" | "end")
                    },
                ),
                _ => false,
            };
            if !valid || !seen.insert(key) {
                return Err(fail(ContinuousErrorKind::UnsupportedSubtitleProfile));
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SubtitleDisposition {
    Written,
    Clipped,
    RejectedLate,
    RejectedRange,
}
#[derive(Debug, Clone)]
pub struct SubtitleCueReport {
    track: OutputTrackId,
    identifier: String,
    disposition: SubtitleDisposition,
    output: u64,
    start: MediaTime,
    end: MediaTime,
}
impl SubtitleCueReport {
    pub fn track_id(&self) -> OutputTrackId {
        self.track
    }
    pub fn identifier(&self) -> &str {
        &self.identifier
    }
    pub fn disposition(&self) -> SubtitleDisposition {
        self.disposition
    }
    pub fn output_index(&self) -> u64 {
        self.output
    }
    pub fn start(&self) -> MediaTime {
        self.start
    }
    pub fn end(&self) -> MediaTime {
        self.end
    }
}
#[derive(Debug, Clone, Copy)]
pub struct SubtitleAcceptance {
    first_receipt: Option<u64>,
    accepted: usize,
    late: usize,
    clipped: usize,
}
impl SubtitleAcceptance {
    /// Receipts are consecutive in submitted order, including rejected late cues.
    pub fn first_receipt(&self) -> Option<u64> {
        self.first_receipt
    }

    pub fn accepted(&self) -> usize {
        self.accepted
    }
    pub fn rejected_late(&self) -> usize {
        self.late
    }
    pub fn clipped(&self) -> usize {
        self.clipped
    }
}
struct QueuedCue {
    receipt: u64,
    cue: SubtitleCue,
    mapped: Option<(MediaTime, MediaTime)>,
}
pub(super) struct SubtitleLane {
    pub config: SubtitleTrack,
    pub track: OutputTrackId,
    queue: Vec<QueuedCue>,
    sealed: Option<MediaTime>,
    mapping: Option<(u64, u64, MediaTime)>,
    ended: bool,
    output: u64,
    origin: MediaTime,
}
impl SubtitleLane {
    pub fn new(config: SubtitleTrack, index: usize) -> Self {
        Self {
            config,
            track: OutputTrackId(65 + index as u32),
            queue: vec![],
            sealed: None,
            mapping: None,
            ended: false,
            output: 0,
            origin: zero(),
        }
    }
    pub fn end(&mut self) {
        self.ended = true;
    }
    pub fn track(&self) -> FragmentedTrack {
        let mut metadata = self.config.metadata.clone();
        metadata.group = 2;
        FragmentedTrack {
            track_id: self.track.0,
            timescale: SCALE,
            kind: crate::mp4::FragmentedTrackKind::Wvtt,
            metadata: Some(metadata),
        }
    }
}
impl MultiState {
    pub fn subtitle_usage(&self) -> (usize, usize) {
        (
            self.subtitles.iter().map(|s| s.queue.len()).sum(),
            self.subtitles
                .iter()
                .flat_map(|s| &s.queue)
                .map(|q| q.cue.bytes())
                .sum(),
        )
    }
    fn cue_report(&mut self, report: SubtitleCueReport, limits: &ContinuousLimits) {
        self.subtitle_reports.push_back(report);
        while self.subtitle_reports.len() > limits.history
            || self
                .subtitle_reports
                .iter()
                .map(|r| r.identifier.len() + std::mem::size_of::<SubtitleCueReport>())
                .sum::<usize>()
                > limits.metadata
        {
            self.subtitle_reports.pop_front();
            self.subtitle_history_truncated = true;
        }
    }
    pub fn accept_cues(
        &mut self,
        track: OutputTrackId,
        cues: &[SubtitleCue],
        limits: &ContinuousLimits,
    ) -> ContinuousResult<SubtitleAcceptance> {
        let index = self
            .subtitles
            .iter()
            .position(|s| s.track == track)
            .ok_or_else(|| fail(ContinuousErrorKind::UnknownInput))?;
        if self.subtitles[index].ended {
            return Err(fail(ContinuousErrorKind::Closed));
        }
        for cue in cues {
            cue.validate()?;
            if let Some((_, _, shift)) = self.subtitles[index]
                .mapping
                .filter(|(g, e, _)| (*g, *e) == (cue.generation, cue.epoch))
            {
                let start = add(cue.start, shift)?;
                let end = add(cue.end, shift)?;
                if self.sidecar {
                    let origin = self.subtitles[index].origin;
                    scale(sub(start, origin)?, SCALE)?;
                    scale(sub(end, origin)?, SCALE)?;
                }
            }
        }
        let (count, bytes) = self.subtitle_usage();
        let added = cues
            .iter()
            .try_fold(0usize, |n, c| n.checked_add(c.bytes()))
            .ok_or_else(|| fail(ContinuousErrorKind::BudgetExceeded))?;
        if cues.len() > limits.samples || added > limits.sample_bytes {
            return Err(fail(ContinuousErrorKind::BudgetExceeded));
        }
        if count
            .saturating_add(self.media_samples)
            .saturating_add(cues.len())
            > limits.samples
            || bytes.saturating_add(self.media_bytes).saturating_add(added) > limits.sample_bytes
        {
            return Err(fail(ContinuousErrorKind::WouldBlock));
        }
        let next_receipt = self
            .next_receipt
            .checked_add(cues.len() as u64)
            .ok_or_else(|| fail(ContinuousErrorKind::TimeOverflow))?;
        // A late disposition can be queued immediately. Reserve the worst-case
        // whole batch before mutating admission, keeping retries atomic.
        if self.sidecar {
            let pending = &self.pending_subtitles.cues;
            let pending_bytes = pending
                .iter()
                .map(CommittedSubtitleCue::bytes)
                .fold(0usize, usize::saturating_add);
            let extra = cues
                .iter()
                .map(|c| {
                    c.bytes().saturating_add(
                        std::mem::size_of::<CommittedSubtitleCue>()
                            + self.subtitles[index].config.id.as_str().len(),
                    )
                })
                .fold(0usize, usize::saturating_add);
            if pending.len().saturating_add(cues.len()) > limits.samples
                || pending_bytes.saturating_add(extra) > limits.sample_bytes
            {
                return Err(fail(ContinuousErrorKind::BudgetExceeded));
            }
        }
        let first_receipt = self.next_receipt;
        let mut result = SubtitleAcceptance {
            first_receipt: (!cues.is_empty()).then_some(first_receipt),
            accepted: 0,
            late: 0,
            clipped: 0,
        };
        for (offset, cue) in cues.iter().enumerate() {
            let receipt = first_receipt + offset as u64;
            let lane = &self.subtitles[index];
            let mut mapped = if let Some((g, e, shift)) = lane
                .mapping
                .filter(|(g, e, _)| (*g, *e) == (cue.generation, cue.epoch))
            {
                let _ = (g, e);
                Some((add(cue.start, shift)?, add(cue.end, shift)?))
            } else {
                None
            };
            let mut report = None;
            if let (Some((start, end)), Some(sealed)) = (mapped, lane.sealed) {
                if !cmp(end, sealed)?.is_gt() {
                    result.late += 1;
                    report = Some(SubtitleCueReport {
                        track,
                        identifier: cue.identifier.clone(),
                        disposition: SubtitleDisposition::RejectedLate,
                        output: lane.output,
                        start,
                        end,
                    });
                } else if cmp(start, sealed)?.is_lt() {
                    mapped = Some((sealed, end));
                    result.clipped += 1;
                    report = Some(SubtitleCueReport {
                        track,
                        identifier: cue.identifier.clone(),
                        disposition: SubtitleDisposition::Clipped,
                        output: lane.output,
                        start: sealed,
                        end,
                    });
                }
            }
            let late = report
                .as_ref()
                .is_some_and(|r| r.disposition == SubtitleDisposition::RejectedLate);
            if let Some(report) = report {
                if late && self.sidecar {
                    self.stage_subtitle(
                        CommittedSubtitleCue {
                            receipt,
                            input: self.subtitles[index].config.id.clone(),
                            track,
                            cue: cue.clone(),
                            disposition: report.disposition,
                            output: report.output,
                            start: MediaTime {
                                ticks: scale(
                                    sub(report.start, self.subtitles[index].origin)?,
                                    SCALE,
                                )?,
                                timescale: SCALE,
                            },
                            end: MediaTime {
                                ticks: scale(
                                    sub(report.end, self.subtitles[index].origin)?,
                                    SCALE,
                                )?,
                                timescale: SCALE,
                            },
                        },
                        limits,
                    )?;
                }
                self.cue_report(report, limits);
            }
            if !late {
                self.subtitles[index].queue.push(QueuedCue {
                    receipt,
                    cue: cue.clone(),
                    mapped,
                });
                result.accepted += 1;
            }
        }
        self.next_receipt = next_receipt;
        Ok(result)
    }
    /// Seal before handing bytes to a writer: admission must never race a pending write.
    #[allow(clippy::too_many_arguments)]
    pub fn render_subtitles(
        &mut self,
        input: &InputId,
        generation: u64,
        epoch: u64,
        shift: MediaTime,
        origin: MediaTime,
        until: MediaTime,
        output: u64,
        range: Option<PresentationRange>,
        limits: &ContinuousLimits,
    ) -> ContinuousResult<Vec<Vec<Mp4Sample>>> {
        // Account for queued cue payloads, pending media and all generated lanes
        // together, before copying cue payload into a sample. A single operation
        // owns this budget; adding tracks never multiplies it.
        let (queued_count, queued_bytes) = self.subtitle_usage();
        let mut used_count = queued_count.saturating_add(self.media_samples);
        let mut used_bytes = queued_bytes.saturating_add(self.media_bytes);
        let mut grouped = Vec::with_capacity(self.subtitles.len());
        let mut reports = vec![];
        let mut committed = vec![];
        let mut committed_bytes = self
            .pending_subtitles
            .cues
            .iter()
            .map(CommittedSubtitleCue::bytes)
            .fold(0usize, usize::saturating_add);
        for lane in &mut self.subtitles {
            if &lane.config.timeline_input != input {
                if lane.config.embedded {
                    grouped.push(vec![]);
                }
                continue;
            }
            lane.mapping = Some((generation, epoch, shift));
            lane.output = output;
            lane.origin = origin;
            let start = max(lane.sealed.unwrap_or(origin), origin)?;
            let until = if let Some(range) = range {
                min(until, range.end())?
            } else {
                until
            };
            for q in &mut lane.queue {
                if q.mapped.is_none() && (q.cue.generation, q.cue.epoch) == (generation, epoch) {
                    q.mapped = Some((add(q.cue.start, shift)?, add(q.cue.end, shift)?));
                }
            }
            let start_tick = scale(sub(start, origin)?, SCALE)?;
            let end_tick = scale(sub(until, origin)?, SCALE)?;
            if end_tick <= start_tick {
                if lane.config.embedded {
                    grouped.push(vec![]);
                }
                continue;
            }
            let mut boundaries = vec![start_tick, end_tick];
            let mut events = vec![];
            for (index, q) in lane.queue.iter().enumerate() {
                if let Some((a, b)) = q.mapped {
                    let a = scale(sub(a, origin)?, SCALE)?.max(start_tick);
                    let b = scale(sub(b, origin)?, SCALE)?.min(end_tick);
                    if a < b {
                        boundaries.extend([a, b]);
                        events.extend([(a, true, index), (b, false, index)]);
                    }
                }
            }
            events.sort_unstable();
            let mut cursor = 0;
            let mut active = std::collections::BTreeSet::new();
            boundaries.sort_unstable();
            boundaries.dedup();
            if boundaries.len() > limits.samples.saturating_add(1) {
                return Err(fail(ContinuousErrorKind::BudgetExceeded));
            }
            let mut samples = vec![];
            for span in boundaries.windows(2).filter(|_| lane.config.embedded) {
                while cursor < events.len() && events[cursor].0 <= span[0] {
                    let (_, insert, index) = events[cursor];
                    if insert {
                        active.insert(index);
                    } else {
                        active.remove(&index);
                    }
                    cursor += 1;
                }
                let mut size = 0usize;
                for &index in &active {
                    let cue = &lane.queue[index].cue;
                    size = size.saturating_add(16 + cue.payload.len());
                    if !cue.identifier.is_empty() {
                        size = size.saturating_add(8 + cue.identifier.len());
                    }
                    if !cue.settings.is_empty() {
                        size = size.saturating_add(8 + cue.settings.len());
                    }
                }
                used_count = used_count.saturating_add(1);
                used_bytes = used_bytes.saturating_add(size.max(8));
                if used_count > limits.samples || used_bytes > limits.sample_bytes {
                    return Err(fail(ContinuousErrorKind::BudgetExceeded));
                }
                let mut payload = Vec::with_capacity(size.max(8));
                for &index in &active {
                    let q = &lane.queue[index];
                    let mut cue = vec![];
                    if !q.cue.identifier.is_empty() {
                        box_bytes(&mut cue, b"iden", q.cue.identifier.as_bytes())?;
                    }
                    if !q.cue.settings.is_empty() {
                        box_bytes(&mut cue, b"sttg", q.cue.settings.as_bytes())?;
                    }
                    box_bytes(&mut cue, b"payl", q.cue.payload.as_bytes())?;
                    box_bytes(&mut payload, b"vttc", &cue)?;
                }
                if payload.is_empty() {
                    box_bytes(&mut payload, b"vtte", &[])?;
                }
                samples.push(Mp4Sample {
                    data: payload,
                    source: None,
                    dts: u64::try_from(span[0])
                        .map_err(|_| fail(ContinuousErrorKind::TimeOverflow))?,
                    pts: span[0],
                    duration: u32::try_from(span[1] - span[0])
                        .map_err(|_| fail(ContinuousErrorKind::TimeOverflow))?,
                    is_key: true,
                    offset: 0,
                });
            }
            for q in &lane.queue {
                if let Some((a, b)) = q.mapped {
                    if self.sidecar && cmp(a, until)?.is_lt() {
                        let late = cmp(b, start)?.is_le();
                        let clipped = cmp(a, start)?.is_lt() || cmp(b, until)?.is_gt();
                        let a = if late { a } else { max(a, start)? };
                        let b = if late { b } else { min(b, until)? };
                        let a = MediaTime {
                            ticks: scale(sub(a, origin)?, SCALE)?,
                            timescale: SCALE,
                        };
                        let b = MediaTime {
                            ticks: scale(sub(b, origin)?, SCALE)?,
                            timescale: SCALE,
                        };
                        if late || a.ticks < b.ticks {
                            committed_bytes = committed_bytes
                                .saturating_add(q.cue.bytes())
                                .saturating_add(std::mem::size_of::<CommittedSubtitleCue>())
                                .saturating_add(lane.config.id.as_str().len());
                            if self.pending_subtitles.cues.len() + committed.len() >= limits.samples
                                || committed_bytes > limits.sample_bytes
                            {
                                return Err(fail(ContinuousErrorKind::BudgetExceeded));
                            }
                            committed.push(CommittedSubtitleCue {
                                receipt: q.receipt,
                                input: lane.config.id.clone(),
                                track: lane.track,
                                cue: q.cue.clone(),
                                output,
                                start: a,
                                end: b,
                                disposition: if late {
                                    SubtitleDisposition::RejectedLate
                                } else if clipped {
                                    SubtitleDisposition::Clipped
                                } else {
                                    SubtitleDisposition::Written
                                },
                            });
                        }
                    }
                    if cmp(b, start)?.is_le() {
                        reports.push(SubtitleCueReport {
                            track: lane.track,
                            identifier: q.cue.identifier.clone(),
                            disposition: SubtitleDisposition::RejectedLate,
                            output,
                            start: a,
                            end: b,
                        });
                    } else if cmp(a, until)?.is_lt() {
                        reports.push(SubtitleCueReport {
                            track: lane.track,
                            identifier: q.cue.identifier.clone(),
                            disposition: if cmp(a, start)?.is_lt() || cmp(b, until)?.is_gt() {
                                SubtitleDisposition::Clipped
                            } else {
                                SubtitleDisposition::Written
                            },
                            output,
                            start: max(a, start)?,
                            end: min(b, until)?,
                        });
                    }
                }
            }
            lane.queue.retain(|q| {
                q.mapped
                    .is_none_or(|(_, b)| cmp(b, until).is_ok_and(|v| v.is_gt()))
            });
            lane.sealed = Some(until);
            if self.sidecar {
                self.pending_subtitles.frontiers.push(SubtitleFrontier {
                    input: lane.config.id.clone(),
                    track: lane.track,
                    generation,
                    epoch,
                    output,
                    end: MediaTime {
                        ticks: end_tick,
                        timescale: SCALE,
                    },
                });
            }
            if let Some(info) = self
                .tracks
                .iter_mut()
                .find(|t| t.id == lane.track && t.output == output)
            {
                info.samples += samples.len() as u64;
                info.duration = info.duration.max(end_tick as u64);
            }
            if lane.config.embedded {
                grouped.push(samples);
            }
        }
        self.pending_subtitles.cues.extend(committed);
        for report in reports {
            self.cue_report(report, limits);
        }
        Ok(grouped)
    }
    pub fn drain_subtitles(
        &mut self,
        origin: MediaTime,
        output: u64,
        range: Option<PresentationRange>,
        limits: &ContinuousLimits,
    ) -> ContinuousResult<(Vec<Vec<Mp4Sample>>, MediaTime)> {
        self.finish_subtitles()?;
        let mut jobs: Vec<(InputId, u64, u64, MediaTime, MediaTime)> = vec![];
        for lane in &self.subtitles {
            if let Some((g, e, shift)) = lane.mapping {
                for q in &lane.queue {
                    if let Some((_, end)) = q.mapped {
                        if let Some(job) = jobs
                            .iter_mut()
                            .find(|(i, _, _, _, _)| *i == lane.config.timeline_input)
                        {
                            job.4 = max(job.4, end)?;
                        } else {
                            jobs.push((lane.config.timeline_input.clone(), g, e, shift, end));
                        }
                    }
                }
            }
        }
        let mut grouped = vec![vec![]; self.subtitles.iter().filter(|s| s.config.embedded).count()];
        let mut end = origin;
        for (input, g, e, shift, until) in jobs {
            let until = range.map_or(Ok(until), |r| min(until, r.end()))?;
            end = max(end, until)?;
            for (dest, samples) in grouped.iter_mut().zip(
                self.render_subtitles(&input, g, e, shift, origin, until, output, range, limits)?,
            ) {
                dest.extend(samples);
            }
        }
        if let Some(range) = range {
            let mut rejected = Vec::new();
            for lane in &self.subtitles {
                for q in &lane.queue {
                    if let Some((a, b)) = q.mapped
                        && cmp(a, range.end())?.is_ge()
                        && self.sidecar
                    {
                        rejected.push(CommittedSubtitleCue {
                            receipt: q.receipt,
                            input: lane.config.id.clone(),
                            track: lane.track,
                            cue: q.cue.clone(),
                            disposition: SubtitleDisposition::RejectedRange,
                            output,
                            start: MediaTime {
                                ticks: scale(sub(a, origin)?, SCALE)?,
                                timescale: SCALE,
                            },
                            end: MediaTime {
                                ticks: scale(sub(b, origin)?, SCALE)?,
                                timescale: SCALE,
                            },
                        });
                    }
                }
            }
            for cue in rejected {
                self.stage_subtitle(cue, limits)?;
            }
        }
        Ok((grouped, end))
    }
    pub fn clear_subtitles(&mut self) {
        for lane in &mut self.subtitles {
            lane.queue.clear();
            lane.ended = true;
        }
        self.pending_subtitles = SubtitleCommit::default();
        self.media_samples = 0;
        self.media_bytes = 0;
    }
    pub fn finish_subtitles(&self) -> ContinuousResult<()> {
        if self
            .subtitles
            .iter()
            .flat_map(|s| &s.queue)
            .any(|q| q.mapped.is_none())
        {
            return Err(fail(ContinuousErrorKind::MissingSubtitleMapping));
        }
        Ok(())
    }
}
fn box_bytes(out: &mut Vec<u8>, kind: &[u8; 4], payload: &[u8]) -> ContinuousResult<()> {
    let size = u32::try_from(
        payload
            .len()
            .checked_add(8)
            .ok_or_else(|| fail(ContinuousErrorKind::BudgetExceeded))?,
    )
    .map_err(|_| fail(ContinuousErrorKind::BudgetExceeded))?;
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    Ok(())
}

#[cfg(feature = "serde")]
mod wire {
    use super::*;
    use serde::{
        Deserialize, Deserializer, Serialize, Serializer, de::Error as _, ser::SerializeStruct,
    };
    impl Serialize for SubtitleCue {
        fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
            let mut out =
                s.serialize_struct("SubtitleCue", if self.source.is_some() { 8 } else { 7 })?;
            out.serialize_field("generation", &self.generation.to_string())?;
            out.serialize_field("epoch", &self.epoch.to_string())?;
            out.serialize_field("start", &self.start)?;
            out.serialize_field("end", &self.end)?;
            out.serialize_field("identifier", &self.identifier)?;
            out.serialize_field("payload", &self.payload)?;
            out.serialize_field("settings", &self.settings)?;
            if let Some(source) = &self.source {
                out.serialize_field("source", source)?;
            }
            out.end()
        }
    }
    impl<'de> Deserialize<'de> for SubtitleCue {
        fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Wire {
                generation: String,
                epoch: String,
                start: MediaTime,
                end: MediaTime,
                identifier: String,
                payload: String,
                settings: String,
                #[serde(default)]
                source: Option<[u8; 32]>,
            }
            let w = Wire::deserialize(d)?;
            let wide = |s: String| -> std::result::Result<u64, D::Error> {
                let n = s.parse::<u64>().map_err(D::Error::custom)?;
                if n.to_string() != s {
                    return Err(D::Error::custom("noncanonical integer"));
                }
                Ok(n)
            };
            let mut cue = SubtitleCue::new(
                wide(w.generation)?,
                wide(w.epoch)?,
                w.start,
                w.end,
                w.payload,
            )
            .with_identifier(w.identifier)
            .with_settings(w.settings);
            cue.source = w.source;
            cue.validate().map_err(D::Error::custom)?;
            Ok(cue)
        }
    }
}

use crate::state_codec::{Reader, StateCodec};
type CueReplayIdentity = ([u8; 32], Option<(MediaTime, MediaTime)>);
fn cue_identity(cue: &SubtitleCue) -> [u8; 32] {
    let mut bytes = Vec::new();
    cue.generation.put(&mut bytes);
    cue.epoch.put(&mut bytes);
    cue.start.put(&mut bytes);
    cue.end.put(&mut bytes);
    cue.identifier.put(&mut bytes);
    cue.payload.put(&mut bytes);
    cue.settings.put(&mut bytes);
    if let Some(source) = cue.source {
        source.put(&mut bytes);
    }
    crate::resume::digest(&bytes)
}
impl MultiState {
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn save_subtitles(&self, out: &mut Vec<u8>, durable: bool) {
        if durable {
            self.next_receipt.put(out);
        }
        self.subtitles.len().put(out);
        for lane in &self.subtitles {
            lane.track.put(out);
            lane.sealed.put(out);
            lane.mapping.put(out);
            lane.ended.put(out);
            lane.output.put(out);
            lane.queue
                .iter()
                .map(|q| (cue_identity(&q.cue), q.mapped))
                .collect::<Vec<_>>()
                .put(out);
            if durable {
                lane.queue
                    .iter()
                    .map(|q| q.receipt)
                    .collect::<Vec<_>>()
                    .put(out);
            }
        }
    }
    pub(super) fn restore_subtitles(
        &mut self,
        r: &mut Reader<'_>,
        terminal: bool,
        operation: &Arc<()>,
        receipt_floor: Option<u64>,
    ) -> ContinuousResult<()> {
        let durable = receipt_floor.is_some();
        let bad = || fail(ContinuousErrorKind::ResumeCorruption);
        let next_receipt = if durable {
            u64::get(r).map_err(|_| bad())?
        } else {
            0
        };
        if receipt_floor.is_some_and(|floor| floor != next_receipt) {
            return Err(bad());
        }
        self.next_receipt = self.next_receipt.max(next_receipt);
        let mut seen_receipts = std::collections::BTreeSet::new();
        let n = usize::get(r).map_err(|_| bad())?;
        if n != self.subtitles.len() {
            return Err(fail(ContinuousErrorKind::ResumeConflict));
        }
        for lane in &mut self.subtitles {
            if <OutputTrackId as StateCodec>::get(r).map_err(|_| bad())? != lane.track {
                return Err(bad());
            }
            lane.sealed = StateCodec::get(r).map_err(|_| bad())?;
            lane.mapping = StateCodec::get(r).map_err(|_| bad())?;
            lane.ended |= bool::get(r).map_err(|_| bad())?;
            lane.output = u64::get(r).map_err(|_| bad())?;
            let required: Vec<CueReplayIdentity> = StateCodec::get(r).map_err(|_| bad())?;
            let receipts: Vec<u64> = if durable {
                StateCodec::get(r).map_err(|_| bad())?
            } else {
                vec![]
            };
            if durable
                && (receipts.len() != required.len()
                    || receipts
                        .iter()
                        .any(|v| *v >= next_receipt || !seen_receipts.insert(*v)))
            {
                return Err(bad());
            }
            if terminal {
                lane.queue.clear();
                continue;
            }
            let mut queue = Vec::new();
            for (number, (digest, mapped)) in required.into_iter().enumerate() {
                let index = lane
                    .queue
                    .iter()
                    .position(|q| cue_identity(&q.cue) == digest)
                    .ok_or_else(|| fail(ContinuousErrorKind::ReplayRequired))?;
                let mut cue = lane.queue.remove(index);
                if cue.cue.source.is_some()
                    && cue
                        .cue
                        .prepared_by
                        .as_ref()
                        .is_none_or(|prepared| !Arc::ptr_eq(prepared, operation))
                {
                    return Err(fail(ContinuousErrorKind::ReplayRequired));
                }
                if durable {
                    cue.receipt = receipts[number];
                }
                cue.mapped = mapped;
                queue.push(cue);
            }
            // Replayed cues already wholly sealed must not re-enter the output.
            for mut cue in lane.queue.drain(..) {
                if lane
                    .mapping
                    .is_some_and(|(g, e, _)| (cue.cue.generation, cue.cue.epoch) < (g, e))
                {
                    // A still-active cue from an earlier epoch is present in
                    // `required`; all other earlier cues have already drained.
                    continue;
                }
                if let Some((g, e, shift)) = lane.mapping
                    && (cue.cue.generation, cue.cue.epoch) == (g, e)
                {
                    let start = add(cue.cue.start, shift)?;
                    let end = add(cue.cue.end, shift)?;
                    if let Some(sealed) = lane.sealed {
                        if !cmp(end, sealed)?.is_gt() {
                            continue;
                        }
                        cue.mapped = Some((max(start, sealed)?, end));
                    } else {
                        cue.mapped = Some((start, end));
                    }
                }
                if cue.cue.source.is_some()
                    && cue
                        .cue
                        .prepared_by
                        .as_ref()
                        .is_none_or(|prepared| !Arc::ptr_eq(prepared, operation))
                {
                    return Err(fail(ContinuousErrorKind::ReplayRequired));
                }
                queue.push(cue);
            }
            lane.queue = queue;
        }
        self.subtitle_history_truncated = true;
        self.subtitle_reports.clear();
        Ok(())
    }
}
