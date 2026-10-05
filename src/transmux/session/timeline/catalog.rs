//! Resource-sized sample cursors. The catalog retains resource/clock summaries,
//! never a sample index for the duration of a selection.
use super::engine::*;
use super::*;
use std::collections::{HashMap, VecDeque};

#[derive(Clone)]
pub(super) struct Lane {
    pub first: PacketTiming,
    pub shift: MediaTime,
    pub tail: MediaTime,
}

pub(super) fn summarize(
    resource: &mut ResourceRecord,
    samples: &[SampleRecord],
    requested: Option<PresentationRange>,
) -> TimelineResult<()> {
    for audio in [false, true] {
        let mut lane = samples
            .iter()
            .filter(|s| matches!(s.kind, StreamKind::Aac) == audio);
        if let Some(first) = lane.next() {
            let last = lane.next_back().unwrap_or(first);
            resource.lanes[usize::from(audio)] = Some(Lane {
                first: first.timing,
                shift: sub(
                    first.public_pts,
                    MediaTime {
                        ticks: first.timing.pts,
                        timescale: first.timing.timescale,
                    },
                )?,
                tail: last.duration,
            });
        }
    }
    resource.coverage = coverage(
        samples
            .iter()
            .map(|s| {
                Ok(PresentationRange {
                    start: s.public_pts,
                    end: add(s.public_pts, s.duration)?,
                })
            })
            .collect::<TimelineResult<Vec<_>>>()?
            .into_iter(),
    )?;
    resource.first_video = samples
        .iter()
        .find(|s| !matches!(s.kind, StreamKind::Aac))
        .map(|s| (s.public_dts, s.rap));
    if let Some(range) = requested {
        for sample in samples
            .iter()
            .filter(|s| s.rap && !matches!(s.kind, StreamKind::Aac))
        {
            if cmp(sample.public_pts, range.start)? != Ordering::Greater
                && resource.rap_before.is_none_or(|old| {
                    cmp(sample.public_pts, old).is_ok_and(|v| v == Ordering::Greater)
                })
            {
                resource.rap_before = Some(sample.public_pts);
            }
            if cmp(sample.public_pts, range.end)? != Ordering::Less
                && resource.rap_after.is_none_or(|old| {
                    cmp(sample.public_pts, old).is_ok_and(|v| v == Ordering::Less)
                })
            {
                resource.rap_after = Some(sample.public_pts);
            }
        }
    }
    Ok(())
}

