use super::*;
use sha2::{Digest, Sha256};

#[derive(Clone)]
pub(super) struct ResourceRecord {
    pub input: usize,
    pub segment: usize,
    pub hash: [u8; 32],
    pub config: DemuxOutput,
    pub config_id: [u8; 32],
    pub lanes: [Option<super::catalog::Lane>; 2],
    pub coverage: Vec<PresentationRange>,
    pub rap_before: Option<MediaTime>,
    pub rap_after: Option<MediaTime>,
    pub first_video: Option<(MediaTime, bool)>,
}
#[derive(Clone)]
pub(super) struct SampleRecord {
    pub resource: usize,
    pub sample: usize,
    pub track: usize,
    pub timing: PacketTiming,
    pub public_dts: MediaTime,
    pub public_pts: MediaTime,
    pub duration: MediaTime,
    pub rap: bool,
    pub kind: StreamKind,
}
pub(super) struct Part {
    pub samples: Vec<SampleRecord>,
    pub tracks: Vec<FragmentedTrack>,
    pub track_keys: Vec<(usize, bool)>,
    pub origin: MediaTime,
    pub reason: TimelineSplitReason,
    pub count: usize,
    pub range: PresentationRange,
    pub mappings: Vec<EpochMapping>,
    pub resource_ids: std::collections::BTreeSet<usize>,
}
pub(super) struct Plan {
    pub resources: Vec<ResourceRecord>,
    pub parts: Vec<Part>,
    pub actual: PresentationRange,
    pub selected: PresentationRange,
    pub gaps: Vec<PresentationRange>,
    pub reads: u64,
    pub bytes: u64,
    pub peak_samples: usize,
    pub peak_resources: usize,
}

impl TimelinePreparedTransmux {
    pub(super) async fn read_resource(
        &self,
        input: usize,
        segment: usize,
        map: &mut Option<ClearResource>,
        expected: Option<[u8; 32]>,
    ) -> TimelineResult<(DemuxOutput, [u8; 32], u64)> {
        self.options.check()?;
        let selected = &self.inputs[input];
        let descriptor = &selected.snapshot.segments()[segment];
        let operation = async {
            if descriptor.map().is_some() {
                let request = ResourceRequest::from_validated(&selected.snapshot, segment, true)
                    .map_err(resource_error)?;
                self.resources
                    .read_map_cached(selected.source.clone(), request, map)
                    .await
                    .map_err(resource_error)?;
            } else {
                *map = None;
            }
            let request = ResourceRequest::from_validated(&selected.snapshot, segment, false)
                .map_err(resource_error)?;
            let bytes = self
                .resources
                .read(selected.source.clone(), request)
                .await
                .map_err(resource_error)?;
            let mut hash = Sha256::new();
            if let Some(init) = map.as_ref() {
                hash.update(init.bytes());
            }
            hash.update(bytes.bytes());
            let hash: [u8; 32] = hash.finalize().into();
            if expected.is_some_and(|expected| expected != hash) {
                return Err(fail(TimelineErrorKind::ResourceChanged));
            }
            let mut data = if let Some(init) = map.as_ref() {
                crate::isobmff::demux_isobmff_with_hook(
                    init.bytes(),
                    bytes.bytes(),
                    &mut crate::raw_sample::ClearSamples,
                    &|| check_cancel(self.options.cancel.as_ref()),
                )
                .await
            } else {
                crate::mpeg_ts::demux_ts_with_hook(
                    bytes.bytes(),
                    &mut crate::raw_sample::ClearSamples,
                    &|| check_cancel(self.options.cancel.as_ref()),
                )
                .await
            }
            .map_err(media_error)?;
            // TS drains trailing PES from a HashMap. Canonicalize track order
            // without sorting raw timestamps across a 33-bit wrap.
            data.packets.sort_by_key(|packet| match packet.kind {
                StreamKind::Avc => 0,
                StreamKind::Hevc => 1,
                StreamKind::Aac => 2,
            });
            let count = if descriptor.keys().is_clear() {
                bytes.bytes().len() as u64
            } else {
                (bytes.bytes().len() as u64 / 16 + 1) * 16
            };
            Ok((data, hash, count))
        };
        let result = if let Some(cancel) = &self.options.cancel {
            tokio::select! { biased;
                _ = cancel.cancelled() => Err(fail(TimelineErrorKind::Cancelled)),
                result = operation => result,
            }
        } else {
            operation.await
        };
        result.map_err(|mut error: TimelineSessionError| {
            error.slot = Some(descriptor.slot().clone());
            error
        })
    }

