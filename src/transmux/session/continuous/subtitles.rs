//! Bounded cue admission and nonblocking wvtt interval mux.
use super::multitrack::MultiState;
use super::*;
const SCALE: u32 = 90_000;

#[derive(Debug, Clone)]
pub struct SubtitleTrack {
    pub(super) id: InputId,
    pub(super) timeline_input: InputId,
    pub(super) metadata: TrackMetadata,
}
impl SubtitleTrack {
    pub fn new(id: InputId, timeline_input: InputId, metadata: TrackMetadata) -> Self {
        Self {
            id,
            timeline_input,
            metadata,
        }
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
    fn bytes(&self) -> usize {
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
    accepted: usize,
    late: usize,
    clipped: usize,
}
impl SubtitleAcceptance {
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
                add(cue.start, shift)?;
                add(cue.end, shift)?;
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
        let mut result = SubtitleAcceptance {
            accepted: 0,
            late: 0,
            clipped: 0,
        };
        for cue in cues {
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
                self.cue_report(report, limits);
            }
            if !late {
                self.subtitles[index].queue.push(QueuedCue {
                    cue: cue.clone(),
                    mapped,
                });
                result.accepted += 1;
            }
        }
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
        for lane in &mut self.subtitles {
            if &lane.config.timeline_input != input {
                grouped.push(vec![]);
                continue;
            }
            lane.mapping = Some((generation, epoch, shift));
            lane.output = output;
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
                grouped.push(vec![]);
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
            for span in boundaries.windows(2) {
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
            if let Some(info) = self
                .tracks
                .iter_mut()
                .find(|t| t.id == lane.track && t.output == output)
            {
                info.samples += samples.len() as u64;
                info.duration = info.duration.max(end_tick as u64);
            }
            grouped.push(samples);
        }
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
        let mut grouped = vec![vec![]; self.subtitles.len()];
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
        Ok((grouped, end))
    }
    pub fn clear_subtitles(&mut self) {
        for lane in &mut self.subtitles {
            lane.queue.clear();
            lane.ended = true;
        }
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
            let mut out = s.serialize_struct("SubtitleCue", 7)?;
            out.serialize_field("generation", &self.generation.to_string())?;
            out.serialize_field("epoch", &self.epoch.to_string())?;
            out.serialize_field("start", &self.start)?;
            out.serialize_field("end", &self.end)?;
            out.serialize_field("identifier", &self.identifier)?;
            out.serialize_field("payload", &self.payload)?;
            out.serialize_field("settings", &self.settings)?;
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
            }
            let w = Wire::deserialize(d)?;
            let wide = |s: String| -> std::result::Result<u64, D::Error> {
                let n = s.parse::<u64>().map_err(D::Error::custom)?;
                if n.to_string() != s {
                    return Err(D::Error::custom("noncanonical integer"));
                }
                Ok(n)
            };
            let cue = SubtitleCue::new(
                wide(w.generation)?,
                wide(w.epoch)?,
                w.start,
                w.end,
                w.payload,
            )
            .with_identifier(w.identifier)
            .with_settings(w.settings);
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
    crate::resume::digest(&bytes)
}
impl MultiState {
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn save_subtitles(&self, out: &mut Vec<u8>) {
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
        }
    }
    pub(super) fn restore_subtitles(
        &mut self,
        r: &mut Reader<'_>,
        terminal: bool,
    ) -> ContinuousResult<()> {
        let bad = || fail(ContinuousErrorKind::ResumeCorruption);
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
            if terminal {
                lane.queue.clear();
                continue;
            }
            let mut queue = Vec::new();
            for (digest, mapped) in required {
                let index = lane
                    .queue
                    .iter()
                    .position(|q| cue_identity(&q.cue) == digest)
                    .ok_or_else(|| fail(ContinuousErrorKind::ReplayRequired))?;
                let mut cue = lane.queue.remove(index);
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
                queue.push(cue);
            }
            lane.queue = queue;
        }
        self.subtitle_history_truncated = true;
        self.subtitle_reports.clear();
        Ok(())
    }
}
