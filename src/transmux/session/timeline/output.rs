use super::engine::*;
use super::*;

#[derive(Debug, Clone)]
pub struct TimelineOutputRequest {
    index: usize,
    range: PresentationRange,
    reason: TimelineSplitReason,
    tracks: Vec<crate::TrackInfo>,
}
impl TimelineOutputRequest {
    pub fn index(&self) -> usize {
        self.index
    }
    pub fn presentation_range(&self) -> PresentationRange {
        self.range
    }
    pub fn reason(&self) -> TimelineSplitReason {
        self.reason
    }
    pub fn tracks(&self) -> &[crate::TrackInfo] {
        &self.tracks
    }
}
/// Returns a lease; its Drop must not publish/close/abort a caller-owned sink.
/// Neither the writer nor the acquisition future needs to implement Send.
pub trait TimelineWriterProvider {
    type Writer: AsyncWrite + Unpin;
    fn acquire<'a>(
        &'a mut self,
        request: TimelineOutputRequest,
    ) -> Pin<Box<dyn Future<Output = TimelineResult<Self::Writer>> + 'a>>;
}
/// Explicit paths prevent implicit overwrites/naming conventions between suboutputs.
pub trait TimelineFileProvider {
    fn acquire<'a>(
        &'a mut self,
        request: TimelineOutputRequest,
    ) -> Pin<Box<dyn Future<Output = TimelineResult<std::path::PathBuf>> + 'a>>;
}