    pub(super) async fn plan(&self, fragmented: bool) -> TimelineResult<Plan> {
        self.options.check()?;
        let mut resources = Vec::new();
        let mut samples = Vec::new();
        let mut reads = 0u64;
        let mut bytes = 0u64;
        let mut peak_samples = 0;
        let mut peak_resources = 0;
        let mut wall_origin = None;
        let mut warmed = Vec::new();
        for input in 0..self.inputs.len() {
            let segment = self.inputs[input]
                .snapshot
                .segments()
                .iter()
                .position(|s| !s.gap())
                .ok_or_else(|| fail(TimelineErrorKind::EmptyRange))?;
            let mut map = None;
            let (mut data, hash, count) =
                self.read_resource(input, segment, &mut map, None).await?;
            if self.inputs.len() == 2 {
                select_track(
                    &mut data,
                    if input == 0 {
                        InputRole::Primary
                    } else {
                        InputRole::Audio
                    },
                );
            }
            warmed.push(Some((segment, data, hash, count, map)));
        }
        let reference = warmed
            .iter()
            .flatten()
            .flat_map(|(_, d, _, _, _)| &d.packets)
            .find_map(|p| p.timing.map(|_| source_pts(p)))
            .or_else(|| {
                warmed[0]
                    .as_ref()
                    .and_then(|(_, d, _, _, _)| d.packets.first())
                    .map(source_pts)
            })
            .ok_or_else(|| fail(TimelineErrorKind::EmptyRange))?;
        let mut beginnings = Vec::new();
        for (_, data, _, _, _) in warmed.iter().flatten() {
            for packet in &data.packets {
                let time = source_pts(packet);
                beginnings.push(if packet.timing.is_none() {
                    MediaTime {
                        ticks: unwrap_near(
                            time.ticks.rem_euclid(1i128 << 33),
                            rescale(reference, 90_000)?,
                        )
                        .map_err(|_| fail(TimelineErrorKind::TimelineAmbiguous))?,
                        timescale: 90_000,
                    }
                } else {
                    time
                });
            }
        }
        let mut common_origin = Some(min_time(beginnings.into_iter())?);
        let mut range_boundary = None;
        // Only metadata survives each resource. Clear media is dropped before the next read.
        for (input, warm) in warmed.iter_mut().enumerate() {
            let selected = &self.inputs[input];
            let mut map = None;
            let mut clock = TimestampClock::default();
            let mut epoch = None;
            let mut shift = zero();
            let mut gap_duration = zero();
            let mut previous_dts: [Option<MediaTime>; 2] = [None, None];
            let mut last_delta: [Option<MediaTime>; 2] = [None, None];
            let mut pending_tail: Option<usize> = None;
            let mut previous_config: Option<DemuxOutput> = None;
            let mut stop_after_segment = false;
            for (segment, descriptor) in selected.snapshot.segments().iter().enumerate() {
                self.options.check()?;
                if descriptor.gap() {
                    finish_tail(
                        &mut samples,
                        pending_tail.take(),
                        last_delta[0],
                        self.options.tail,
                    )?;
                    // A missing resource is not evidence for the preceding sample's
                    // duration. Resume inference between samples in the next resource.
                    previous_dts = [None, None];
                    last_delta = [None, None];
                    gap_duration = add(
                        gap_duration,
                        descriptor.duration().media_time().map_err(media_error)?,
                    )?;
                    continue;
                }
                let new_epoch = epoch != Some(descriptor.slot().epoch());
                if new_epoch {
                    finish_tail(
                        &mut samples,
                        pending_tail.take(),
                        last_delta[0],
                        self.options.tail,
                    )?;
                    clock = TimestampClock::default();
                    previous_dts = [None, None];
                    last_delta = [None, None];
                }
                let (mut data, hash, count) = if warm.as_ref().is_some_and(|v| v.0 == segment) {
                    let (_, data, hash, count, initial_map) = warm.take().unwrap();
                    map = initial_map;
                    (data, hash, count)
                } else {
                    self.read_resource(input, segment, &mut map, None).await?
                };
                reads = reads
                    .checked_add(1)
                    .ok_or_else(|| fail(TimelineErrorKind::TimeOverflow))?;
                bytes = bytes
                    .checked_add(count)
                    .ok_or_else(|| fail(TimelineErrorKind::TimeOverflow))?;
                if self.inputs.len() == 2 {
                    select_track(
                        &mut data,
                        if input == 0 {
                            InputRole::Primary
                        } else {
                            InputRole::Audio
                        },
                    );
                }
                // The replay index is the stable demux order. Only TS needs unwrapping.
                for packet in &mut data.packets {
                    if packet.timing.is_some() {
                        continue;
                    }
                    let lane = usize::from(matches!(packet.kind, StreamKind::Aac));
                    let last = if lane == 0 { clock.video } else { clock.audio };
                    // A declared epoch starts a new source clock domain. Only the
                    // initial epoch needs cross-input modulo alignment; borrowing
                    // that initial wrap cycle after a reset breaks explicit anchors.
                    let initial_reference = if new_epoch && epoch.is_some() {
                        packet.dts_90k
                    } else {
                        u64::try_from(rescale(common_origin.unwrap(), 90_000)?)
                            .unwrap_or(packet.dts_90k)
                    };
                    let reference = last
                        .or(clock.video)
                        .or(clock.audio)
                        .unwrap_or(initial_reference);
                    let raw = i128::from(packet.dts_90k);
                    let dts = unwrap_near(raw, i128::from(reference))
                        .map_err(|_| fail(TimelineErrorKind::TimelineAmbiguous))?;
                    let cts = unwrap_near(packet.pts_90k.rem_euclid(1i128 << 33), raw)
                        .map_err(|_| fail(TimelineErrorKind::TimelineAmbiguous))?
                        - raw;
                    if last.is_some_and(|v| dts <= i128::from(v)) {
                        return Err(fail(TimelineErrorKind::TimelineAmbiguous));
                    }
                    packet.dts_90k = u64::try_from(dts)
                        .map_err(|_| fail(TimelineErrorKind::TimelineAmbiguous))?;
                    packet.pts_90k = dts
                        .checked_add(cts)
                        .ok_or_else(|| fail(TimelineErrorKind::TimeOverflow))?;
                    if lane == 0 {
                        clock.video = Some(packet.dts_90k);
                    } else {
                        clock.audio = Some(packet.dts_90k);
                    }
                }
                let first = data
                    .packets
                    .first()
                    .ok_or_else(|| fail(TimelineErrorKind::Media))?;
                let first_time = source_pts(first);
                if common_origin.is_none() {
                    common_origin = Some(first_time);
                }
                if epoch.is_none() {
                    shift = sub(zero(), common_origin.unwrap())?;
                }
                let explicit_anchor = self.options.anchors.iter().find(|a| {
                    &a.input == selected.snapshot.context().input_id()
                        && a.epoch == descriptor.slot().epoch()
                });
                if let Some(anchor) = explicit_anchor {
                    shift = sub(anchor.public, anchor.source)?;
                }
                if let Some(pdt) = descriptor.program_date_time() {
                    let wall = parse_pdt(pdt)?;
                    if wall_origin.is_none() {
                        wall_origin = Some(sub(wall, add(first_time, shift)?)?);
                    }
                    let candidate = sub(sub(wall, wall_origin.unwrap())?, first_time)?;
                    if (!new_epoch || explicit_anchor.is_some())
                        && cmp(candidate, shift)? != Ordering::Equal
                    {
                        // HLS PDT precision is commonly milliseconds; allow one declared millisecond.
                        let difference = sub(candidate, shift)?;
                        if rescale(difference, 1_000_000_000)?.unsigned_abs() > 1_000_000 {
                            return Err(fail(TimelineErrorKind::TimelineAmbiguous));
                        }
                    }
                    // PDT rounding within an epoch must not move an already chosen
                    // affine mapping. A declared anchor has the same precedence.
                    if new_epoch && explicit_anchor.is_none() {
                        shift = candidate;
                    }
                } else if new_epoch && epoch.is_some() && explicit_anchor.is_none() {
                    let end = max_time(
                        samples
                            .iter()
                            .filter(|s| s.track / 2 == input)
                            .map(|s| add(s.public_pts, s.duration))
                            .collect::<TimelineResult<Vec<_>>>()?
                            .into_iter(),
                    )?;
                    if cmp(add(first_time, shift)?, end)? == Ordering::Less {
                        if self.inputs.len() != 1 {
                            return Err(fail(TimelineErrorKind::TimelineAmbiguous));
                        }
                        shift = sub(add(end, gap_duration)?, first_time)?;
                    }
                }
                epoch = Some(descriptor.slot().epoch());
                gap_duration = zero();
                let resource = resources.len();
                self.options
                    .planning
                    .admit(samples.len(), usize::from(!samples.is_empty()) + 1)?;
                if let Some(old) = &previous_config
                    && data.saw_video
                    && !new_epoch
                    && descriptor.map().is_none()
                    && data
                        .packets
                        .iter()
                        .find(|packet| !matches!(packet.kind, StreamKind::Aac))
                        .is_some_and(|packet| {
                            matches!(packet.kind, StreamKind::Hevc) == old.vps.is_some()
                        })
                {
                    data.sps = data.sps.or_else(|| old.sps.clone());
                    data.pps = data.pps.or_else(|| old.pps.clone());
                    data.vps = data.vps.or_else(|| old.vps.clone());
                    data.width = data.width.or(old.width);
                    data.height = data.height.or(old.height);
                }
                let mut config = None;
                check_media_config(&mut config, &data).map_err(media_error)?;
                let config = config.unwrap();
                let config_id =
                    config_digest(&build_fragmented_tracks(&config).map_err(media_error)?);
                previous_config = Some(config.clone());
                resources.push(ResourceRecord {
                    input,
                    segment,
                    hash,
                    config,
                    config_id,
                    lanes: [None, None],
                    coverage: Vec::new(),
                    rap_before: None,
                    rap_after: None,
                    first_video: None,
                });
                peak_resources = peak_resources.max(usize::from(!samples.is_empty()) + 1);
                for (sample, packet) in data.packets.iter().enumerate() {
                    self.options.planning.admit(
                        samples.len() + 1,
                        usize::from(samples.iter().any(|s| s.resource != resource)) + 1,
                    )?;
                    let audio = matches!(packet.kind, StreamKind::Aac);
                    let slot = usize::from(audio);
                    let timing = packet.timing.unwrap_or(PacketTiming {
                        edit_offset: 0,
                        timescale: 90_000,
                        dts: i128::from(packet.dts_90k),
                        pts: packet.pts_90k,
                        duration: if audio {
                            u32::try_from(
                                packet.duration * 90_000
                                    / u64::from(data.sample_rate.unwrap_or(48_000)),
                            )
                            .map_err(|_| fail(TimelineErrorKind::TimeOverflow))?
                        } else {
                            0
                        },
                    });
                    let dts = MediaTime {
                        ticks: timing.dts,
                        timescale: timing.timescale,
                    };
                    if let Some(previous) = previous_dts[slot] {
                        let delta = sub(dts, previous)?;
                        if delta.ticks <= 0
                            || (packet.timing.is_none() && rescale(delta, 90_000)? >= 1i128 << 32)
                        {
                            return Err(fail(TimelineErrorKind::TimelineAmbiguous));
                        }
                        if !audio {
                            if let Some(tail) = pending_tail.take() {
                                samples[tail].duration = delta;
                                samples[tail].timing.duration =
                                    u32::try_from(rescale(delta, samples[tail].timing.timescale)?)
                                        .map_err(|_| fail(TimelineErrorKind::TimeOverflow))?;
                            }
                            last_delta[slot] = Some(delta);
                        }
                    }
                    previous_dts[slot] = Some(dts);
                    let pts = MediaTime {
                        ticks: timing.pts,
                        timescale: timing.timescale,
                    };
                    let duration = if audio && packet.timing.is_none() {
                        MediaTime {
                            ticks: i128::from(packet.duration),
                            timescale: data.sample_rate.unwrap_or(48_000),
                        }
                    } else {
                        MediaTime {
                            ticks: i128::from(timing.duration),
                            timescale: timing.timescale,
                        }
                    };
                    let public_pts = add(pts, shift)?;
                    if timing.duration == 0 && !audio {
                        pending_tail = Some(samples.len());
                    }
                    let rap = independent(packet)?;
                    if let Some(range) = self.options.range {
                        let limit = range_boundary.unwrap_or(range.end);
                        if !audio
                            && input == 0
                            && rap
                            && cmp(public_pts, range.end)? != Ordering::Less
                        {
                            range_boundary.get_or_insert(public_pts);
                            stop_after_segment = true;
                        } else if audio
                            && (input > 0 || !data.saw_video)
                            && cmp(public_pts, limit)? != Ordering::Less
                        {
                            stop_after_segment = true;
                        }
                    }
                    samples.push(SampleRecord {
                        resource,
                        sample,
                        track: input * 2 + slot,
                        timing,
                        public_dts: add(dts, shift)?,
                        public_pts,
                        duration,
                        rap,
                        kind: packet.kind,
                    });
                    peak_samples = peak_samples.max(samples.len());
                }
                // The next resource has resolved the preceding TS tail. Store only
                // its clock/configuration summary, then release its sample records.
                let count = samples
                    .iter()
                    .take_while(|s| s.resource != resource)
                    .count();
                if count > 0 {
                    if pending_tail.is_some_and(|i| i < count) {
                        finish_tail(
                            &mut samples,
                            pending_tail.take(),
                            last_delta[0],
                            self.options.tail,
                        )?;
                    }
                    let old = samples[0].resource;
                    super::catalog::summarize(
                        &mut resources[old],
                        &samples[..count],
                        self.options.range,
                    )?;
                    samples.drain(..count);
                    pending_tail = pending_tail.map(|i| i - count);
                }
                if stop_after_segment {
                    break;
                }
            }
            finish_tail(&mut samples, pending_tail, last_delta[0], self.options.tail)?;
            if let Some(first) = samples.first() {
                super::catalog::summarize(
                    &mut resources[first.resource],
                    &samples,
                    self.options.range,
                )?;
            }
            samples.clear();
        }
        let mut plan = build_catalog_plan(self, resources, fragmented, reads, bytes).await?;
        plan.peak_samples = plan.peak_samples.max(peak_samples);
        plan.peak_resources = plan.peak_resources.max(peak_resources);
        Ok(plan)
    }
}

