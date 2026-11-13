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
    fn first(&self) -> EngineResult<MediaTime> {
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
            .ok_or_else(|| fail(EngineErrorKind::EmptyInput))
    }
}
struct Lane {
    aac: AacRepairState,
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
    recovery_boundary: bool,
    mapping_dirty: bool,
}
impl Lane {
    fn new() -> Self {
        Self {
            aac: AacRepairState::default(),
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
            recovery_boundary: false,
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
    downloads: Downloads,
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
    pub mappings: VecDeque<EngineMapping>,
    pub outputs: VecDeque<EngineOutputReport>,
    pub truncated: bool,
    pub gaps: u64,
    wall: Option<MediaTime>,
    pub selected_start: Option<MediaTime>,
    selected_end: Option<MediaTime>,
    common_gap: Option<(Vec<usize>, MediaTime, MediaTime)>,
}
impl SessionCore {
    async fn descriptor(&self, input: usize) -> EngineResult<Option<SegmentDescriptor>> {
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
                _ = self.shared.signal.cancelled() => return Err(fail(EngineErrorKind::Cancelled)),
                _ = async {if let Some(deadline)=&mut timeout {deadline.await;}else{std::future::pending::<()>().await;}} => return Err(fail(EngineErrorKind::SkewTimeout)),
                _ = wake.changed() => {}
            }
        }
    }
    async fn load(
        &self,
        input: usize,
        map: &mut Option<EncodedResource>,
        descriptor: &SegmentDescriptor,
        downloads: Option<&mut Downloads>,
    ) -> EngineResult<DemuxOutput> {
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
                let downloaded = match downloads {
                    Some(downloads) if self.options.download_concurrency > 1 => {
                        Some(downloads.read(self, input, descriptor).await?)
                    }
                    _ => None,
                };
                let bytes = self
                    .resources
                    .read_encoded_downloaded(source, request.clone(), downloaded)
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
                    let kind = if e.key_error().is_some_and(|k| {
                        k.kind() == crate::crypto::key::KeyErrorKind::ResumeConflict
                    }) {
                        EngineErrorKind::ResumeConflict
                    } else if e.kind() == crate::crypto::sample::SampleErrorKind::Cancelled {
                        EngineErrorKind::Cancelled
                    } else {
                        EngineErrorKind::Media
                    };
                    EngineError {
                        sample: Some(Box::new(e)),
                        ..fail(kind)
                    }
                })?;
                if self.resources.recovery_enabled() {
                    data.resource_digest = Some(crate::resume::digest(bytes.bytes()));
                    data.map_digest = map.as_ref().map(|m| crate::resume::digest(m.bytes()));
                }
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
    ) -> EngineResult<()> {
        self.check()?;
        let progress = {
            let mut state = self.shared.inner.lock().unwrap();
            let lane = &mut state.lanes[input];
            lane.outstanding -= 1;
            lane.progress.committed += 1;
            lane.progress.watermark = Some(descriptor.slot().clone());
            lane.progress.clone()
        };
        self.resources.committed_keys(descriptor.slot());
        self.shared.signal.wake();
        self.emit(EngineEvent::Committed {
            input: progress,
            bytes,
        })
    }
}
impl Engine {
    pub(super) fn replay_front(&mut self, input: usize, batch: Batch) {
        self.lanes[input].pending.push_front(batch);
    }
    pub(super) fn take_front(&mut self, input: usize) -> Batch {
        self.lanes[input]
            .pending
            .pop_front()
            .expect("saved split batch")
    }
    pub async fn prepare(session: &SessionCore) -> EngineResult<Self> {
        let mut engine = Self {
            downloads: Downloads::default(),
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
                    .ok_or_else(|| fail(EngineErrorKind::EmptyInput))?;
                if descriptor.gap() {
                    return Err(at(EngineErrorKind::MissingRandomAccess, &descriptor));
                }
                let mut data = session
                    .load(
                        input,
                        &mut engine.lanes[input].map,
                        &descriptor,
                        Some(&mut engine.downloads),
                    )
                    .await?;
                merge_probe_config(&mut engine.lanes[input].config, &mut data)
                    .map_err(media_error)?;
                if build_fragmented_tracks(&data).is_ok() && !data.packets.is_empty() {
                    found = Some((descriptor, data));
                    break;
                }
                if !data.packets.is_empty() {
                    return Err(at(EngineErrorKind::Media, &descriptor));
                }
                session.commit(input, &descriptor, 0)?;
            }
            raw.push(found.ok_or_else(|| fail(EngineErrorKind::EmptyInput))?);
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
                return Err(fail(EngineErrorKind::BudgetExceeded));
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
                    p.dts_90k =
                        u64::try_from(dts).map_err(|_| fail(EngineErrorKind::TimelineAmbiguous))?;
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
        engine.common = common.ok_or_else(|| fail(EngineErrorKind::EmptyInput))?;
        for (input, (descriptor, data)) in raw.into_iter().enumerate() {
            engine.push(session, input, descriptor, data)?;
        }
        engine.refresh_tracks(session)?;
        engine.budget(session)?;
        Ok(engine)
    }
    fn refresh_tracks(&mut self, session: &SessionCore) -> EngineResult<()> {
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
        self.origin = origin.ok_or_else(|| fail(EngineErrorKind::EmptyInput))?;
        for (input, lane) in self.lanes.iter().enumerate() {
            let config = lane
                .pending
                .front()
                .map(|b| &b.data)
                .or(lane.config.as_ref())
                .ok_or_else(|| fail(EngineErrorKind::EmptyInput))?;
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
                        .ok_or_else(|| fail(EngineErrorKind::EmptyInput))?;
                    if !audio && !independent(first)? {
                        return Err(at(EngineErrorKind::MissingRandomAccess, &batch.descriptor));
                    }
                    scale(
                        sub(add(packet_time(first, 0), batch.shift)?, self.origin)?,
                        track.timescale,
                    )?
                } else {
                    0
                };
                self.offsets.push(
                    u64::try_from(offset).map_err(|_| fail(EngineErrorKind::TimelineAmbiguous))?,
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
            return Err(fail(EngineErrorKind::EmptyInput));
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
        session: &SessionCore,
        input: usize,
        descriptor: SegmentDescriptor,
        mut data: DemuxOutput,
    ) -> EngineResult<()> {
        let lane = &mut self.lanes[input];
        let epoch = (descriptor.slot().generation(), descriptor.slot().epoch());
        let new_epoch = lane.epoch != Some(epoch);
        let previous_epoch = lane.epoch;
        if new_epoch {
            lane.aac.next = None;
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
            lane.aac.next = None;
            if session.options.changes == TimelineChangePolicy::Fail {
                return Err(at(EngineErrorKind::ConfigurationChanged, &descriptor));
            }
            lane.config = None;
            check_media_config(&mut lane.config, &data).map_err(media_error)?;
        }
        if data.packets.is_empty() {
            return Err(at(EngineErrorKind::EmptyInput, &descriptor));
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
        let mut repaired_count = 0u64;
        let mut repaired_max = zero();
        let repair_limit = session.options.aac.limit()?;
        let mut previous_packet = [None, None];
        for index in 0..data.packets.len() {
            let p = &mut data.packets[index];
            let audio = matches!(p.kind, StreamKind::Aac);
            let kind = usize::from(audio);
            let ts_audio = audio && p.timing.is_none() && data.packed_anchor.is_none();
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
                                .ok_or_else(|| fail(EngineErrorKind::TimeOverflow))?
                                / u64::from(data.sample_rate.unwrap_or(48_000)),
                        )
                        .map_err(|_| fail(EngineErrorKind::TimeOverflow))?
                    } else {
                        0
                    },
                });
            }
            if ts_audio
                && let Some(limit) = repair_limit
                && let Some(position) =
                    lane.aac
                        .apply(p, data.sample_rate.unwrap_or(0), descriptor.slot(), limit)?
            {
                repaired_count += 1;
                repaired_max = max(repaired_max, sub(position.repaired, position.original)?)?;
            }
            let t = packet_time(p, 0);
            if let Some(last) = lane.last[kind] {
                let delta = sub(t, last)?;
                if delta.ticks <= 0
                    || (descriptor.map().is_none() && scale(delta, 90_000)? >= 1i128 << 32)
                {
                    if delta.ticks <= 0
                        && ts_audio
                        && repair_limit.is_none()
                        && (!config_changed || previous_packet[kind].is_some())
                        && p.duration == 1024
                        && p.timing
                            .is_some_and(|t| t.pts == t.dts && t.edit_offset == 0)
                        && let Some(rate) = data.sample_rate.filter(|rate| *rate > 0)
                    {
                        let expected = add(
                            last,
                            MediaTime {
                                ticks: 1024,
                                timescale: rate,
                            },
                        )?;
                        return Err(aac::overlap_error(descriptor.slot(), t, expected)?);
                    }
                    return Err(at(EngineErrorKind::TimelineAmbiguous, &descriptor));
                }
                if !audio {
                    lane.delta = Some(delta);
                    if let Some(previous) = previous_packet[kind] {
                        let prev: &mut EncodedPacket = &mut data.packets[previous];
                        if let Some(timing) = &mut prev.timing
                            && timing.duration == 0
                        {
                            timing.duration = u32::try_from(scale(delta, timing.timescale)?)
                                .map_err(|_| fail(EngineErrorKind::TimeOverflow))?;
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
                            .map_err(|_| fail(EngineErrorKind::TimeOverflow))?;
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
            let wall = parse_pdt(pdt)?;
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
                return Err(at(EngineErrorKind::TimelineAmbiguous, &descriptor));
            }
            if new_epoch && explicit.is_none() {
                lane.shift = candidate;
            }
        } else if new_epoch && previous_epoch.is_some() && explicit.is_none() {
            if session.sources.len() > 1 {
                return Err(at(EngineErrorKind::TimelineAmbiguous, &descriptor));
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
        if repaired_count > 0 {
            session.emit(EngineEvent::AacTimestampRepaired {
                slot: descriptor.slot().clone(),
                corrected_frames: repaired_count,
                max_shift: repaired_max,
            })?;
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
    pub fn budget(&self, session: &SessionCore) -> EngineResult<()> {
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
            return Err(fail(EngineErrorKind::BudgetExceeded));
        }
        let mut state = session.shared.inner.lock().unwrap();
        state.peaks.samples = state.peaks.samples.max(samples);
        state.peaks.sample_bytes = state.peaks.sample_bytes.max(bytes);
        Ok(())
    }
    fn tail(&mut self, session: &SessionCore, input: usize) -> EngineResult<()> {
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
                            at(EngineErrorKind::MissingTailDuration, &batch.descriptor)
                        })?;
                    t.duration = u32::try_from(scale(delta, t.timescale)?)
                        .map_err(|_| fail(EngineErrorKind::TimeOverflow))?;
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
    async fn fill(&mut self, session: &SessionCore, input: usize) -> EngineResult<()> {
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
                    return Err(at(EngineErrorKind::MissingSegment, &descriptor));
                }
                self.tail(session, input)?;
                let gap = descriptor.duration().media_time().map_err(media_error)?;
                let lane = &mut self.lanes[input];
                lane.aac.next = None;
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
                                return Err(at(EngineErrorKind::TimelineAmbiguous, &descriptor));
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
                .load(
                    input,
                    &mut self.lanes[input].map,
                    &descriptor,
                    Some(&mut self.downloads),
                )
                .await?;
            self.push(session, input, descriptor, data)?;
        }
    }
    pub async fn next(&mut self, session: &SessionCore) -> EngineResult<Option<(usize, Batch)>> {
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
                            return Err(at(EngineErrorKind::TimelineAmbiguous, &other.descriptor));
                        }
                    }
                } else {
                    return Err(fail(EngineErrorKind::TimelineAmbiguous));
                }
            }
            if selected.is_some_and(|(i, _)| inputs.contains(&i)) {
                return Err(fail(EngineErrorKind::TimelineAmbiguous));
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
                return Err(fail(EngineErrorKind::SkewTimeout));
            }
        }
        let lane = &mut self.lanes[input];
        let mut batch = lane.pending.pop_front().unwrap();
        if batch.descriptor.gap() {
            lane.recovery_boundary = true;
        } else if lane.recovery_boundary {
            if let Some(packet) = batch
                .data
                .packets
                .iter()
                .find(|p| !matches!(p.kind, StreamKind::Aac))
                && !independent(packet)?
            {
                return Err(at(EngineErrorKind::MissingRandomAccess, &batch.descriptor));
            }
            batch.changed |= session.options.missing == MissingSegmentPolicy::Split;
            lane.recovery_boundary = false;
        }
        Ok(Some((input, batch)))
    }
    pub fn samples(
        &mut self,
        session: &SessionCore,
        input: usize,
        batch: &mut Batch,
    ) -> EngineResult<Vec<Vec<Mp4Sample>>> {
        let mut grouped = vec![Vec::new(); self.tracks.len()];
        let mut mappings = Vec::new();
        if input == 0
            && let Some(range) = session.options.range
        {
            for packet in &batch.data.packets {
                if !matches!(packet.kind, StreamKind::Aac) && independent(packet)? {
                    let time = add(pts(packet), batch.shift)?;
                    if !cmp(time, range.end())?.is_lt() {
                        self.selected_end = Some(time);
                        session.handle().drain(EngineEndReason::DurationLimit);
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
                .ok_or_else(|| at(EngineErrorKind::ConfigurationChanged, &batch.descriptor))?;
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
            packet.is_key = independent(&packet)?;
            packet.timing = Some(PacketTiming {
                edit_offset: 0,
                timescale,
                dts: scale(sub(public_dts, self.origin)?, timescale)?,
                pts: scale(sub(public_pts, self.origin)?, timescale)?,
                duration: u32::try_from(scale(duration, timescale)?)
                    .map_err(|_| fail(EngineErrorKind::TimeOverflow))?,
            });
            let mut sample =
                packet_sample(packet, timescale, 0, Some((0, 1))).map_err(media_error)?;
            if sample.duration == 0 {
                return Err(at(EngineErrorKind::MissingTailDuration, &batch.descriptor));
            }
            if let Some(end) = self.ends[index] {
                if sample.dts.abs_diff(end) <= 1 {
                    sample.pts += i128::from(end) - i128::from(sample.dts);
                    sample.dts = end;
                } else if sample.dts < end || !grouped[index].is_empty() {
                    // Retain the existing one-output-tick rounding tolerance. Classify
                    // only an actual overlap, not a positive gap in the same fragment.
                    // Demux metadata survives schema 2/3 restore: TS has no native
                    // audio timescale, while Packed AAC carries a packed anchor.
                    if sample.dts < end
                        && audio
                        && !session.options.aac.enabled()
                        && batch.data.audio_timescale.is_none()
                        && batch.data.packed_anchor.is_none()
                        && batch.data.sample_rate.is_some_and(|rate| rate > 0)
                        && duration.ticks == 1024
                        && t.pts == t.dts
                        && t.edit_offset == 0
                    {
                        let expected = sub(
                            add(
                                MediaTime {
                                    ticks: i128::from(end),
                                    timescale,
                                },
                                self.origin,
                            )?,
                            batch.shift,
                        )?;
                        return Err(aac::overlap_error(
                            batch.descriptor.slot(),
                            source,
                            expected,
                        )?);
                    }
                    return Err(at(EngineErrorKind::TimelineAmbiguous, &batch.descriptor));
                }
            }
            let end = sample
                .dts
                .checked_add(u64::from(sample.duration))
                .ok_or_else(|| fail(EngineErrorKind::TimeOverflow))?;
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
                    .any(|m: &EngineMapping| m.track == self.tracks[index].track_id)
            {
                mappings.push(EngineMapping {
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
                .ok_or_else(|| fail(EngineErrorKind::TimelineAmbiguous))?;
            sample.pts -= i128::from(self.offsets[index]);
            grouped[index].push(sample);
        }
        // Mapping publication is deferred until the fragment has flushed.
        for mapping in mappings {
            self.mappings.push_back(mapping);
        }
        Ok(grouped)
    }
    pub fn output_report(&self) -> EngineOutputReport {
        EngineOutputReport {
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
    pub fn report(&self, session: &SessionCore) -> EngineResult<EngineMediaReport> {
        let state = session.shared.inner.lock().unwrap();
        let start = self.selected_start.unwrap_or(zero());
        Ok(EngineMediaReport {
            aac_repairs: session.options.aac.enabled().then(|| {
                self.lanes
                    .iter()
                    .filter_map(|l| l.aac.summary.clone())
                    .collect()
            }),
            requested: session.options.range,
            actual: Some(PresentationRange::new(start, self.duration)?),
            reason: state.reason.unwrap_or(EngineEndReason::Eof),
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
    pub fn retain_history(&mut self, session: &SessionCore) {
        while self.outputs.len() > session.options.limits.history {
            self.outputs.pop_front();
            self.truncated = true;
        }
        while self.mappings.len() > session.options.limits.history {
            self.mappings.pop_front();
            self.truncated = true;
        }
    }
    pub async fn seek(&mut self, session: &SessionCore) -> EngineResult<()> {
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
                    if (video && !cmp(time, range.start())?.is_gt() && independent(packet)?)
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
                    return Err(fail(EngineErrorKind::MissingRandomAccess));
                }
                break;
            }
            let Some(descriptor) = session.descriptor(0).await? else {
                return Err(fail(EngineErrorKind::MissingRandomAccess));
            };
            if descriptor.gap() {
                return Err(at(EngineErrorKind::MissingSegment, &descriptor));
            }
            let data = session
                .load(
                    0,
                    &mut self.lanes[0].map,
                    &descriptor,
                    Some(&mut self.downloads),
                )
                .await?;
            self.push(session, 0, descriptor, data)?;
        }
        for input in 1..self.lanes.len() {
            let start = self.selected_start.unwrap();
            loop {
                self.fill(session, input).await?;
                let Some(batch) = self.lanes[input].pending.front_mut() else {
                    return Err(fail(EngineErrorKind::EmptyInput));
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
    pub fn split(&mut self, session: &SessionCore, input: usize, batch: Batch) -> EngineResult<()> {
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

fn unwrap_packed(data: &mut DemuxOutput, reference: i128) -> EngineResult<()> {
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
            .ok_or_else(|| fail(EngineErrorKind::Media))?;
        let shift = delta
            .checked_mul(i128::from(t.timescale / 90_000))
            .ok_or_else(|| fail(EngineErrorKind::TimeOverflow))?;
        // prepare may already have selected the same wrap; restore original anchor first.
        t.dts += shift;
        t.pts += shift;
    }
    data.packed_anchor =
        Some(u64::try_from(unwrapped).map_err(|_| fail(EngineErrorKind::TimelineAmbiguous))?);
    Ok(())
}

impl Engine {
    pub fn mux_tracks(&self, session: &SessionCore) -> (Vec<FragmentedTrack>, Vec<u64>) {
        let mut tracks = self.tracks.clone();
        let mut offsets = self.offsets.clone();
        if let Some(state) = &session.multi {
            let mut state = state.lock().unwrap();
            let subtitles: Vec<_> = state
                .subtitles
                .iter()
                .filter(|s| s.config.embedded)
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
        session: &SessionCore,
        input: usize,
        batch: &Batch,
        grouped: &mut Vec<Vec<Mp4Sample>>,
    ) -> EngineResult<()> {
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
            tracks.extend(
                state
                    .subtitles
                    .iter()
                    .filter(|s| s.config.embedded)
                    .map(|s| s.track()),
            );
            let configuration = config_digest(&tracks);
            for lane in &state.subtitles {
                if &lane.config.timeline_input == batch.descriptor.slot().input_id()
                    && (batch.new_epoch
                        || !self
                            .mappings
                            .iter()
                            .any(|m| m.track == lane.track.0 && m.output == self.output))
                {
                    self.mappings.push_back(EngineMapping {
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
            return Err(fail(EngineErrorKind::BudgetExceeded));
        }
        Ok(())
    }
}

use crate::state_codec::{DecodeResult, Reader, StateCodec, state_struct};
state_struct!(TrackKey { input, kind });

// Only metadata and hashes enter the archive; replay supplies every packet byte.
impl Engine {
    #[cfg(not(target_arch = "wasm32"))]
    fn resources_for_checkpoint(&self, session: &SessionCore) -> EngineResult<Vec<u8>> {
        if session.shared.inner.lock().unwrap().state == EngineState::Finalizing {
            return Ok(vec![0; 8]);
        }
        session
            .resources
            .save_recovery_keys()
            .map_err(resource_error)
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn save(&self, session: &SessionCore) -> EngineResult<Vec<u8>> {
        let mut out = Vec::new();
        self.resources_for_checkpoint(session)?.put(&mut out);
        self.tracks.put(&mut out);
        self.track_keys.put(&mut out);
        self.offsets.put(&mut out);
        self.origin.put(&mut out);
        self.common.put(&mut out);
        self.reports.put(&mut out);
        self.ends.put(&mut out);
        self.bytes.put(&mut out);
        self.part_bytes.put(&mut out);
        self.segments.put(&mut out);
        self.duration.put(&mut out);
        self.output.put(&mut out);
        self.mappings.put(&mut out);
        self.outputs.put(&mut out);
        self.truncated.put(&mut out);
        self.gaps.put(&mut out);
        self.wall.put(&mut out);
        self.selected_start.put(&mut out);
        self.selected_end.put(&mut out);
        self.common_gap.put(&mut out);
        self.lanes.len().put(&mut out);
        for lane in &self.lanes {
            lane.config.put(&mut out);
            lane.epoch.put(&mut out);
            lane.shift.put(&mut out);
            lane.clock.put(&mut out);
            lane.last.put(&mut out);
            lane.delta.put(&mut out);
            lane.end.put(&mut out);
            lane.gap.put(&mut out);
            lane.split_pending.put(&mut out);
            lane.recovery_boundary.put(&mut out);
            lane.mapping_dirty.put(&mut out);
            if session.options.aac.enabled() {
                lane.aac.put(&mut out);
            }
            let terminal = session.shared.inner.lock().unwrap().state == EngineState::Finalizing;
            let pending = if terminal { 0 } else { lane.pending.len() };
            pending.put(&mut out);
            for batch in lane.pending.iter().take(pending) {
                batch.descriptor.slot().put(&mut out);
                batch.descriptor.checkpoint_identity().put(&mut out);
                batch.descriptor.duration().put(&mut out);
                batch.shift.put(&mut out);
                batch.new_epoch.put(&mut out);
                batch.changed.put(&mut out);
                batch.data.put(&mut out);
                batch.data.packets.len().put(&mut out);
                for p in &batch.data.packets {
                    crate::resume::digest(&p.data).put(&mut out);
                    p.kind.put(&mut out);
                    p.timing.put(&mut out);
                    p.pts_90k.put(&mut out);
                    p.dts_90k.put(&mut out);
                    p.duration.put(&mut out);
                    p.is_key.put(&mut out);
                    p.is_length_prefixed.put(&mut out);
                }
            }
        }
        let state = session.shared.inner.lock().unwrap();
        state.reason.put(&mut out);
        for lane in &state.lanes {
            lane.progress.put(&mut out);
            lane.generation.put(&mut out);
            lane.revision.put(&mut out);
            lane.media_sequence.put(&mut out);
            lane.last.put(&mut out);
            lane.restart.put(&mut out);
            lane.ended.put(&mut out);
            lane.queue
                .iter()
                .take(if state.state == EngineState::Finalizing {
                    0
                } else {
                    lane.queue.len()
                })
                .map(|d| (d.slot().clone(), d.checkpoint_identity(), d.duration()))
                .collect::<Vec<_>>()
                .put(&mut out);
        }
        if let Some(multi) = &session.multi {
            let multi = multi.lock().unwrap();
            multi.tracks.put(&mut out);
            multi.track_history_truncated.put(&mut out);
            multi.save_subtitles(&mut out, session.durable_subtitles());
        } else {
            return Err(fail(EngineErrorKind::InvalidOptions));
        }
        if out.len() > session.options.limits.metadata {
            return Err(fail(EngineErrorKind::BudgetExceeded));
        }
        Ok(out)
    }

    pub(super) async fn restore(session: &SessionCore, bytes: &[u8]) -> EngineResult<Self> {
        let bad = || fail(EngineErrorKind::ResumeCorruption);
        if bytes.len() > session.options.limits.metadata {
            return Err(fail(EngineErrorKind::BudgetExceeded));
        }
        let terminal = session.recovery.as_ref().is_some_and(|c| c.finalizing);
        let mut r = Reader(bytes);
        let keys: Vec<u8> = StateCodec::get(&mut r).map_err(|_| bad())?;
        if !terminal {
            session
                .resources
                .restore_recovery_keys(&keys)
                .map_err(resource_error)?;
        }
        let mut engine = (|| -> DecodeResult<Self> {
            Ok(Self {
                downloads: Downloads::default(),
                tracks: StateCodec::get(&mut r)?,
                track_keys: StateCodec::get(&mut r)?,
                offsets: StateCodec::get(&mut r)?,
                origin: StateCodec::get(&mut r)?,
                common: StateCodec::get(&mut r)?,
                reports: StateCodec::get(&mut r)?,
                ends: StateCodec::get(&mut r)?,
                bytes: StateCodec::get(&mut r)?,
                part_bytes: StateCodec::get(&mut r)?,
                segments: StateCodec::get(&mut r)?,
                duration: StateCodec::get(&mut r)?,
                output: StateCodec::get(&mut r)?,
                mappings: StateCodec::get(&mut r)?,
                outputs: StateCodec::get(&mut r)?,
                truncated: StateCodec::get(&mut r)?,
                gaps: StateCodec::get(&mut r)?,
                wall: StateCodec::get(&mut r)?,
                selected_start: StateCodec::get(&mut r)?,
                selected_end: StateCodec::get(&mut r)?,
                common_gap: StateCodec::get(&mut r)?,
                lanes: vec![],
            })
        })()
        .map_err(|_| bad())?;
        let count = usize::get(&mut r).map_err(|_| bad())?;
        if count != session.sources.len()
            || count == 0
            || engine.tracks.len() != engine.track_keys.len()
            || engine.tracks.len() != engine.offsets.len()
            || engine.tracks.len() != engine.reports.len()
            || engine.tracks.len() != engine.ends.len()
            || engine.track_keys.iter().any(|t| t.input >= count)
            || engine.tracks.iter().any(|t| t.timescale == 0)
            || engine.bytes < engine.part_bytes
        {
            return Err(bad());
        }
        let find = |slot: &SegmentSlot,
                    digest: [u8; 32],
                    duration: crate::playlist::PlaylistDuration|
         -> EngineResult<(SegmentDescriptor, bool)> {
            let state = session.shared.inner.lock().unwrap();
            let lane = state
                .lanes
                .iter()
                .find(|l| &l.id == slot.input_id())
                .ok_or_else(|| fail(EngineErrorKind::ResumeConflict))?;
            if let Some(descriptor) = lane
                .queue
                .iter()
                .chain(&lane.history)
                .find(|d| d.slot() == slot)
            {
                if descriptor.checkpoint_identity() != digest || descriptor.duration() != duration {
                    return Err(fail(EngineErrorKind::ResumeConflict));
                }
                return Ok((descriptor.clone(), false));
            }
            // Only an explicitly advanced live window proves eviction. Omitted
            // snapshots or holes inside a retained window still require replay.
            if session.options.mode == EngineMode::Open
                && session.options.missing != MissingSegmentPolicy::Fail
                && lane.generation == Some(slot.generation())
                && lane.media_sequence.is_some_and(|n| n > slot.sequence())
            {
                return Ok((
                    SegmentDescriptor::recovery_gap(slot.clone(), duration),
                    true,
                ));
            }
            Err(fail(EngineErrorKind::ReplayRequired))
        };
        let mut pending_slots = vec![vec![]; count];
        let mut collapsed_intervals = vec![Vec::new(); count];
        for (input, slots) in pending_slots.iter_mut().enumerate() {
            let mut lane = (|| -> DecodeResult<Lane> {
                Ok(Lane {
                    config: StateCodec::get(&mut r)?,
                    epoch: StateCodec::get(&mut r)?,
                    shift: StateCodec::get(&mut r)?,
                    clock: StateCodec::get(&mut r)?,
                    last: StateCodec::get(&mut r)?,
                    delta: StateCodec::get(&mut r)?,
                    end: StateCodec::get(&mut r)?,
                    gap: StateCodec::get(&mut r)?,
                    split_pending: StateCodec::get(&mut r)?,
                    recovery_boundary: StateCodec::get(&mut r)?,
                    mapping_dirty: StateCodec::get(&mut r)?,
                    aac: if session.options.aac.enabled() {
                        StateCodec::get(&mut r)?
                    } else {
                        AacRepairState::default()
                    },
                    map: None,
                    pending: VecDeque::new(),
                })
            })()
            .map_err(|_| bad())?;
            let n = usize::get(&mut r).map_err(|_| bad())?;
            if n > session.options.limits.descriptors {
                return Err(bad());
            }
            let mut collapsed = zero();
            for _ in 0..n {
                let slot = SegmentSlot::get(&mut r).map_err(|_| bad())?;
                let digest = StateCodec::get(&mut r).map_err(|_| bad())?;
                let duration = StateCodec::get(&mut r).map_err(|_| bad())?;
                let (descriptor, evicted) = find(&slot, digest, duration)?;
                let mut batch = (|| -> DecodeResult<Batch> {
                    Ok(Batch {
                        descriptor,
                        shift: StateCodec::get(&mut r)?,
                        new_epoch: StateCodec::get(&mut r)?,
                        changed: StateCodec::get(&mut r)?,
                        data: StateCodec::get(&mut r)?,
                    })
                })()
                .map_err(|_| bad())?;
                let samples = usize::get(&mut r).map_err(|_| bad())?;
                if samples > session.options.limits.samples {
                    return Err(bad());
                }
                let mut raw = if batch.descriptor.gap() {
                    DemuxOutput::default()
                } else {
                    session
                        .load(input, &mut lane.map, &batch.descriptor, None)
                        .await?
                };
                if batch.descriptor.map().is_none() && !batch.new_epoch {
                    // TS parameter sets may be carried from an earlier segment
                    // of the same epoch, just as they are during initial input.
                    raw.sps = raw.sps.or_else(|| batch.data.sps.clone());
                    raw.pps = raw.pps.or_else(|| batch.data.pps.clone());
                    raw.vps = raw.vps.or_else(|| batch.data.vps.clone());
                    raw.width = raw.width.or(batch.data.width);
                    raw.height = raw.height.or(batch.data.height);
                }
                if let (Some(raw_anchor), Some(saved_anchor)) =
                    (raw.packed_anchor, batch.data.packed_anchor)
                {
                    if raw_anchor != saved_anchor % (1 << 33) {
                        return Err(fail(EngineErrorKind::ResumeConflict));
                    }
                    raw.packed_anchor = Some(saved_anchor);
                }
                let mut actual_config = Vec::new();
                let mut saved_config = Vec::new();
                raw.put(&mut actual_config);
                batch.data.put(&mut saved_config);
                if !evicted && actual_config != saved_config {
                    return Err(fail(EngineErrorKind::ResumeConflict));
                }
                let mut payloads = std::collections::HashMap::<[u8; 32], VecDeque<Vec<u8>>>::new();
                for packet in raw.packets.drain(..) {
                    payloads
                        .entry(crate::resume::digest(&packet.data))
                        .or_default()
                        .push_back(packet.data);
                }
                for _ in 0..samples {
                    let digest: [u8; 32] = StateCodec::get(&mut r).map_err(|_| bad())?;
                    let data = if evicted {
                        vec![]
                    } else {
                        payloads
                            .get_mut(&digest)
                            .and_then(|p| p.pop_front())
                            .ok_or_else(|| fail(EngineErrorKind::ResumeConflict))?
                    };
                    let packet = (|| -> DecodeResult<EncodedPacket> {
                        Ok(EncodedPacket {
                            data,
                            kind: StateCodec::get(&mut r)?,
                            timing: StateCodec::get(&mut r)?,
                            pts_90k: StateCodec::get(&mut r)?,
                            dts_90k: StateCodec::get(&mut r)?,
                            duration: StateCodec::get(&mut r)?,
                            is_key: StateCodec::get(&mut r)?,
                            is_length_prefixed: StateCodec::get(&mut r)?,
                        })
                    })()
                    .map_err(|_| bad())?;
                    if packet.timing.is_some_and(|t| t.timescale == 0) {
                        return Err(bad());
                    }
                    batch.data.packets.push(packet);
                }
                batch.shift = sub(batch.shift, collapsed)?;
                if evicted {
                    // The checkpoint may retain only a trimmed tail of a resource.
                    // Report that exact interval, never the whole manifest duration.
                    let mut start = None;
                    let mut end = None;
                    for packet in &batch.data.packets {
                        let t = packet.timing.ok_or_else(bad)?;
                        let a = add(pts(packet), batch.shift)?;
                        let duration = if t.duration == 0 {
                            lane.delta
                                .ok_or_else(|| fail(EngineErrorKind::MissingTailDuration))?
                        } else {
                            MediaTime {
                                ticks: i128::from(t.duration),
                                timescale: t.timescale,
                            }
                        };
                        let b = add(a, duration)?;
                        start = Some(start.map_or(Ok(a), |old| min(old, a))?);
                        end = Some(end.map_or(Ok(b), |old| max(old, b))?);
                    }
                    if let (Some(start), Some(end)) = (start, end) {
                        let declared_gap = batch
                            .descriptor
                            .duration()
                            .media_time()
                            .map_err(media_error)?;
                        let duration = sub(end, start)?;
                        batch.descriptor = SegmentDescriptor::recovery_gap(
                            slot.clone(),
                            crate::playlist::PlaylistDuration::from_time(duration)
                                .map_err(media_error)?,
                        );
                        batch.shift = start;
                        if session.options.gaps == GapPolicy::Collapse {
                            // A/V packet endpoints need not coincide. Collapse
                            // the declared common interval just as normal GAP
                            // admission does, rather than the union of tails.
                            collapsed_intervals[input].push((add(start, collapsed)?, declared_gap));
                            collapsed = add(collapsed, declared_gap)?;
                        }
                    }
                    batch.data = DemuxOutput::default();
                    batch.changed = false;
                    lane.aac.next = None;
                    lane.split_pending |= session.options.missing == MissingSegmentPolicy::Split;
                }
                slots.push(slot);
                lane.pending.push_back(batch);
            }
            if collapsed.ticks != 0 {
                lane.shift = sub(lane.shift, collapsed)?;
                lane.end = sub(lane.end, collapsed)?;
                lane.mapping_dirty = true;
            }
            engine.lanes.push(lane);
        }
        // Collapse is meaningful across inputs only when the evicted intervals
        // establish the same common clock. Check before any output acquisition.
        for intervals in collapsed_intervals.iter().skip(1) {
            if intervals.len() != collapsed_intervals[0].len() {
                return Err(fail(EngineErrorKind::TimelineAmbiguous));
            }
            for ((start, duration), (other_start, other_duration)) in
                intervals.iter().zip(&collapsed_intervals[0])
            {
                if !cmp(*start, *other_start)?.is_eq() || !cmp(*duration, *other_duration)?.is_eq()
                {
                    return Err(fail(EngineErrorKind::TimelineAmbiguous));
                }
            }
        }
        let reason: Option<EngineEndReason> = StateCodec::get(&mut r).map_err(|_| bad())?;
        for (index, slots) in pending_slots.iter().enumerate() {
            let mut progress = EngineInputProgress::get(&mut r).map_err(|_| bad())?;
            let generation: Option<u64> = StateCodec::get(&mut r).map_err(|_| bad())?;
            let revision: Option<u64> = StateCodec::get(&mut r).map_err(|_| bad())?;
            let media_sequence: Option<u64> = StateCodec::get(&mut r).map_err(|_| bad())?;
            let last: Option<u64> = StateCodec::get(&mut r).map_err(|_| bad())?;
            let restart: Option<u64> = StateCodec::get(&mut r).map_err(|_| bad())?;
            let ended = bool::get(&mut r).map_err(|_| bad())?;
            let required: Vec<(SegmentSlot, [u8; 32], crate::playlist::PlaylistDuration)> =
                StateCodec::get(&mut r).map_err(|_| bad())?;
            let mut evicted = Vec::new();
            for (slot, digest, duration) in &required {
                let (descriptor, synthesized) = find(slot, *digest, *duration)?;
                if synthesized {
                    evicted.push(descriptor);
                }
            }
            let mut state = session.shared.inner.lock().unwrap();
            let lane = &mut state.lanes[index];
            if terminal {
                lane.generation = generation;
                lane.queue.clear();
            } else if lane.revision < revision || lane.media_sequence < media_sequence {
                return Err(fail(EngineErrorKind::ResumeConflict));
            }
            if lane.id != progress.input || lane.generation != generation {
                return Err(fail(EngineErrorKind::ResumeConflict));
            }
            let newly_accepted = lane
                .queue
                .iter()
                .filter(|d| last.is_none_or(|n| d.slot().sequence() > n))
                .count() as u64;
            progress.discovered = progress
                .discovered
                .checked_add(newly_accepted)
                .ok_or_else(bad)?;
            progress.accepted = progress
                .accepted
                .checked_add(newly_accepted)
                .ok_or_else(bad)?;
            lane.revision = lane.revision.max(revision);
            lane.media_sequence = lane.media_sequence.max(media_sequence);
            lane.last = lane.last.max(last);
            lane.restart = restart;
            lane.queue.retain(|d| {
                !slots.contains(d.slot())
                    && progress.watermark.as_ref().is_none_or(|w| {
                        d.slot().generation() > w.generation()
                            || (d.slot().generation() == w.generation()
                                && d.slot().sequence() > w.sequence())
                    })
            });
            for descriptor in evicted.into_iter().rev() {
                lane.queue.push_front(descriptor);
            }
            lane.progress = progress;
            lane.ended |= ended;
            lane.outstanding = lane.queue.len() + slots.len();
        }
        {
            let mut state = session.shared.inner.lock().unwrap();
            state.reason = reason;
            state.queued = state.lanes.iter().map(|l| l.queue.len()).sum();
            state.metadata = state
                .lanes
                .iter()
                .flat_map(|l| &l.queue)
                .map(metadata_bytes)
                .sum();
        }
        if let Some(multi) = &session.multi {
            let mut multi = multi.lock().unwrap();
            multi.tracks = Vec::get(&mut r).map_err(|_| bad())?;
            multi.track_history_truncated = bool::get(&mut r).map_err(|_| bad())?;
            multi.restore_subtitles(
                &mut r,
                terminal,
                session.resources.provenance(),
                session
                    .recovery
                    .as_ref()
                    .and_then(|c| c.sidecars.as_ref())
                    .map(|s| s.next_receipt),
            )?;
        } else {
            return Err(bad());
        }
        if !r.0.is_empty() {
            return Err(bad());
        }
        engine.budget(session)?;
        if engine.bytes != 0 {
            let _ = engine.report(session)?;
        }
        Ok(engine)
    }
}