fn request(index: usize, part: &Part) -> TimelineResult<TimelineOutputRequest> {
    Ok(TimelineOutputRequest {
        index,
        range: part.range,
        reason: part.reason,
        tracks: part.tracks.iter().map(|t| track_report(t, 0, 0)).collect(),
    })
}
type Replay = super::catalog::Cursor;
impl TimelinePreparedTransmux {
    pub async fn into_mp4_bytes(self) -> TimelineResult<(Vec<u8>, TimelineSessionReport)> {
        self.single()?;
        let (mut outputs, report) = self.into_mp4_outputs().await?;
        Ok((outputs.remove(0), report))
    }
    /// Collect classic MP4 suboutputs. All output bytes are retained by this API.
    pub async fn into_mp4_outputs(self) -> TimelineResult<(Vec<Vec<u8>>, TimelineSessionReport)> {
        let plan = self.plan(false).await?;
        let mut replay = Replay::new(self.inputs.len());
        let mut outputs = Vec::new();
        let mut reports = Vec::new();
        for (index, part) in plan.parts.iter().enumerate() {
            let result = self.classic(&plan, part, index, &mut replay).await;
            let (bytes, report) = result.map_err(|mut e| {
                let current = std::mem::take(&mut e.completed);
                e.completed = reports.clone();
                e.completed.extend(current);
                e
            })?;
            self.complete(&report).map_err(|mut error| {
                let current = std::mem::take(&mut error.completed);
                error.completed = reports.clone();
                error.completed.extend(current);
                error
            })?;
            outputs.push(bytes);
            reports.push(report);
        }
        let report = self.report(&plan, replay, reports)?;
        Ok((outputs, report))
    }
    pub async fn write_to<W: AsyncWrite + Unpin>(
        self,
        writer: &mut W,
    ) -> TimelineResult<TimelineSessionReport> {
        self.single()?;
        let plan = self.plan(true).await?;
        let mut replay = Replay::new(self.inputs.len());
        let part = plan
            .parts
            .first()
            .ok_or_else(|| fail(TimelineErrorKind::EmptyRange))?;
        let report = self.fragmented(&plan, part, 0, &mut replay, writer).await?;
        self.flush(writer).await?;
        self.complete(&report)?;
        self.report(&plan, replay, vec![report])
    }
    pub async fn write_to_outputs<P: TimelineWriterProvider>(
        self,
        provider: &mut P,
    ) -> TimelineResult<TimelineSessionReport> {
        let plan = self.plan(true).await?;
        let mut replay = Replay::new(self.inputs.len());
        let mut reports = Vec::new();
        let mut writer = self
            .acquire_writer(provider, request(0, &plan.parts[0])?)
            .await?;
        for (index, part) in plan.parts.iter().enumerate() {
            let result = async {
                let report = self
                    .fragmented(&plan, part, index, &mut replay, &mut writer)
                    .await?;
                // Acquire the next lease before completing the old output.
                let next = if let Some(next) = plan.parts.get(index + 1) {
                    Some(
                        self.acquire_writer(provider, request(index + 1, next)?)
                            .await?,
                    )
                } else {
                    None
                };
                self.flush(&mut writer).await?;
                self.complete(&report)?;
                Ok((report, next))
            }
            .await;
            let (report, next) = result.map_err(|mut e: TimelineSessionError| {
                let current = std::mem::take(&mut e.completed);
                e.completed = reports.clone();
                e.completed.extend(current);
                e
            })?;
            reports.push(report);
            if let Some(next) = next {
                writer = next;
            }
        }
        self.report(&plan, replay, reports)
    }
    fn single(&self) -> TimelineResult<()> {
        if self.options.changes == TimelineChangePolicy::Split {
            Err(fail(TimelineErrorKind::SplitRequiresProvider))
        } else {
            Ok(())
        }
    }
    async fn acquire_writer<P: TimelineWriterProvider>(
        &self,
        provider: &mut P,
        request: TimelineOutputRequest,
    ) -> TimelineResult<P::Writer> {
        self.options.check()?;
        let future = provider.acquire(request);
        if let Some(cancel) = &self.options.cancel {
            tokio::select! { biased; _=cancel.cancelled()=>Err(fail(TimelineErrorKind::Cancelled)), result=future=>result }
        } else {
            future.await
        }
    }
    async fn flush<W: AsyncWrite + Unpin>(&self, writer: &mut W) -> TimelineResult<()> {
        crate::cancel::wait(self.options.cancel.as_ref(), async {
            writer.flush().await.map_err(Error::from)
        })
        .await
        .map_err(output_error)
    }
    async fn write<W: AsyncWrite + Unpin>(
        &self,
        writer: &mut W,
        bytes: &[u8],
    ) -> TimelineResult<()> {
        crate::cancel::wait(self.options.cancel.as_ref(), async {
            writer.write_all(bytes).await?;
            writer.flush().await?;
            Ok(())
        })
        .await
        .map_err(output_error)
    }
    async fn sample(
        &self,
        plan: &Plan,
        part: &Part,
        s: &SampleRecord,
        replay: &mut Replay,
    ) -> TimelineResult<(usize, Mp4Sample)> {
        let resource = &plan.resources[s.resource];
        let track = part
            .track_keys
            .iter()
            .position(|key| *key == (resource.input, matches!(s.kind, StreamKind::Aac)))
            .unwrap();
        let scale = part.tracks[track].timescale;
        let mut packet = replay.packet(s)?;
        // Preserve exact rational origins until the final conversion to track ticks.
        let dts = rescale(
            sub(
                sample_time(s, s.public_dts, &plan.gaps, self.options.gaps)?,
                part.origin,
            )?,
            scale,
        )?;
        let pts = rescale(
            sub(
                output_time(s.public_pts, &plan.gaps, self.options.gaps)?,
                part.origin,
            )?,
            scale,
        )?;
        let duration = rescale(s.duration, scale)?;
        packet.timing = Some(PacketTiming {
            edit_offset: 0,
            timescale: scale,
            dts,
            pts,
            duration: u32::try_from(duration).map_err(|_| fail(TimelineErrorKind::TimeOverflow))?,
        });
        packet.is_key = s.rap;
        let sample = packet_sample(packet, scale, 0, Some((0, scale))).map_err(media_error)?;
        if sample.duration == 0 {
            return Err(fail(TimelineErrorKind::MissingTailDuration));
        }
        Ok((track, sample))
    }
    fn output_report(
        &self,
        _plan: &Plan,
        part: &Part,
        index: usize,
        tracks: Vec<crate::TrackInfo>,
        bytes: u64,
    ) -> TimelineResult<TimelineOutputReport> {
        let range = request(index, part)?.range;
        let mappings = part.mappings.clone();
        let duration = tracks
            .iter()
            .map(|t| u128::from(t.duration) * 1000 / u128::from(t.timescale))
            .max()
            .unwrap_or(0);
        Ok(TimelineOutputReport {
            index,
            range,
            reason: part.reason,
            mappings,
            media: TransmuxReport {
                segment_count: part.resource_ids.len(),
                tracks,
                duration: u64::try_from(duration)
                    .map_err(|_| fail(TimelineErrorKind::TimeOverflow))?,
                duration_timescale: 1000,
                bytes_written: bytes,
            },
        })
    }
    async fn classic(
        &self,
        plan: &Plan,
        part: &Part,
        index: usize,
        replay: &mut Replay,
    ) -> TimelineResult<(Vec<u8>, TimelineOutputReport)> {
        let mut tracks = vec![Vec::new(); part.tracks.len()];
        for _ in 0..part.count {
            let source = replay
                .next(self, &plan.resources, plan.selected)
                .await?
                .ok_or_else(|| fail(TimelineErrorKind::ResourceChanged))?;
            let (track, sample) = self.sample(plan, part, &source, replay).await?;
            tracks[track].push(sample);
        }
        let tracks = part
            .tracks
            .iter()
            .cloned()
            .zip(tracks)
            .map(|(t, s)| t.into_classic(s))
            .collect();
        let (bytes, tracks) = Mp4Muxer::new(tracks)
            .write_checked(&|| check_cancel(self.options.cancel.as_ref()))
            .map_err(output_error)?;
        let report = self.output_report(plan, part, index, tracks, bytes.len() as u64)?;
        Ok((bytes, report))
    }
    async fn fragmented<W: AsyncWrite + Unpin>(
        &self,
        plan: &Plan,
        part: &Part,
        index: usize,
        replay: &mut Replay,
        writer: &mut W,
    ) -> TimelineResult<TimelineOutputReport> {
        let mut offsets = vec![None; part.tracks.len()];
        for s in &part.samples {
            let input = plan.resources[s.resource].input;
            let i = part
                .track_keys
                .iter()
                .position(|k| *k == (input, matches!(s.kind, StreamKind::Aac)))
                .unwrap();
            if offsets[i].is_none() {
                offsets[i] = Some(
                    u64::try_from(rescale(
                        sub(
                            sample_time(s, s.public_dts, &plan.gaps, self.options.gaps)?,
                            part.origin,
                        )?,
                        part.tracks[i].timescale,
                    )?)
                    .map_err(|_| fail(TimelineErrorKind::TimeOverflow))?,
                );
            }
        }
        let offsets: Vec<_> = offsets.into_iter().map(|v| v.unwrap_or(0)).collect();
        let mut mux = FragmentedMp4Muxer::new(part.tracks.clone());
        let header = mux
            .write_header_with_offsets(&offsets)
            .map_err(output_error)?;
        self.write(writer, &header).await?;
        let mut bytes = header.len() as u64;
        let mut tracks: Vec<_> = part.tracks.iter().map(|t| track_report(t, 0, 0)).collect();
        let mut grouped: Vec<Vec<Mp4Sample>> = vec![Vec::new(); part.tracks.len()];
        let mut group_start = None;
        let mut group_bytes = 0u64;
        let mut decode_ends = vec![None; part.tracks.len()];
        for _ in 0..part.count {
            let source = replay
                .next(self, &plan.resources, plan.selected)
                .await?
                .ok_or_else(|| fail(TimelineErrorKind::ResourceChanged))?;
            if group_start.is_some_and(|start| {
                rescale(sub(source.public_dts, start).unwrap_or(zero()), 1000).unwrap_or(i128::MAX)
                    >= 1000
            }) {
                bytes += self.flush_fragment(writer, &mut mux, &mut grouped).await?;
                group_start = None;
                group_bytes = 0;
            }
            let (track, mut sample) = self.sample(plan, part, &source, replay).await?;
            if let Some(end) = decode_ends[track] {
                if sample.dts.abs_diff(end) <= 1 {
                    sample.pts += i128::from(end) - i128::from(sample.dts);
                    sample.dts = end;
                } else if grouped.iter().any(|s| !s.is_empty()) {
                    bytes += self.flush_fragment(writer, &mut mux, &mut grouped).await?;
                    group_start = None;
                    group_bytes = 0;
                }
            }
            if group_bytes + sample.data.len() as u64 > self.options.resources.max_resource_bytes()
                && group_bytes > 0
            {
                bytes += self.flush_fragment(writer, &mut mux, &mut grouped).await?;
                group_start = None;
                group_bytes = 0;
            }
            group_start.get_or_insert(source.public_dts);
            group_bytes += sample.data.len() as u64;
            let end = sample
                .dts
                .checked_add(u64::from(sample.duration))
                .ok_or_else(|| fail(TimelineErrorKind::TimeOverflow))?;
            decode_ends[track] = Some(end);
            tracks[track].sample_count += 1;
            tracks[track].duration = tracks[track]
                .duration
                .max(end)
                .max(u64::try_from(sample.pts + i128::from(sample.duration)).unwrap_or(0));
            sample.dts = sample
                .dts
                .checked_sub(offsets[track])
                .ok_or_else(|| fail(TimelineErrorKind::TimelineAmbiguous))?;
            sample.pts -= i128::from(offsets[track]);
            grouped[track].push(sample);
        }
        bytes = bytes
            .checked_add(self.flush_fragment(writer, &mut mux, &mut grouped).await?)
            .ok_or_else(|| fail(TimelineErrorKind::TimeOverflow))?;
        self.output_report(plan, part, index, tracks, bytes)
    }
    async fn flush_fragment<W: AsyncWrite + Unpin>(
        &self,
        writer: &mut W,
        mux: &mut FragmentedMp4Muxer,
        grouped: &mut [Vec<Mp4Sample>],
    ) -> TimelineResult<u64> {
        if grouped.iter().all(Vec::is_empty) {
            return Ok(0);
        }
        let bytes = mux.write_fragment(grouped).map_err(output_error)?;
        self.write(writer, &bytes).await?;
        for track in grouped {
            track.clear();
        }
        Ok(bytes.len() as u64)
    }
    fn complete(&self, report: &TimelineOutputReport) -> TimelineResult<()> {
        (|| {
            self.options.emit(TimelineSessionEvent {
                kind: TimelineEventKind::MappingCommitted,
                output: report.index,
                mappings: report.mappings.clone(),
            })?;
            self.options.emit(TimelineSessionEvent {
                kind: TimelineEventKind::OutputCompleted,
                output: report.index,
                mappings: Vec::new(),
            })
        })()
        .map_err(|mut error: TimelineSessionError| {
            error.completed.push(report.clone());
            error
        })
    }
    fn report(
        &self,
        plan: &Plan,
        replay: Replay,
        outputs: Vec<TimelineOutputReport>,
    ) -> TimelineResult<TimelineSessionReport> {
        let check = || {
            self.options.check().map_err(|mut error| {
                error.completed = outputs.clone();
                error
            })
        };
        check()?;
        let mut dependencies = Vec::new();
        for id in plan.parts.iter().flat_map(|p| &p.resource_ids) {
            let resource = &plan.resources[*id];
            let descriptor = &self.inputs[resource.input].snapshot.segments()[resource.segment];
            if !dependencies
                .iter()
                .any(|d: &TimelineDependency| d.slot == *descriptor.slot())
            {
                dependencies.push(TimelineDependency {
                    slot: descriptor.slot().clone(),
                    map: descriptor.map().map(|m| m.declaration()),
                    keys: descriptor
                        .keys()
                        .candidates()
                        .iter()
                        .map(|k| k.declaration())
                        .collect(),
                    map_keys: descriptor
                        .map()
                        .map(|m| {
                            m.keys()
                                .candidates()
                                .iter()
                                .map(|k| k.declaration())
                                .collect()
                        })
                        .unwrap_or_default(),
                });
            }
        }
        let mut access_points = Vec::new();
        for (output, part) in plan.parts.iter().enumerate() {
            if let Some(sample) = part
                .samples
                .iter()
                .find(|s| s.rap && !matches!(s.kind, StreamKind::Aac))
                .or_else(|| part.samples.first())
            {
                let resource = &plan.resources[sample.resource];
                access_points.push(TimelineAccessPoint {
                    slot: self.inputs[resource.input].snapshot.segments()[resource.segment]
                        .slot()
                        .clone(),
                    sample: sample.sample,
                    source: MediaTime {
                        ticks: sample.timing.pts,
                        timescale: sample.timing.timescale,
                    },
                    presentation: sample.public_pts,
                    output,
                });
            }
        }
        // Completion linearizes here. Cancellation requested by the terminal
        // callback cannot turn an already completed operation into Cancelled.
        check()?;
        if let Some(callback) = &self.options.on_event {
            callback(TimelineSessionEvent {
                kind: TimelineEventKind::Completed,
                output: outputs.len().saturating_sub(1),
                mappings: Vec::new(),
            });
        }
        Ok(TimelineSessionReport {
            requested: self.options.range,
            actual: plan.actual,
            outputs,
            gaps: plan.gaps.clone(),
            dependencies,
            access_points,
            resource_reads: plan.reads + replay.reads,
            source_bytes: plan.bytes + replay.bytes,
            peak_planned_samples: plan.peak_samples.max(replay.peak_samples),
            peak_planned_resources: plan.peak_resources.max(replay.peak_resources),
            indexed_resources: plan.resources.len(),
            sample_buffers: self.resources.stats(),
        })
    }
}
fn output_error(error: Error) -> TimelineSessionError {
    let mut result = media_error(error);
    if result.kind != TimelineErrorKind::Cancelled {
        result.kind = TimelineErrorKind::Output;
    }
    result
}