fn finish_tail(
    samples: &mut [SampleRecord],
    tail: Option<usize>,
    last: Option<MediaTime>,
    policy: TailDurationPolicy,
) -> TimelineResult<()> {
    if let Some(index) = tail {
        let duration = last
            .or(match policy {
                TailDurationPolicy::Explicit(v) => Some(v),
                _ => None,
            })
            .ok_or_else(|| fail(TimelineErrorKind::MissingTailDuration))?;
        let sample = &mut samples[index];
        sample.duration = duration;
        sample.timing.duration = u32::try_from(rescale(duration, sample.timing.timescale)?)
            .map_err(|_| fail(TimelineErrorKind::TimeOverflow))?;
    }
    Ok(())
}
fn source_pts(packet: &EncodedPacket) -> MediaTime {
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
pub(super) fn min_time(mut values: impl Iterator<Item = MediaTime>) -> TimelineResult<MediaTime> {
    values
        .try_fold(None, |old, value| -> TimelineResult<_> {
            Ok(Some(match old {
                Some(old) if cmp(old, value)? != Ordering::Greater => old,
                _ => value,
            }))
        })?
        .ok_or_else(|| fail(TimelineErrorKind::EmptyRange))
}
pub(super) fn max_time(mut values: impl Iterator<Item = MediaTime>) -> TimelineResult<MediaTime> {
    values
        .try_fold(None, |old, value| -> TimelineResult<_> {
            Ok(Some(match old {
                Some(old) if cmp(old, value)? != Ordering::Less => old,
                _ => value,
            }))
        })?
        .ok_or_else(|| fail(TimelineErrorKind::EmptyRange))
}
pub(super) fn independent(packet: &EncodedPacket) -> TimelineResult<bool> {
    if matches!(packet.kind, StreamKind::Aac) {
        return Ok(true);
    }
    let bytes = if packet.is_length_prefixed {
        packet.data.clone()
    } else {
        avc::annex_b_to_length_prefixed(&packet.data).map_err(media_error)?
    };
    let mut offset = 0;
    let mut safe = false;
    while offset < bytes.len() {
        let prefix = bytes
            .get(offset..offset + 4)
            .ok_or_else(|| fail(TimelineErrorKind::Media))?;
        let length = u32::from_be_bytes(prefix.try_into().unwrap()) as usize;
        offset += 4;
        let end = offset
            .checked_add(length)
            .ok_or_else(|| fail(TimelineErrorKind::TimeOverflow))?;
        let nal = bytes
            .get(offset..end)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| fail(TimelineErrorKind::Media))?;
        safe |= match packet.kind {
            StreamKind::Avc => nal[0] & 31 == 5,
            StreamKind::Hevc => matches!((nal[0] >> 1) & 63, 16..=20),
            _ => false,
        };
        offset = end;
    }
    Ok(safe)
}

