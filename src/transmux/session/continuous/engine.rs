use super::super::timeline::engine::{independent, parse_pdt};
use super::*;
use crate::playlist::reconcile::metadata_bytes;

pub(super) struct Batch {
    pub descriptor: SegmentDescriptor,
    pub data: DemuxOutput,
    pub shift: MediaTime,
    pub new_epoch: bool,
    pub changed: bool,
}
impl Batch {
    fn first(&self) -> ContinuousResult<MediaTime> {
        if self.descriptor.gap() {
            return Ok(self.shift);
        }
        self.data
            .packets
            .iter()
            .map(|p| add(packet_time(p, 0), self.shift))
            .try_fold(None, |old, t| {
                let t = t?;
                Ok(Some(if let Some(old) = old { min(old, t)? } else { t }))
            })?
            .ok_or_else(|| fail(ContinuousErrorKind::EmptyInput))
    }
}
struct Lane {
    pending: VecDeque<Batch>,
    map: Option<EncodedResource>,
    config: Option<DemuxOutput>,
    epoch: Option<(u64, u64)>,
    shift: MediaTime,
    clock: [Option<i128>; 2],
    last: [Option<MediaTime>; 2],
    delta: Option<MediaTime>,
    end: MediaTime,
    gap: MediaTime,
    split_pending: bool,
    mapping_dirty: bool,
}
impl Lane {
    fn new() -> Self {
        Self {
            pending: VecDeque::new(),
            map: None,
            config: None,
            epoch: None,
            shift: zero(),
            clock: [None, None],
            last: [None, None],
            delta: None,
            end: zero(),
            gap: zero(),
            split_pending: false,
            mapping_dirty: false,
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct TrackKey {
    input: usize,
    kind: OutputTrackKind,
}
pub(super) struct Engine {
    lanes: Vec<Lane>,
    pub tracks: Vec<FragmentedTrack>,
    track_keys: Vec<TrackKey>,
    pub offsets: Vec<u64>,
    pub origin: MediaTime,
    pub common: MediaTime,
    pub reports: Vec<crate::TrackInfo>,
    pub ends: Vec<Option<u64>>,
    pub bytes: u64,
    pub part_bytes: u64,
    pub segments: usize,
    pub duration: MediaTime,
    pub output: u64,
    pub mappings: VecDeque<ContinuousMapping>,
    pub outputs: VecDeque<ContinuousOutputReport>,
    pub truncated: bool,
    pub gaps: u64,
    wall: Option<MediaTime>,
    pub selected_start: Option<MediaTime>,
    selected_end: Option<MediaTime>,
    common_gap: Option<(Vec<usize>, MediaTime, MediaTime)>,
}
impl ContinuousSession {
    async fn descriptor(&self, input: usize) -> ContinuousResult<Option<SegmentDescriptor>> {
        let mut wake = self.shared.signal.changed.subscribe();
        let mut timeout: Option<Pin<Box<dyn Future<Output = ()> + '_>>> = None;
        loop {
            self.check()?;
            let paused = self.shared.inner.lock().unwrap().paused;
            if paused {
                self.pause_boundary().await?;
            }
            let lagging = {
                let mut state = self.shared.inner.lock().unwrap();
                let lane = &mut state.lanes[input];
                if let Some(descriptor) = lane.queue.pop_front() {
                    state.queued -= 1;
                    state.metadata -= metadata_bytes(&descriptor);
                    drop(state);
                    self.shared.signal.wake();
                    return Ok(Some(descriptor));
                }
                if lane.ended && (lane.restart.is_none() || lane.outstanding > 0) {
                    return Ok(None);
                }
                state
                    .lanes
                    .iter()
                    .enumerate()
                    .any(|(i, l)| i != input && l.outstanding > 0)
            };
            if lagging && timeout.is_none() {
                if let Some(waiter) = &self.options.waiter {
                    timeout = Some(waiter.wait(self.options.timeout));
                }
            } else if !lagging {
                timeout = None;
            }
            tokio::select! { biased;
                _ = self.shared.signal.cancelled() => return Err(fail(ContinuousErrorKind::Cancelled)),
                _ = async {if let Some(deadline)=&mut timeout {deadline.await;}else{std::future::pending::<()>().await;}} => return Err(fail(ContinuousErrorKind::SkewTimeout)),
                _ = wake.changed() => {}
            }
        }
    }
    async fn load(
        &self,
        input: usize,
        map: &mut Option<EncodedResource>,
        descriptor: &SegmentDescriptor,
    ) -> ContinuousResult<DemuxOutput> {
        let result = self
            .wait(async {
                let source = self.sources[input].0.clone();
                if descriptor.map().is_some() {
                    let request = ResourceRequest::from_descriptor(descriptor.clone(), true)
                        .map_err(resource_error)?;
                    self.resources
                        .read_encoded_map(source.clone(), request, map)
                        .await
                        .map_err(resource_error)?;
                } else {
                    *map = None;
                }
                let request = ResourceRequest::from_descriptor(descriptor.clone(), false)
                    .map_err(resource_error)?
                    .with_packed(self.multi.is_some());
                let bytes = self
                    .resources
                    .read_encoded(source, request.clone())
                    .await
                    .map_err(resource_error)?;
                self.shared.inner.lock().unwrap().lanes[input]
                    .progress
                    .downloaded += 1;
                let mut data = crate::crypto::sample::demux_selected(
                    &self.resources,
                    &request,
                    map.as_ref().map(|m| m.bytes()),
                    bytes.bytes(),
                    &|| {
                        if self.shared.signal.is_cancelled() {
                            Err(Error::Cancelled)
                        } else {
                            Ok(())
                        }
                    },
                    self.multi.is_some(),
                )
                .await
                .map_err(|e| {
                    let kind = if e.kind() == crate::crypto::sample::SampleErrorKind::Cancelled {
                        ContinuousErrorKind::Cancelled
                    } else {
                        ContinuousErrorKind::Media
                    };
                    ContinuousError {
                        sample: Some(Box::new(e)),
                        ..fail(kind)
                    }
                })?;
                self.resources
                    .sample_decrypted(&request, bytes.bytes().len() as u64)
                    .map_err(resource_error)?;
                self.shared.inner.lock().unwrap().lanes[input]
                    .progress
                    .decrypted += 1;
                data.packets.sort_by_key(|p| match p.kind {
                    StreamKind::Avc => 0,
                    StreamKind::Hevc => 1,
                    StreamKind::Aac => 2,
                });
                if input > 0 || !self.keep_embedded {
                    select_track(
                        &mut data,
                        if input == 0 {
                            InputRole::Primary
                        } else {
                            InputRole::Audio
                        },
                    );
                }
                Ok(data)
            })
            .await;
        result.map_err(|mut e| {
            e.slot = Some(descriptor.slot().clone());
            e
        })
    }
    pub(super) fn commit(
        &self,
        input: usize,
        descriptor: &SegmentDescriptor,
        bytes: u64,
    ) -> ContinuousResult<()> {
        self.check()?;
        let progress = {
            let mut state = self.shared.inner.lock().unwrap();
            let lane = &mut state.lanes[input];
            lane.outstanding -= 1;
            lane.progress.committed += 1;
            lane.progress.watermark = Some(descriptor.slot().clone());
            lane.progress.clone()
        };
        self.shared.signal.wake();
        self.emit(ContinuousEvent::Committed {
            input: progress,
            bytes,
        })
    }
}
impl Engine {
    pub async fn prepare(session: &ContinuousSession) -> ContinuousResult<Self> {
        let mut engine = Self {
            lanes: (0..session.sources.len()).map(|_| Lane::new()).collect(),
            tracks: vec![],
            track_keys: vec![],
            offsets: vec![],
            origin: zero(),
            common: zero(),
            reports: vec![],
            ends: vec![],
            bytes: 0,
            part_bytes: 0,
            segments: 0,
            duration: zero(),
            output: 0,
            mappings: VecDeque::new(),
            outputs: VecDeque::new(),
            truncated: false,
            gaps: 0,
            wall: None,
            selected_start: None,
            selected_end: None,
            common_gap: None,
        };
        let mut raw = Vec::new();
        for input in 0..engine.lanes.len() {
            let mut found = None;
            for _ in 0..session.options.limits.probe {
                let descriptor = session
                    .descriptor(input)
                    .await?
                    .ok_or_else(|| fail(ContinuousErrorKind::EmptyInput))?;
                if descriptor.gap() {
                    return Err(at(ContinuousErrorKind::MissingRandomAccess, &descriptor));
                }
                let mut data = session
                    .load(input, &mut engine.lanes[input].map, &descriptor)
                    .await?;
                merge_probe_config(&mut engine.lanes[input].config, &mut data)
                    .map_err(media_error)?;
                if build_fragmented_tracks(&data).is_ok() && !data.packets.is_empty() {
                    found = Some((descriptor, data));
                    break;
                }
                if !data.packets.is_empty() {
                    return Err(at(ContinuousErrorKind::Media, &descriptor));
                }
                session.commit(input, &descriptor, 0)?;
            }
            raw.push(found.ok_or_else(|| fail(ContinuousErrorKind::EmptyInput))?);
            let count = raw.iter().map(|(_, d)| d.packets.len()).sum::<usize>();
            let bytes = raw
                .iter()
                .flat_map(|(_, d)| &d.packets)
                .map(|p| p.data.len())
                .sum::<usize>();
            let maps = if session.multi.is_some() {
                engine
                    .lanes
                    .iter()
                    .filter_map(|l| l.map.as_ref())
                    .map(|m| m.bytes().len())
                    .sum::<usize>()
            } else {
                0
            };
            if count > session.options.limits.samples
                || bytes.saturating_add(maps) > session.options.limits.sample_bytes
            {
                return Err(fail(ContinuousErrorKind::BudgetExceeded));
            }
        }
        // Match TS modulo clocks against authoritative fMP4 time when available.
        let anchor = raw
            .iter()
            .filter(|(_, d)| d.packed_anchor.is_none())
            .flat_map(|(_, d)| &d.packets)
            .find_map(|p| {
                p.timing.map(|t| MediaTime {
                    ticks: t.dts,
                    timescale: t.timescale,
                })
            })
            .unwrap_or_else(|| packet_time(&raw[0].1.packets[0], 0));
        let anchor90 = scale(anchor, 90_000)?;
        for (_, data) in &mut raw {
            unwrap_packed(data, anchor90)?;
            let mut clocks = [None, None];
            for p in &mut data.packets {
                if p.timing.is_none() {
                    let lane = usize::from(matches!(p.kind, StreamKind::Aac));
                    let original = i128::from(p.dts_90k);
                    let dts = unwrap_near(original, clocks[lane].unwrap_or(anchor90))
                        .map_err(media_error)?;
                    let cts = unwrap_near(p.pts_90k.rem_euclid(1i128 << 33), original)
                        .map_err(media_error)?
                        - original;
                    p.dts_90k = u64::try_from(dts)
                        .map_err(|_| fail(ContinuousErrorKind::TimelineAmbiguous))?;
                    p.pts_90k = dts + cts;
                    clocks[lane] = Some(dts);
                }
            }
        }
        let mut common = None;
        for (_, data) in &raw {
            for audio in [false, true] {
                if let Some(p) = data
                    .packets
                    .iter()
                    .find(|p| matches!(p.kind, StreamKind::Aac) == audio)
                {
                    let t = pts(p);
                    common = Some(if let Some(old) = common {
                        min(old, t)?
                    } else {
                        t
                    });
                }
            }
        }
        engine.common = common.ok_or_else(|| fail(ContinuousErrorKind::EmptyInput))?;
        for (input, (descriptor, data)) in raw.into_iter().enumerate() {
            engine.push(session, input, descriptor, data)?;
        }
        engine.refresh_tracks(session)?;
        engine.budget(session)?;
        Ok(engine)
    }
    fn refresh_tracks(&mut self, session: &ContinuousSession) -> ContinuousResult<()> {
        self.tracks.clear();
        self.track_keys.clear();
        self.offsets.clear();
        let mut origin = None;
        for lane in &self.lanes {
            if let Some(batch) = lane.pending.front() {
                let first = batch.first()?;
                origin = Some(if let Some(old) = origin {
                    min(old, first)?
                } else {
                    first
                });
            }
        }
        self.origin = origin.ok_or_else(|| fail(ContinuousErrorKind::EmptyInput))?;
        for (input, lane) in self.lanes.iter().enumerate() {
            let config = lane
                .pending
                .front()
                .map(|b| &b.data)
                .or(lane.config.as_ref())
                .ok_or_else(|| fail(ContinuousErrorKind::EmptyInput))?;
            for mut track in build_fragmented_tracks(config).map_err(media_error)? {
                track.track_id = self.tracks.len() as u32 + 1;
                let audio = matches!(track.kind, crate::mp4::FragmentedTrackKind::Audio { .. });
                if let Some(state) = &session.multi {
                    let state = state.lock().unwrap();
                    track.track_id = if input == 0 {
                        if audio { 2 } else { 1 }
                    } else {
                        input as u32 + 2
                    };
                    let mut metadata = if audio {
                        state.metadata[input].clone()
                    } else {
                        TrackMetadata::new("und", "Video")
                    };
                    metadata.group = if audio { 1 } else { 0 };
                    if !audio {
                        metadata.default = true;
                    }
                    track.metadata = Some(metadata);
                }
                let offset = if let Some(batch) = lane.pending.front() {
                    let first = batch
                        .data
                        .packets
                        .iter()
                        .find(|p| matches!(p.kind, StreamKind::Aac) == audio)
                        .ok_or_else(|| fail(ContinuousErrorKind::EmptyInput))?;
                    if !audio && !independent(first).map_err(timeline_error)? {
                        return Err(at(
                            ContinuousErrorKind::MissingRandomAccess,
                            &batch.descriptor,
                        ));
                    }
                    scale(
                        sub(add(packet_time(first, 0), batch.shift)?, self.origin)?,
                        track.timescale,
                    )?
                } else {
                    0
                };
                self.offsets.push(
                    u64::try_from(offset)
                        .map_err(|_| fail(ContinuousErrorKind::TimelineAmbiguous))?,
                );
                self.track_keys.push(TrackKey {
                    input,
                    kind: if audio {
                        OutputTrackKind::Audio
                    } else {
                        OutputTrackKind::Video
                    },
                });
                self.tracks.push(track);
            }
        }
        if self.tracks.is_empty() {
            return Err(fail(ContinuousErrorKind::EmptyInput));
        }
        if let Some(state) = &session.multi {
            let mut state = state.lock().unwrap();
            let audio_default = self
                .tracks
                .iter()
                .filter(|t| matches!(t.kind, crate::mp4::FragmentedTrackKind::Audio { .. }))
                .any(|t| t.metadata.as_ref().is_some_and(|m| m.default));
            if !audio_default
                && let Some(track) = self
                    .tracks
                    .iter_mut()
                    .find(|t| matches!(t.kind, crate::mp4::FragmentedTrackKind::Audio { .. }))
            {
                track.metadata.as_mut().unwrap().default = true;
            }
            for (track, key) in self.tracks.iter().zip(&self.track_keys) {
                if let Some(old) = state
                    .tracks
                    .iter_mut()
                    .find(|t| t.id.0 == track.track_id && t.output == self.output)
                {
                    old.timescale = track.timescale;
                } else {
                    let input_id = state.input_ids[key.input].clone();
                    state.tracks.push(OutputTrackInfo {
                        id: OutputTrackId(track.track_id),
                        output: self.output,
                        input: input_id,
                        kind: key.kind,
                        codec: OutputTrackCodec::of(track),
                        metadata: track.metadata.clone().unwrap(),
                        timescale: track.timescale,
                        duration: 0,
                        samples: 0,
                    });
                }
            }
        }
        if let Some(state) = &session.multi {
            state
                .lock()
                .unwrap()
                .trim_tracks(self.output, &session.options.limits);
        }
        self.reports = self.tracks.iter().map(|t| track_report(t, 0, 0)).collect();
        self.ends = vec![None; self.tracks.len()];
        self.part_bytes = 0;
        self.segments = 0;
        self.budget(session)
    }
    fn push(
        &mut self,
        session: &ContinuousSession,
        input: usize,
        descriptor: SegmentDescriptor,
        mut data: DemuxOutput,
    ) -> ContinuousResult<()> {
        let lane = &mut self.lanes[input];
        let epoch = (descriptor.slot().generation(), descriptor.slot().epoch());
        let new_epoch = lane.epoch != Some(epoch);
        let previous_epoch = lane.epoch;
        if new_epoch {
            lane.clock = [None, None];
            lane.last = [None, None];
            lane.delta = None;
        }
        // Carry TS configuration only inside a declared epoch; a new init is authoritative.
        if !new_epoch
            && descriptor.map().is_none()
            && let Some(old) = &lane.config
        {
            data.sps = data.sps.or_else(|| old.sps.clone());
            data.pps = data.pps.or_else(|| old.pps.clone());
            data.vps = data.vps.or_else(|| old.vps.clone());
            data.width = data.width.or(old.width);
            data.height = data.height.or(old.height);
        }
        let config_changed = check_media_config(&mut lane.config, &data).is_err();
        let changed = config_changed || lane.split_pending;
        lane.split_pending = false;
        if config_changed {
            if session.options.changes == TimelineChangePolicy::Fail {
                return Err(at(ContinuousErrorKind::ConfigurationChanged, &descriptor));
            }
            lane.config = None;
            check_media_config(&mut lane.config, &data).map_err(media_error)?;
        }
        if data.packets.is_empty() {
            return Err(at(ContinuousErrorKind::EmptyInput, &descriptor));
        }
        if let Some(anchor) = data.packed_anchor {
            let reference = lane.clock[1].unwrap_or(if previous_epoch.is_none() {
                scale(self.common, 90_000)?
            } else {
                i128::from(anchor)
            });
            unwrap_packed(&mut data, reference)?;
            if let Some(last) = data.packets.last() {
                lane.clock[1] = Some(scale(packet_time(last, 0), 90_000)?);
            }
        }
        let mut previous_packet = [None, None];
        for index in 0..data.packets.len() {
            let p = &mut data.packets[index];
            let audio = matches!(p.kind, StreamKind::Aac);
            let kind = usize::from(audio);
            if p.timing.is_none() {
                let raw = i128::from(p.dts_90k);
                let reference = lane.clock[kind].unwrap_or(if previous_epoch.is_none() {
                    scale(self.common, 90_000)?
                } else {
                    raw
                });
                let dts = unwrap_near(raw, reference).map_err(media_error)?;
                let cts =
                    unwrap_near(p.pts_90k.rem_euclid(1i128 << 33), raw).map_err(media_error)? - raw;
                lane.clock[kind] = Some(dts);
                p.timing = Some(PacketTiming {
                    edit_offset: 0,
                    timescale: 90_000,
                    dts,
                    pts: dts + cts,
                    duration: if audio {
                        u32::try_from(
                            p.duration
                                .checked_mul(90_000)
                                .ok_or_else(|| fail(ContinuousErrorKind::TimeOverflow))?
                                / u64::from(data.sample_rate.unwrap_or(48_000)),
                        )
                        .map_err(|_| fail(ContinuousErrorKind::TimeOverflow))?
                    } else {
                        0
                    },
                });
            }
            let t = packet_time(p, 0);
            if let Some(last) = lane.last[kind] {
                let delta = sub(t, last)?;
                if delta.ticks <= 0
                    || (descriptor.map().is_none() && scale(delta, 90_000)? >= 1i128 << 32)
                {
                    return Err(at(ContinuousErrorKind::TimelineAmbiguous, &descriptor));
                }
                if !audio {
                    lane.delta = Some(delta);
                    if let Some(previous) = previous_packet[kind] {
                        let prev: &mut EncodedPacket = &mut data.packets[previous];
                        if let Some(timing) = &mut prev.timing
                            && timing.duration == 0
                        {
                            timing.duration = u32::try_from(scale(delta, timing.timescale)?)
                                .map_err(|_| fail(ContinuousErrorKind::TimeOverflow))?;
                        }
                    } else if let Some(old) = lane.pending.back_mut()
                        && let Some(prev) = old
                            .data
                            .packets
                            .iter_mut()
                            .rev()
                            .find(|p| !matches!(p.kind, StreamKind::Aac))
                        && let Some(timing) = &mut prev.timing
                        && timing.duration == 0
                    {
                        timing.duration = u32::try_from(scale(delta, timing.timescale)?)
                            .map_err(|_| fail(ContinuousErrorKind::TimeOverflow))?;
                    }
                }
            }
            lane.last[kind] = Some(t);
            previous_packet[kind] = Some(index);
        }
        let first = pts(&data.packets[0]);
        if previous_epoch.is_none() {
            lane.shift = sub(zero(), self.common)?;
        }
        let explicit = session.options.anchors.iter().find(|a| {
            a.input == *descriptor.slot().input_id()
                && a.generation == epoch.0
                && a.epoch == epoch.1
        });
        if let Some(anchor) = explicit {
            lane.shift = sub(anchor.presentation, anchor.source)?;
        }
        if let Some(pdt) = descriptor.program_date_time() {
            let wall = parse_pdt(pdt).map_err(timeline_error)?;
            let wall_origin = match self.wall {
                Some(v) => v,
                None => {
                    let v = sub(wall, add(first, lane.shift)?)?;
                    self.wall = Some(v);
                    v
                }
            };
            let candidate = sub(sub(wall, wall_origin)?, first)?;
            if (!new_epoch || explicit.is_some())
                && scale(sub(candidate, lane.shift)?, 1_000_000_000)?.unsigned_abs() > 1_000_000
            {
                return Err(at(ContinuousErrorKind::TimelineAmbiguous, &descriptor));
            }
            if new_epoch && explicit.is_none() {
                lane.shift = candidate;
            }
        } else if new_epoch && previous_epoch.is_some() && explicit.is_none() {
            if session.sources.len() > 1 {
                return Err(at(ContinuousErrorKind::TimelineAmbiguous, &descriptor));
            }
            if cmp(add(first, lane.shift)?, lane.end)?.is_lt() {
                lane.shift = sub(add(lane.end, lane.gap)?, first)?;
            }
        }
        lane.gap = zero();
        lane.epoch = Some(epoch);
        for p in &data.packets {
            let t = p.timing.unwrap();
            lane.end = max(
                lane.end,
                add(
                    add(pts(p), lane.shift)?,
                    MediaTime {
                        ticks: i128::from(t.duration),
                        timescale: t.timescale,
                    },
                )?,
            )?;
        }
        lane.pending.push_back(Batch {
            descriptor,
            data,
            shift: lane.shift,
            new_epoch: new_epoch || std::mem::take(&mut lane.mapping_dirty),
            changed,
        });
        self.budget(session)
    }
    pub fn budget(&self, session: &ContinuousSession) -> ContinuousResult<()> {
        let samples = self
            .lanes
            .iter()
            .flat_map(|l| &l.pending)
            .map(|b| b.data.packets.len())
            .sum::<usize>();
        let bytes = self
            .lanes
            .iter()
            .flat_map(|l| &l.pending)
            .flat_map(|b| &b.data.packets)
            .map(|p| p.data.len())
            .sum::<usize>();
        // Retained encoded MAPs share the operation byte budget with samples.
        // Resource permits cover in-flight reads, not these per-input caches.
        let bytes = if session.multi.is_some() {
            bytes.saturating_add(
                self.lanes
                    .iter()
                    .filter_map(|l| l.map.as_ref())
                    .map(|m| m.bytes().len())
                    .sum::<usize>(),
            )
        } else {
            bytes
        };
        let (subtitle_samples, subtitle_bytes) = if let Some(shared) = &session.multi {
            let mut multi = shared.lock().unwrap();
            multi.media_samples = samples;
            multi.media_bytes = bytes;
            multi.subtitle_usage()
        } else {
            (0, 0)
        };
        let samples = samples.saturating_add(subtitle_samples);
        let bytes = bytes.saturating_add(subtitle_bytes);
        if samples > session.options.limits.samples || bytes > session.options.limits.sample_bytes {
            return Err(fail(ContinuousErrorKind::BudgetExceeded));
        }
        let mut state = session.shared.inner.lock().unwrap();
        state.peaks.samples = state.peaks.samples.max(samples);
        state.peaks.sample_bytes = state.peaks.sample_bytes.max(bytes);
        Ok(())
    }
    fn tail(&mut self, session: &ContinuousSession, input: usize) -> ContinuousResult<()> {
        let lane = &mut self.lanes[input];
        for batch in &mut lane.pending {
            for p in &mut batch.data.packets {
                if let Some(t) = &mut p.timing
                    && t.duration == 0
                {
                    let delta = lane
                        .delta
                        .or(match session.options.tail {
                            TailDurationPolicy::Explicit(v) => Some(v),
                            _ => None,
                        })
                        .ok_or_else(|| {
                            at(ContinuousErrorKind::MissingTailDuration, &batch.descriptor)
                        })?;
                    t.duration = u32::try_from(scale(delta, t.timescale)?)
                        .map_err(|_| fail(ContinuousErrorKind::TimeOverflow))?;
                    lane.end = max(
                        lane.end,
                        add(
                            MediaTime {
                                ticks: t.pts + i128::from(t.duration),
                                timescale: t.timescale,
                            },
                            batch.shift,
                        )?,
                    )?;
                }
            }
        }
        Ok(())
    }
    async fn fill(&mut self, session: &ContinuousSession, input: usize) -> ContinuousResult<()> {
        loop {
            let needs = self.lanes[input].pending.front().is_none_or(|b| {
                !b.descriptor.gap()
                    && b.data
                        .packets
                        .iter()
                        .any(|p| p.timing.is_some_and(|t| t.duration == 0))
            });
            if !needs {
                return Ok(());
            }
            let descriptor = match session.descriptor(input).await? {
                Some(v) => v,
                None => {
                    self.tail(session, input)?;
                    return Ok(());
                }
            };
            if descriptor.gap() {
                if session.options.missing == MissingSegmentPolicy::Fail {
                    return Err(at(ContinuousErrorKind::MissingSegment, &descriptor));
                }
                self.tail(session, input)?;
                let gap = descriptor.duration().media_time().map_err(media_error)?;
                let lane = &mut self.lanes[input];
                lane.gap = add(lane.gap, gap)?;
                if session.options.gaps == GapPolicy::Collapse {
                    if session.sources.len() > 1 {
                        let start = lane.end;
                        let end = add(start, gap)?;
                        if let Some((others, a, b)) = &mut self.common_gap {
                            if others.contains(&input)
                                || !cmp(*a, start)?.is_eq()
                                || !cmp(*b, end)?.is_eq()
                            {
                                return Err(at(
                                    ContinuousErrorKind::TimelineAmbiguous,
                                    &descriptor,
                                ));
                            }
                            others.push(input);
                            if others.len() == session.sources.len() {
                                self.common_gap = None;
                            }
                        } else {
                            self.common_gap = Some((vec![input], start, end));
                        }
                    }
                    lane.shift = sub(lane.shift, gap)?;
                    lane.mapping_dirty = true;
                }
                lane.split_pending = session.options.missing == MissingSegmentPolicy::Split;
                lane.pending.push_back(Batch {
                    descriptor,
                    data: DemuxOutput::default(),
                    shift: lane.end,
                    new_epoch: false,
                    changed: false,
                });
                continue;
            }
            if self.lanes[input]
                .epoch
                .is_some_and(|e| e != (descriptor.slot().generation(), descriptor.slot().epoch()))
            {
                self.tail(session, input)?;
            }
            let data = session
                .load(input, &mut self.lanes[input].map, &descriptor)
                .await?;
            self.push(session, input, descriptor, data)?;
        }
    }
    pub async fn next(
        &mut self,
        session: &ContinuousSession,
    ) -> ContinuousResult<Option<(usize, Batch)>> {
        session.pause_boundary().await?;
        for input in 0..self.lanes.len() {
            self.fill(session, input).await?;
        }
        let mut selected = None;
        for (input, lane) in self.lanes.iter().enumerate() {
            if let Some(batch) = lane.pending.front() {
                let time = batch.first()?;
                if selected.is_none_or(|(_, old)| cmp(time, old).is_ok_and(|v| v.is_lt())) {
                    selected = Some((input, time));
                }
            }
        }
        if let Some((inputs, start, _)) = &self.common_gap {
            for input in 0..self.lanes.len() {
                if inputs.contains(&input) {
                    continue;
                }
                if let Some(other) = self.lanes[input].pending.front() {
                    for packet in &other.data.packets {
                        let t = packet.timing.unwrap();
                        let end = add(
                            add(pts(packet), other.shift)?,
                            MediaTime {
                                ticks: i128::from(t.duration),
                                timescale: t.timescale,
                            },
                        )?;
                        if cmp(end, *start)?.is_gt() {
                            return Err(at(
                                ContinuousErrorKind::TimelineAmbiguous,
                                &other.descriptor,
                            ));
                        }
                    }
                } else {
                    return Err(fail(ContinuousErrorKind::TimelineAmbiguous));
                }
            }
            if selected.is_some_and(|(i, _)| inputs.contains(&i)) {
                return Err(fail(ContinuousErrorKind::TimelineAmbiguous));
            }
        }
        let Some((input, _)) = selected else {
            return Ok(None);
        };
        for other in 0..self.lanes.len() {
            let (Some(a), Some(b)) = (
                self.lanes[input].pending.front(),
                self.lanes[other].pending.front(),
            ) else {
                continue;
            };
            let delta = sub(a.first()?, b.first()?)?;
            if scale(delta, 1_000_000)?.unsigned_abs()
                > scale(session.options.limits.skew, 1_000_000)? as u128
            {
                return Err(fail(ContinuousErrorKind::SkewTimeout));
            }
        }
        Ok(Some((
            input,
            self.lanes[input].pending.pop_front().unwrap(),
        )))
    }
    pub fn samples(
        &mut self,
        session: &ContinuousSession,
        input: usize,
        batch: &mut Batch,
    ) -> ContinuousResult<Vec<Vec<Mp4Sample>>> {
        let mut grouped = vec![Vec::new(); self.tracks.len()];
        let mut mappings = Vec::new();
        if input == 0
            && let Some(range) = session.options.range
        {
            for packet in &batch.data.packets {
                if !matches!(packet.kind, StreamKind::Aac)
                    && independent(packet).map_err(timeline_error)?
                {
                    let time = add(pts(packet), batch.shift)?;
                    if !cmp(time, range.end())?.is_lt() {
                        self.selected_end = Some(time);
                        session.handle().drain(ContinuousEndReason::DurationLimit);
                        break;
                    }
                }
            }
        }
        for mut packet in std::mem::take(&mut batch.data.packets) {
            session.check()?;
            let audio = matches!(packet.kind, StreamKind::Aac);
            let index = self
                .track_keys
                .iter()
                .position(|k| {
                    k.input == input
                        && k.kind
                            == if audio {
                                OutputTrackKind::Audio
                            } else {
                                OutputTrackKind::Video
                            }
                })
                .ok_or_else(|| at(ContinuousErrorKind::ConfigurationChanged, &batch.descriptor))?;
            let timescale = self.tracks[index].timescale;
            let t = packet.timing.unwrap();
            let source = MediaTime {
                ticks: t.dts,
                timescale: t.timescale,
            };
            let public_dts = add(source, batch.shift)?;
            let public_pts = add(pts(&packet), batch.shift)?;
            let duration = if audio && batch.descriptor.map().is_none() {
                MediaTime {
                    ticks: i128::from(packet.duration),
                    timescale: batch.data.sample_rate.unwrap_or(48_000),
                }
            } else {
                MediaTime {
                    ticks: i128::from(t.duration),
                    timescale: t.timescale,
                }
            };
            let limit = if audio {
                self.selected_end.or(session.options.range.map(|r| r.end()))
            } else {
                self.selected_end
            };
            if limit.is_some_and(|end| cmp(public_pts, end).is_ok_and(|v| !v.is_lt())) {
                continue;
            }
            packet.is_key = independent(&packet).map_err(timeline_error)?;
            packet.timing = Some(PacketTiming {
                edit_offset: 0,
                timescale,
                dts: scale(sub(public_dts, self.origin)?, timescale)?,
                pts: scale(sub(public_pts, self.origin)?, timescale)?,
                duration: u32::try_from(scale(duration, timescale)?)
                    .map_err(|_| fail(ContinuousErrorKind::TimeOverflow))?,
            });
            let mut sample =
                packet_sample(packet, timescale, 0, Some((0, 1))).map_err(media_error)?;
            if sample.duration == 0 {
                return Err(at(
                    ContinuousErrorKind::MissingTailDuration,
                    &batch.descriptor,
                ));
            }
            if let Some(end) = self.ends[index] {
                if sample.dts.abs_diff(end) <= 1 {
                    sample.pts += i128::from(end) - i128::from(sample.dts);
                    sample.dts = end;
                } else if sample.dts < end || !grouped[index].is_empty() {
                    return Err(at(
                        ContinuousErrorKind::TimelineAmbiguous,
                        &batch.descriptor,
                    ));
                }
            }
            let end = sample
                .dts
                .checked_add(u64::from(sample.duration))
                .ok_or_else(|| fail(ContinuousErrorKind::TimeOverflow))?;
            self.ends[index] = Some(end);
            if let Some(state) = &session.multi {
                let mut state = state.lock().unwrap();
                let report = state
                    .tracks
                    .iter_mut()
                    .find(|r| r.id.0 == self.tracks[index].track_id && r.output == self.output)
                    .unwrap();
                report.samples += 1;
                report.duration = report.duration.max(end);
            }
            self.reports[index].sample_count += 1;
            self.reports[index].duration = self.reports[index]
                .duration
                .max(end)
                .max(u64::try_from(sample.pts + i128::from(sample.duration)).unwrap_or(0));
            self.duration = max(self.duration, add(public_pts, duration)?)?;
            if batch.new_epoch
                && !mappings
                    .iter()
                    .any(|m: &ContinuousMapping| m.track == self.tracks[index].track_id)
            {
                mappings.push(ContinuousMapping {
                    input: batch.descriptor.slot().input_id().clone(),
                    generation: batch.descriptor.slot().generation(),
                    epoch: batch.descriptor.slot().epoch(),
                    track: self.tracks[index].track_id,
                    source,
                    presentation: public_dts,
                    output: self.output,
                    output_start: sub(public_dts, self.origin)?,
                    configuration: config_digest(&self.tracks),
                    pdt: batch.descriptor.program_date_time().map(str::to_owned),
                });
            }
            sample.dts = sample
                .dts
                .checked_sub(self.offsets[index])
                .ok_or_else(|| fail(ContinuousErrorKind::TimelineAmbiguous))?;
            sample.pts -= i128::from(self.offsets[index]);
            grouped[index].push(sample);
        }
        // Mapping publication is deferred until the fragment has flushed.
        for mapping in mappings {
            self.mappings.push_back(mapping);
        }
        Ok(grouped)
    }
    pub fn output_report(&self) -> ContinuousOutputReport {
        ContinuousOutputReport {
            index: self.output,
            collected_bytes: 0,
            classic_index_samples: 0,
            media: TransmuxReport {
                segment_count: self.segments,
                tracks: self.reports.clone(),
                duration: self
                    .reports
                    .iter()
                    .map(|t| (u128::from(t.duration) * 1000 / u128::from(t.timescale)) as u64)
                    .max()
                    .unwrap_or(0),
                duration_timescale: 1000,
                bytes_written: self.part_bytes,
            },
        }
    }
    pub fn report(&self, session: &ContinuousSession) -> ContinuousResult<ContinuousReport> {
        let state = session.shared.inner.lock().unwrap();
        let start = self.selected_start.unwrap_or(zero());
        Ok(ContinuousReport {
            requested: session.options.range,
            actual: Some(PresentationRange::new(start, self.duration).map_err(timeline_error)?),
            reason: state.reason.unwrap_or(ContinuousEndReason::Eof),
            inputs: state.lanes.iter().map(|l| l.progress.clone()).collect(),
            bytes: self.bytes,
            duration: sub(self.duration, start)?,
            gaps: self.gaps,
            outputs: self.outputs.iter().cloned().collect(),
            mappings: self.mappings.iter().cloned().collect(),
            truncated: self.truncated,
            peaks: state.peaks.clone(),
        })
    }
    pub fn retain_history(&mut self, session: &ContinuousSession) {
        while self.outputs.len() > session.options.limits.history {
            self.outputs.pop_front();
            self.truncated = true;
        }
        while self.mappings.len() > session.options.limits.history {
            self.mappings.pop_front();
            self.truncated = true;
        }
    }
    pub async fn seek(&mut self, session: &ContinuousSession) -> ContinuousResult<()> {
        let range = session.options.range.unwrap();
        let video = self
            .track_keys
            .iter()
            .any(|k| k.input == 0 && k.kind == OutputTrackKind::Video);
        loop {
            self.fill(session, 0).await?;
            let lane = &self.lanes[0];
            let mut candidate = None;
            let mut reached = false;
            for (b, batch) in lane.pending.iter().enumerate() {
                for packet in &batch.data.packets {
                    if matches!(packet.kind, StreamKind::Aac) == video {
                        continue;
                    }
                    let time = add(pts(packet), batch.shift)?;
                    if !cmp(time, range.start())?.is_lt() {
                        reached = true;
                    }
                    if (video
                        && !cmp(time, range.start())?.is_gt()
                        && independent(packet).map_err(timeline_error)?)
                        || (!video && candidate.is_none() && !cmp(time, range.start())?.is_lt())
                    {
                        candidate = Some((b, packet_time(packet, 0), time));
                    }
                }
            }
            if let Some((batch_index, dts, start)) = candidate {
                for _ in 0..batch_index {
                    let old = self.lanes[0].pending.pop_front().unwrap();
                    session.commit(0, &old.descriptor, 0)?;
                }
                let batch = self.lanes[0].pending.front_mut().unwrap();
                let shift = batch.shift;
                batch.data.packets.retain(|p| {
                    if matches!(p.kind, StreamKind::Aac) {
                        cmp(add(pts(p), shift).unwrap_or(zero()), start).is_ok_and(|v| !v.is_lt())
                    } else {
                        cmp(packet_time(p, 0), dts).is_ok_and(|v| !v.is_lt())
                    }
                });
                self.selected_start = Some(start);
            }
            if reached {
                if self.selected_start.is_none() {
                    return Err(fail(ContinuousErrorKind::MissingRandomAccess));
                }
                break;
            }
            let Some(descriptor) = session.descriptor(0).await? else {
                return Err(fail(ContinuousErrorKind::MissingRandomAccess));
            };
            if descriptor.gap() {
                return Err(at(ContinuousErrorKind::MissingSegment, &descriptor));
            }
            let data = session.load(0, &mut self.lanes[0].map, &descriptor).await?;
            self.push(session, 0, descriptor, data)?;
        }
        for input in 1..self.lanes.len() {
            let start = self.selected_start.unwrap();
            loop {
                self.fill(session, input).await?;
                let Some(batch) = self.lanes[input].pending.front_mut() else {
                    return Err(fail(ContinuousErrorKind::EmptyInput));
                };
                let shift = batch.shift;
                batch.data.packets.retain(|p| {
                    cmp(add(pts(p), shift).unwrap_or(zero()), start).is_ok_and(|v| !v.is_lt())
                });
                if !batch.data.packets.is_empty() {
                    break;
                }
                let old = self.lanes[input].pending.pop_front().unwrap();
                session.commit(input, &old.descriptor, 0)?;
            }
        }
        self.refresh_tracks(session)
    }
    pub fn split(
        &mut self,
        session: &ContinuousSession,
        input: usize,
        batch: Batch,
    ) -> ContinuousResult<()> {
        let mut batch = batch;
        batch.changed = false;
        self.lanes[input].pending.push_front(batch);
        self.output += 1;
        self.refresh_tracks(session)
    }
}
fn pts(packet: &EncodedPacket) -> MediaTime {
    packet.timing.map_or(
        MediaTime {
            ticks: packet.pts_90k,
            timescale: 90_000,
        },
        |t| MediaTime {
            ticks: t.pts,
            timescale: t.timescale,
        },
    )
}

fn unwrap_packed(data: &mut DemuxOutput, reference: i128) -> ContinuousResult<()> {
    let Some(anchor) = data.packed_anchor else {
        return Ok(());
    };
    let unwrapped = unwrap_near(i128::from(anchor), reference).map_err(media_error)?;
    // Adjust exact LCM clocks by whole wrap periods, never round individual frames.
    let delta = unwrapped - i128::from(anchor);
    for packet in &mut data.packets {
        let t = packet
            .timing
            .as_mut()
            .ok_or_else(|| fail(ContinuousErrorKind::Media))?;
        let shift = delta
            .checked_mul(i128::from(t.timescale / 90_000))
            .ok_or_else(|| fail(ContinuousErrorKind::TimeOverflow))?;
        // prepare may already have selected the same wrap; restore original anchor first.
        t.dts += shift;
        t.pts += shift;
    }
    data.packed_anchor =
        Some(u64::try_from(unwrapped).map_err(|_| fail(ContinuousErrorKind::TimelineAmbiguous))?);
    Ok(())
}

impl Engine {
    pub fn mux_tracks(&self, session: &ContinuousSession) -> (Vec<FragmentedTrack>, Vec<u64>) {
        let mut tracks = self.tracks.clone();
        let mut offsets = self.offsets.clone();
        if let Some(state) = &session.multi {
            let mut state = state.lock().unwrap();
            let subtitles: Vec<_> = state
                .subtitles
                .iter()
                .map(|s| (s.track(), s.config.id.clone()))
                .collect();
            for (track, id) in subtitles {
                if !state
                    .tracks
                    .iter()
                    .any(|t| t.id.0 == track.track_id && t.output == self.output)
                {
                    state.tracks.push(OutputTrackInfo {
                        id: OutputTrackId(track.track_id),
                        output: self.output,
                        input: id,
                        kind: OutputTrackKind::Subtitle,
                        codec: OutputTrackCodec::Wvtt,
                        metadata: track.metadata.clone().unwrap(),
                        timescale: track.timescale,
                        duration: 0,
                        samples: 0,
                    });
                }
                tracks.push(track);
                offsets.push(0);
            }
            state.trim_tracks(self.output, &session.options.limits);
        }
        (tracks, offsets)
    }
    pub fn append_subtitles(
        &mut self,
        session: &ContinuousSession,
        input: usize,
        batch: &Batch,
        grouped: &mut Vec<Vec<Mp4Sample>>,
    ) -> ContinuousResult<()> {
        let Some(shared) = &session.multi else {
            return Ok(());
        };
        let mut until = self.origin;
        for (index, samples) in grouped.iter().enumerate() {
            if self.track_keys[index].input != input {
                continue;
            }
            for sample in samples {
                let end = add(
                    self.origin,
                    MediaTime {
                        ticks: sample.pts
                            + i128::from(self.offsets[index])
                            + i128::from(sample.duration),
                        timescale: self.tracks[index].timescale,
                    },
                )?;
                until = max(until, end)?;
            }
        }
        let extra = shared.lock().unwrap().render_subtitles(
            batch.descriptor.slot().input_id(),
            batch.descriptor.slot().generation(),
            batch.descriptor.slot().epoch(),
            batch.shift,
            self.origin,
            until,
            self.output,
            session.options.range,
            &session.options.limits,
        )?;
        {
            let state = shared.lock().unwrap();
            let mut tracks = self.tracks.clone();
            tracks.extend(state.subtitles.iter().map(|s| s.track()));
            let configuration = config_digest(&tracks);
            for lane in &state.subtitles {
                if &lane.config.timeline_input == batch.descriptor.slot().input_id()
                    && (batch.new_epoch
                        || !self
                            .mappings
                            .iter()
                            .any(|m| m.track == lane.track.0 && m.output == self.output))
                {
                    self.mappings.push_back(ContinuousMapping {
                        input: lane.config.id.clone(),
                        generation: batch.descriptor.slot().generation(),
                        epoch: batch.descriptor.slot().epoch(),
                        track: lane.track.0,
                        source: sub(self.origin, batch.shift)?,
                        presentation: self.origin,
                        output: self.output,
                        output_start: zero(),
                        configuration,
                        pdt: batch.descriptor.program_date_time().map(str::to_owned),
                    });
                }
            }
        }
        grouped.extend(extra);
        let count = grouped.iter().map(Vec::len).sum::<usize>();
        let bytes = grouped
            .iter()
            .flatten()
            .map(|s| s.data.len())
            .sum::<usize>();
        if count > session.options.limits.samples || bytes > session.options.limits.sample_bytes {
            return Err(fail(ContinuousErrorKind::BudgetExceeded));
        }
        Ok(())
    }
}