#[cfg(not(target_arch = "wasm32"))]
impl TimelinePreparedTransmux {
    pub async fn write_to_file(
        self,
        path: impl AsRef<Path>,
        options: FileOutputOptions,
    ) -> TimelineResult<TimelineSessionReport> {
        self.single()?;
        struct One(std::path::PathBuf);
        impl TimelineFileProvider for One {
            fn acquire<'a>(
                &'a mut self,
                _: TimelineOutputRequest,
            ) -> Pin<Box<dyn Future<Output = TimelineResult<std::path::PathBuf>> + 'a>>
            {
                Box::pin(async { Ok(self.0.clone()) })
            }
        }
        self.write_to_files(&mut One(path.as_ref().to_path_buf()), options)
            .await
    }
    pub async fn write_to_files<P: TimelineFileProvider>(
        self,
        provider: &mut P,
        options: FileOutputOptions,
    ) -> TimelineResult<TimelineSessionReport> {
        let plan = self
            .plan(options.format == OutputFormat::FragmentedMp4)
            .await?;
        let mut replay = Replay::new(self.inputs.len());
        let mut reports = Vec::new();
        let mut paths = std::collections::HashSet::new();
        let mut target = self
            .acquire_file(provider, request(0, &plan.parts[0])?)
            .await?;
        for (index, part) in plan.parts.iter().enumerate() {
            let result = async {
                let parent = target
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .unwrap_or_else(|| Path::new("."));
                let identity = tokio::fs::canonicalize(parent)
                    .await
                    .map_err(|error| output_error(error.into()))?
                    .join(
                        target
                            .file_name()
                            .ok_or_else(|| fail(TimelineErrorKind::InvalidOptions))?,
                    );
                if !paths.insert(identity) {
                    return Err(fail(TimelineErrorKind::InvalidOptions));
                }
                let (temp, file) = temporary_file(&target).map_err(output_error)?;
                let mut writer = tokio::fs::File::from_std(file);
                let mut report = if options.format == OutputFormat::Mp4 {
                    let (bytes, report) = self.classic(&plan, part, index, &mut replay).await?;
                    self.write(&mut writer, &bytes).await?;
                    report
                } else {
                    self.fragmented(&plan, part, index, &mut replay, &mut writer)
                        .await?
                };
                let next = if let Some(part) = plan.parts.get(index + 1) {
                    Some(
                        self.acquire_file(provider, request(index + 1, part)?)
                            .await?,
                    )
                } else {
                    None
                };
                self.flush(&mut writer).await?;
                drop(writer);
                let mut temp = temp;
                if options.format == OutputFormat::StreamingMp4 {
                    let source = temp.0.clone();
                    let destination = target.clone();
                    let tracks = part.tracks.clone();
                    let previous_reports = report.media.tracks.clone();
                    let cancel = self.options.cancel.clone();
                    let backend = options.backend;
                    let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
                    let guard = BlockingStopGuard(stopped.clone());
                    let (next, bytes, tracks) =
                        tokio::task::spawn_blocking(move || -> TimelineResult<_> {
                            let check = || {
                                if stopped.load(std::sync::atomic::Ordering::Acquire) {
                                    return Err(Error::Cancelled);
                                }
                                check_cancel(cancel.as_ref())
                            };
                            check().map_err(output_error)?;
                            let (final_temp, mut file) =
                                temporary_file(&destination).map_err(output_error)?;
                            let mut input =
                                std::fs::File::open(&source).map_err(|e| output_error(e.into()))?;
                            let size = input.metadata().map_err(|e| output_error(e.into()))?.len();
                            let scan =
                                crate::isobmff::scan_file(&mut input, size, true, false, &check)
                                    .map_err(output_error)?;
                            let (bytes, reports) = match backend {
                                FinalizeBackend::Native => Mp4Muxer::new(
                                    tracks
                                        .into_iter()
                                        .zip(scan.samples)
                                        .map(|(t, s)| t.into_classic(s))
                                        .collect(),
                                )
                                .write_file(&mut input, &mut file, &check)
                                .map_err(output_error)?,
                                #[cfg(feature = "ffmpeg-finalize")]
                                FinalizeBackend::Ffmpeg => {
                                    crate::ffmpeg_finalize::remux_blocking(
                                        &source,
                                        &final_temp.0,
                                        &check,
                                    )
                                    .map_err(output_error)?;
                                    (
                                        file.metadata().map_err(|e| output_error(e.into()))?.len(),
                                        previous_reports,
                                    )
                                }
                            };
                            #[cfg(not(feature = "ffmpeg-finalize"))]
                            let _ = previous_reports;
                            std::io::Write::flush(&mut file).map_err(|e| output_error(e.into()))?;
                            check().map_err(output_error)?;
                            Ok((final_temp, bytes, reports))
                        })
                        .await
                        .map_err(|_| fail(TimelineErrorKind::Output))??;
                    drop(guard);
                    report.media.bytes_written = bytes;
                    report.media.tracks = tracks;
                    temp = next;
                }
                self.options.check()?;
                tokio::fs::rename(&temp.0, &target)
                    .await
                    .map_err(|e| output_error(e.into()))?;
                self.complete(&report)?;
                Ok((report, next))
            }
            .await;
            let (report, next) = result.map_err(|mut e: TimelineSessionError| {
                let current = std::mem::take(&mut e.completed);
                e.completed = reports.clone();
                e.completed.extend(current);
                e
            })?;
            reports.push(report);
            if let Some(next) = next {
                target = next;
            }
        }
        self.report(&plan, replay, reports)
    }
    async fn acquire_file<P: TimelineFileProvider>(
        &self,
        provider: &mut P,
        request: TimelineOutputRequest,
    ) -> TimelineResult<std::path::PathBuf> {
        self.options.check()?;
        let future = provider.acquire(request);
        if let Some(cancel) = &self.options.cancel {
            tokio::select! {biased;_=cancel.cancelled()=>Err(fail(TimelineErrorKind::Cancelled)),result=future=>result}
        } else {
            future.await
        }
    }
}