async fn build_catalog_plan(
    session: &TimelinePreparedTransmux,
    resources: Vec<ResourceRecord>,
    fragmented: bool,
    reads: u64,
    bytes: u64,
) -> TimelineResult<Plan> {
    let coverage =
        super::catalog::coverage(resources.iter().flat_map(|r| r.coverage.iter().copied()))?;
    let first = coverage
        .first()
        .ok_or_else(|| fail(TimelineErrorKind::EmptyRange))?;
    let end = coverage.last().unwrap().end;
    let mut start = first.start;
    let mut selected_end = end;
    let video = resources.iter().any(|r| r.first_video.is_some());
    if let Some(range) = session.options.range {
        if cmp(range.start, end)? != Ordering::Less {
            return Err(fail(TimelineErrorKind::OutOfBounds));
        }
        if !coverage
            .iter()
            .any(|c| c.intersects(&range).unwrap_or(false))
        {
            return Err(fail(TimelineErrorKind::EmptyRange));
        }
        if video {
            start = max_time(resources.iter().filter_map(|r| r.rap_before))
                .map_err(|_| fail(TimelineErrorKind::NoRandomAccessPoint))?;
            selected_end = min_time(resources.iter().filter_map(|r| r.rap_after)).unwrap_or(end);
        } else {
            start = range.start;
            if cmp(range.end, end)? == Ordering::Less {
                selected_end = range.end;
            }
        }
    } else if video {
        let first_video = resources
            .iter()
            .filter_map(|r| r.first_video)
            .min_by(|a, b| cmp(a.0, b.0).unwrap_or(Ordering::Equal))
            .unwrap();
        if !first_video.1 {
            return Err(fail(TimelineErrorKind::NoRandomAccessPoint));
        }
    }
    let selected = PresentationRange {
        start,
        end: selected_end,
    };
    let gaps: Vec<_> = coverage
        .windows(2)
        .map(|pair| PresentationRange {
            start: pair[0].end,
            end: pair[1].start,
        })
        .filter(|gap| gap.intersects(&selected).unwrap_or(false))
        .collect();
    let mut cursor = super::catalog::Cursor::new(session.inputs.len());
    let mut parts = Vec::new();
    let mut current: Option<Part> = None;
    let mut reason = TimelineSplitReason::Initial;
    let mut configurations = vec![None; session.inputs.len()];
    let mut ends = [None; 4];
    while let Some(sample) = cursor.next(session, &resources, selected).await? {
        let resource = &resources[sample.resource];
        let changed = configurations[resource.input].is_some_and(|v| v != resource.config_id);
        let dts = sample_time(&sample, sample.public_dts, &gaps, session.options.gaps)?;
        let gap = ends[sample.track].is_some_and(|end| {
            cmp(dts, end).is_ok_and(|v| v == Ordering::Greater)
                && rescale(sub(dts, end).unwrap(), sample.timing.timescale).unwrap_or(2) > 1
        });
        if changed || (gap && !fragmented) {
            if session.options.changes != TimelineChangePolicy::Split {
                return Err(fail(if changed {
                    TimelineErrorKind::ConfigurationChanged
                } else {
                    TimelineErrorKind::UnrepresentableGap
                }));
            }
            if !sample.rap || (video && matches!(sample.kind, StreamKind::Aac)) {
                return Err(fail(TimelineErrorKind::NoRandomAccessPoint));
            }
            if let Some(part) = current.take() {
                parts.push(finish_part(part)?);
            }
            reason = if changed {
                TimelineSplitReason::ConfigurationChanged
            } else {
                TimelineSplitReason::Gap
            };
            ends = [None; 4];
        }
        if let Some(end) = ends[sample.track]
            && cmp(dts, end)? == Ordering::Less
            && rescale(sub(end, dts)?, sample.timing.timescale)? > 1
        {
            return Err(fail(TimelineErrorKind::TimelineAmbiguous));
        }
        configurations[resource.input] = Some(resource.config_id);
        ends[sample.track] = Some(add(dts, sample.duration)?);
        let end = add(sample.public_pts, sample.duration)?;
        let part = current.get_or_insert_with(|| Part {
            samples: Vec::new(),
            tracks: Vec::new(),
            track_keys: Vec::new(),
            origin: dts,
            reason,
            count: 0,
            range: PresentationRange {
                start: sample.public_pts,
                end,
            },
            mappings: Vec::new(),
            resource_ids: Default::default(),
        });
        if cmp(dts, part.origin)? == Ordering::Less {
            part.origin = dts;
        }
        if cmp(sample.public_pts, part.range.start)? == Ordering::Less {
            part.range.start = sample.public_pts;
        }
        if cmp(end, part.range.end)? == Ordering::Greater {
            part.range.end = end;
        }
        let key = (resource.input, matches!(sample.kind, StreamKind::Aac));
        let track = if let Some(track) = part.track_keys.iter().position(|k| *k == key) {
            track
        } else {
            part.track_keys.push(key);
            part.tracks.push(
                build_fragmented_tracks(&resource.config)
                    .map_err(media_error)?
                    .into_iter()
                    .find(|t| {
                        matches!(t.kind, crate::mp4::FragmentedTrackKind::Audio { .. }) == key.1
                    })
                    .ok_or_else(|| fail(TimelineErrorKind::Media))?,
            );
            part.samples.push(sample.clone());
            part.tracks.len() - 1
        };
        let snapshot = &session.inputs[resource.input].snapshot;
        let descriptor = &snapshot.segments()[resource.segment];
        part.mappings.push(EpochMapping {
            input: snapshot.context().input_id().clone(),
            track: track as u32 + 1,
            epoch: descriptor.slot().epoch(),
            source_decode_start: MediaTime {
                ticks: sample.timing.dts,
                timescale: sample.timing.timescale,
            },
            config_id: resource.config_id,
            source_origin: MediaTime {
                ticks: sample.timing.pts,
                timescale: sample.timing.timescale,
            },
            public: PresentationRange {
                start: sample.public_pts,
                end,
            },
            output_start: output_time(sample.public_pts, &gaps, session.options.gaps)?,
            output: parts.len(),
            wrap_anchor: descriptor.map().is_none().then_some(MediaTime {
                ticks: sample.timing.dts,
                timescale: sample.timing.timescale,
            }),
            pdt: descriptor.program_date_time().map(str::to_owned),
        });
        coalesce_mappings(&mut part.mappings)?;
        part.resource_ids.insert(sample.resource);
        part.count += 1;
    }
    if let Some(part) = current {
        parts.push(finish_part(part)?);
    }
    let actual = PresentationRange {
        start: min_time(parts.iter().map(|p| p.range.start))?,
        end: max_time(parts.iter().map(|p| p.range.end))?,
    };
    Ok(Plan {
        resources,
        parts,
        actual,
        selected,
        gaps,
        reads: reads + cursor.reads,
        bytes: bytes + cursor.bytes,
        peak_samples: cursor.peak_samples,
        peak_resources: cursor.peak_resources,
    })
}