pub(super) struct Cursor {
    positions: [usize; 4],
    queues: [VecDeque<SampleRecord>; 4],
    loaded: HashMap<usize, DemuxOutput>,
    maps: Vec<Option<ClearResource>>,
    pub reads: u64,
    pub bytes: u64,
    pub peak_samples: usize,
    pub peak_resources: usize,
}
impl Cursor {
    pub fn new(inputs: usize) -> Self {
        Self {
            positions: [0; 4],
            queues: std::array::from_fn(|_| VecDeque::new()),
            loaded: HashMap::new(),
            maps: (0..inputs).map(|_| None).collect(),
            reads: 0,
            bytes: 0,
            peak_samples: 0,
            peak_resources: 0,
        }
    }
    pub async fn next(
        &mut self,
        session: &TimelinePreparedTransmux,
        resources: &[ResourceRecord],
        range: PresentationRange,
    ) -> TimelineResult<Option<SampleRecord>> {
        session.options.check()?;
        self.loaded.retain(|id, _| {
            (0..session.inputs.len() * 2).any(|lane| {
                let record = &resources[*id];
                record.input == lane / 2
                    && record.lanes[lane % 2].is_some()
                    && (self.positions[lane] <= *id
                        || self.queues[lane].iter().any(|s| s.resource == *id))
            })
        });
        for lane in 0..session.inputs.len() * 2 {
            while self.queues[lane].is_empty() {
                let Some(id) = (self.positions[lane]..resources.len()).find(|id| {
                    resources[*id].input == lane / 2
                        && resources[*id].lanes[lane % 2].is_some()
                        && resources[*id]
                            .coverage
                            .iter()
                            .any(|r| r.intersects(&range).unwrap_or(false))
                }) else {
                    self.positions[lane] = resources.len();
                    break;
                };
                self.positions[lane] = id + 1;
                let resource = &resources[id];
                if !self.loaded.contains_key(&id) {
                    session.options.planning.admit(
                        self.queues.iter().map(VecDeque::len).sum(),
                        self.loaded.len() + 1,
                    )?;
                    let (mut data, _, count) = session
                        .read_resource(
                            resource.input,
                            resource.segment,
                            &mut self.maps[resource.input],
                            Some(resource.hash),
                        )
                        .await?;
                    if session.inputs.len() == 2 {
                        select_track(
                            &mut data,
                            if resource.input == 0 {
                                InputRole::Primary
                            } else {
                                InputRole::Audio
                            },
                        );
                    }
                    self.reads += 1;
                    self.bytes += count;
                    self.loaded.insert(id, data);
                    self.peak_resources = self.peak_resources.max(self.loaded.len());
                }
                let data = &self.loaded[&id];
                let summary = resource.lanes[lane % 2].as_ref().unwrap();
                let mut samples = Vec::new();
                let mut reference = summary.first.dts;
                for (index, packet) in data
                    .packets
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| usize::from(matches!(p.kind, StreamKind::Aac)) == lane % 2)
                {
                    let audio = lane % 2 != 0;
                    let timing = if let Some(timing) = packet.timing {
                        timing
                    } else {
                        let raw = i128::from(packet.dts_90k);
                        let dts = unwrap_near(raw, reference)
                            .map_err(|_| fail(TimelineErrorKind::TimelineAmbiguous))?;
                        reference = dts;
                        PacketTiming {
                            edit_offset: 0,
                            timescale: 90_000,
                            dts,
                            pts: dts
                                + unwrap_near(packet.pts_90k.rem_euclid(1i128 << 33), raw)
                                    .map_err(|_| fail(TimelineErrorKind::TimelineAmbiguous))?
                                - raw,
                            duration: if audio {
                                u32::try_from(
                                    packet.duration * 90_000
                                        / u64::from(data.sample_rate.unwrap_or(48_000)),
                                )
                                .map_err(|_| fail(TimelineErrorKind::TimeOverflow))?
                            } else {
                                0
                            },
                        }
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
                    session.options.planning.admit(
                        self.queues.iter().map(VecDeque::len).sum::<usize>() + samples.len() + 1,
                        self.loaded.len(),
                    )?;
                    samples.push(SampleRecord {
                        resource: id,
                        sample: index,
                        track: lane,
                        timing,
                        public_dts: add(
                            MediaTime {
                                ticks: timing.dts,
                                timescale: timing.timescale,
                            },
                            summary.shift,
                        )?,
                        public_pts: add(
                            MediaTime {
                                ticks: timing.pts,
                                timescale: timing.timescale,
                            },
                            summary.shift,
                        )?,
                        duration,
                        rap: independent(packet)?,
                        kind: packet.kind,
                    });
                }
                self.peak_samples = self
                    .peak_samples
                    .max(self.queues.iter().map(VecDeque::len).sum::<usize>() + samples.len());
                for i in 0..samples.len() {
                    if samples[i].timing.duration == 0 {
                        let duration = if let Some(next) = samples.get(i + 1) {
                            sub(next.public_dts, samples[i].public_dts)?
                        } else {
                            summary.tail
                        };
                        samples[i].duration = duration;
                        samples[i].timing.duration =
                            u32::try_from(rescale(duration, samples[i].timing.timescale)?)
                                .map_err(|_| fail(TimelineErrorKind::TimeOverflow))?;
                    }
                }
                for sample in samples {
                    if (PresentationRange {
                        start: sample.public_pts,
                        end: add(sample.public_pts, sample.duration)?,
                    })
                    .intersects(&range)?
                    {
                        self.queues[lane].push_back(sample);
                    }
                }
                // A discarded prefix must not pin payload while seeking the next resource.
                if self.queues[lane].is_empty()
                    && (0..session.inputs.len() * 2).all(|other| {
                        resources[id].input != other / 2
                            || resources[id].lanes[other % 2].is_none()
                            || (self.positions[other] > id
                                && !self.queues[other].iter().any(|s| s.resource == id))
                    })
                {
                    self.loaded.remove(&id);
                }
            }
        }
        let mut next = None;
        for lane in 0..session.inputs.len() * 2 {
            if let Some(sample) = self.queues[lane].front()
                && next.is_none_or(|old: usize| {
                    cmp(
                        sample.public_dts,
                        self.queues[old].front().unwrap().public_dts,
                    )
                    .is_ok_and(|v| v == Ordering::Less)
                })
            {
                next = Some(lane);
            }
        }
        Ok(next.and_then(|lane| self.queues[lane].pop_front()))
    }
    pub fn packet(&self, sample: &SampleRecord) -> TimelineResult<EncodedPacket> {
        self.loaded
            .get(&sample.resource)
            .and_then(|data| data.packets.get(sample.sample))
            .cloned()
            .ok_or_else(|| fail(TimelineErrorKind::ResourceChanged))
    }
}

/// Merge coverage intervals, retaining only holes (report metadata), not samples.
pub(super) fn coverage(
    intervals: impl Iterator<Item = PresentationRange>,
) -> TimelineResult<Vec<PresentationRange>> {
    let mut ranges: Vec<_> = intervals.collect();
    ranges.sort_by(|a, b| cmp(a.start, b.start).unwrap_or(Ordering::Equal));
    let mut merged: Vec<PresentationRange> = Vec::new();
    for range in ranges {
        if let Some(previous) = merged.last_mut()
            && cmp(range.start, previous.end)? != Ordering::Greater
        {
            if cmp(range.end, previous.end)? == Ordering::Greater {
                previous.end = range.end;
            }
        } else {
            merged.push(range);
        }
    }
    Ok(merged)
}