pub(super) fn coalesce_mappings(mappings: &mut Vec<EpochMapping>) -> TimelineResult<()> {
    mappings.sort_by(|a, b| {
        a.track
            .cmp(&b.track)
            .then_with(|| cmp(a.public.start, b.public.start).unwrap_or(Ordering::Equal))
    });
    let mut merged: Vec<EpochMapping> = Vec::new();
    for mapping in mappings.drain(..) {
        if let Some(previous) = merged.last_mut()
            && previous.input == mapping.input
            && previous.track == mapping.track
            && previous.epoch == mapping.epoch
            && previous.config_id == mapping.config_id
            && cmp(previous.public.end, mapping.public.start)? != Ordering::Less
            && cmp(
                previous.presentation_to_output(mapping.public.start)?,
                mapping.output_start,
            )? == Ordering::Equal
            && cmp(
                previous.source_to_presentation(mapping.source_origin)?,
                mapping.public.start,
            )? == Ordering::Equal
        {
            if cmp(mapping.public.end, previous.public.end)? == Ordering::Greater {
                previous.public.end = mapping.public.end;
            }
            if cmp(mapping.source_decode_start, previous.source_decode_start)? == Ordering::Less {
                previous.source_decode_start = mapping.source_decode_start;
            }
        } else {
            merged.push(mapping);
        }
    }
    *mappings = merged;
    Ok(())
}
fn finish_part(mut part: Part) -> TimelineResult<Part> {
    let mut order: Vec<_> = (0..part.tracks.len()).collect();
    order.sort_by_key(|i| part.track_keys[*i]);
    let old_tracks = part.tracks.clone();
    let old_keys = part.track_keys.clone();
    for (new, old) in order.iter().copied().enumerate() {
        part.tracks[new] = old_tracks[old].clone();
        part.tracks[new].track_id = new as u32 + 1;
        part.track_keys[new] = old_keys[old];
    }
    for mapping in &mut part.mappings {
        mapping.track = order
            .iter()
            .position(|old| *old == mapping.track as usize - 1)
            .unwrap() as u32
            + 1;
        mapping.output_start = sub(mapping.output_start, part.origin)?;
    }
    part.mappings.sort_by(|a, b| {
        cmp(a.public.start, b.public.start)
            .unwrap_or(Ordering::Equal)
            .then(a.track.cmp(&b.track))
    });
    Ok(part)
}

pub(super) fn sample_time(
    sample: &SampleRecord,
    time: MediaTime,
    gaps: &[PresentationRange],
    policy: GapPolicy,
) -> TimelineResult<MediaTime> {
    add(
        time,
        sub(
            output_time(sample.public_pts, gaps, policy)?,
            sample.public_pts,
        )?,
    )
}
pub(super) fn output_time(
    time: MediaTime,
    gaps: &[PresentationRange],
    policy: GapPolicy,
) -> TimelineResult<MediaTime> {
    let mut result = time;
    if policy == GapPolicy::Collapse {
        for gap in gaps {
            if cmp(gap.end, time)? != Ordering::Greater {
                result = sub(result, sub(gap.end, gap.start)?)?;
            }
        }
    }
    Ok(result)
}
/// Checked RFC3339 mapping, independent of host timezone. No floating-point dates.
fn parse_pdt(text: &str) -> TimelineResult<MediaTime> {
    let invalid = || fail(TimelineErrorKind::TimelineAmbiguous);
    let n = |start: usize, end: usize| {
        let digits = text.get(start..end).ok_or_else(invalid)?;
        if !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid());
        }
        digits.parse::<i128>().map_err(|_| invalid())
    };
    let (year, month, day, hour, minute, second) = (
        n(0, 4)?,
        n(5, 7)?,
        n(8, 10)?,
        n(11, 13)?,
        n(14, 16)?,
        n(17, 19)?,
    );
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_days = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if !text.is_ascii()
        || text.as_bytes().get(4) != Some(&b'-')
        || text.as_bytes().get(7) != Some(&b'-')
        || !matches!(text.as_bytes().get(10), Some(b'T' | b't'))
        || text.as_bytes().get(13) != Some(&b':')
        || text.as_bytes().get(16) != Some(&b':')
        || !(1..=12).contains(&month)
        || !(1..=month_days).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return Err(invalid());
    }
    let y = year - i128::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = month + if month > 2 { -3 } else { 9 };
    let days =
        era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + (153 * m + 2) / 5 + day - 1 - 719468;
    let mut rest = text.get(19..).ok_or_else(invalid)?;
    let mut nano = 0;
    if rest.starts_with('.') {
        rest = &rest[1..];
        let length = rest.bytes().take_while(u8::is_ascii_digit).count();
        if length == 0 || length > 9 {
            return Err(invalid());
        }
        nano = rest[..length].parse::<i128>().map_err(|_| invalid())?
            * 10i128.pow((9 - length) as u32);
        rest = &rest[length..];
    }
    let offset = if matches!(rest, "Z" | "z") {
        0
    } else {
        if rest.len() != 6
            || !matches!(rest.as_bytes()[0], b'+' | b'-')
            || rest.as_bytes()[3] != b':'
            || !rest.as_bytes()[1..3].iter().all(u8::is_ascii_digit)
            || !rest.as_bytes()[4..6].iter().all(u8::is_ascii_digit)
        {
            return Err(invalid());
        }
        let h = rest[1..3].parse::<i128>().map_err(|_| invalid())?;
        let m = rest[4..6].parse::<i128>().map_err(|_| invalid())?;
        if h > 23 || m > 59 {
            return Err(invalid());
        }
        (h * 3600 + m * 60) * if rest.starts_with('-') { -1 } else { 1 }
    };
    Ok(MediaTime {
        ticks: ((days * 24 + hour) * 3600 + minute * 60 + second - offset) * 1_000_000_000 + nano,
        timescale: 1_000_000_000,
    })
}

#[cfg(test)]
mod pdt_tests {
    use super::*;
    #[test]
    fn validates_calendar_delimiters_digits_and_timezone() {
        for date in [
            "2026-02-29T00:00:00Z",
            "2026-04-31T00:00:00Z",
            "2026/10/05T00:00:00Z",
            "2026-10-05T00/00/00Z",
            "2026-10-05T00:00:00++1:00",
            "2026-10-05T00:00:00+08:99",
            "2026-10-05T00:00:00.1234567890Z",
            "2026-10-05T00:00:00.Z",
            "2026-10-05T00:00:00+é:00",
            "2026-10-05T+1:00:00Z",
        ] {
            assert!(parse_pdt(date).is_err(), "{date}");
        }
        assert_eq!(
            parse_pdt("2024-02-29T08:00:00.123+08:00").unwrap(),
            parse_pdt("2024-02-29t00:00:00.123z").unwrap()
        );
        assert!(parse_pdt("1900-02-29T00:00:00Z").is_err());
        assert!(parse_pdt("2000-02-29T00:00:00Z").is_ok());
        assert_eq!(parse_pdt("1970-01-01T00:00:00Z").unwrap().ticks, 0);
    }
}
