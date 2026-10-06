use super::*;
use std::task::{Context, Poll};

#[derive(Debug, Clone)]
pub struct ContinuousOutputRequest {
    index: u64,
    tracks: Vec<crate::TrackInfo>,
}
impl ContinuousOutputRequest {
    pub fn index(&self) -> u64 {
        self.index
    }
    pub fn tracks(&self) -> &[crate::TrackInfo] {
        &self.tracks
    }
}
/// Acquire a lease. The core flushes but never closes or aborts a borrowed sink.
pub trait ContinuousWriterProvider {
    type Writer: AsyncWrite + Unpin;
    fn acquire<'a>(
        &'a mut self,
        request: ContinuousOutputRequest,
    ) -> Pin<Box<dyn Future<Output = ContinuousResult<Self::Writer>> + 'a>>;
}
#[cfg(not(target_arch = "wasm32"))]
pub trait ContinuousFileProvider {
    fn acquire<'a>(
        &'a mut self,
        request: ContinuousOutputRequest,
    ) -> Pin<Box<dyn Future<Output = ContinuousResult<std::path::PathBuf>> + 'a>>;
}
trait Target {
    type Writer: AsyncWrite + Unpin;
    fn acquire<'a>(
        &'a mut self,
        request: ContinuousOutputRequest,
    ) -> Pin<Box<dyn Future<Output = ContinuousResult<Self::Writer>> + 'a>>;
    fn finish<'a>(
        &'a mut self,
        writer: &'a mut Self::Writer,
        report: &'a mut ContinuousOutputReport,
        _final_output: bool,
    ) -> Pin<Box<dyn Future<Output = ContinuousResult<()>> + 'a>>;
}
struct Borrowed<'a, P>(&'a mut P);
impl<P: ContinuousWriterProvider> Target for Borrowed<'_, P> {
    type Writer = P::Writer;
    fn acquire<'a>(
        &'a mut self,
        request: ContinuousOutputRequest,
    ) -> Pin<Box<dyn Future<Output = ContinuousResult<Self::Writer>> + 'a>> {
        self.0.acquire(request)
    }
    fn finish<'a>(
        &'a mut self,
        _: &'a mut Self::Writer,
        _: &'a mut ContinuousOutputReport,
        _final_output: bool,
    ) -> Pin<Box<dyn Future<Output = ContinuousResult<()>> + 'a>> {
        Box::pin(async { Ok(()) })
    }
}
struct One<'a, W>(Option<&'a mut W>);
impl<'w, W: AsyncWrite + Unpin> ContinuousWriterProvider for One<'w, W> {
    type Writer = &'w mut W;
    fn acquire<'a>(
        &'a mut self,
        _: ContinuousOutputRequest,
    ) -> Pin<Box<dyn Future<Output = ContinuousResult<Self::Writer>> + 'a>> {
        Box::pin(async {
            self.0
                .take()
                .ok_or_else(|| fail(ContinuousErrorKind::ConfigurationChanged))
        })
    }
}
impl ContinuousSession {
    pub async fn write_to<W: AsyncWrite + Unpin>(
        self,
        writer: &mut W,
    ) -> ContinuousResult<ContinuousReport> {
        if self.options.changes == TimelineChangePolicy::Split
            || self.options.missing == MissingSegmentPolicy::Split
        {
            return self.finish(Err(fail(ContinuousErrorKind::InvalidOptions)));
        }
        self.write_to_outputs(&mut One(Some(writer))).await
    }
    pub async fn write_to_outputs<P: ContinuousWriterProvider>(
        self,
        provider: &mut P,
    ) -> ContinuousResult<ContinuousReport> {
        let result = self.run(&mut Borrowed(provider)).await;
        self.finish(result)
    }
    /// Capacity bounds the collected output bytes. Demux/mux copies and classic
    /// sample metadata have additional, separately documented costs.
    pub async fn into_bytes(
        self,
        capacity: usize,
        format: OutputFormat,
    ) -> ContinuousResult<(Vec<u8>, ContinuousReport)> {
        if capacity == 0
            || format == OutputFormat::StreamingMp4
            || self.options.changes == TimelineChangePolicy::Split
            || self.options.missing == MissingSegmentPolicy::Split
        {
            return self.finish(Err(fail(ContinuousErrorKind::InvalidOptions)));
        }
        let mut target = MemoryTarget {
            writer: Some(MemoryWriter {
                bytes: Vec::new(),
                capacity,
            }),
            format,
            result: None,
            signal: self.shared.signal.clone(),
        };
        let result = self
            .run(&mut target)
            .await
            .map(|report| (target.result.take().unwrap_or_default(), report));
        self.finish(result)
    }
    async fn write<W: AsyncWrite + Unpin>(
        &self,
        writer: &mut W,
        bytes: &[u8],
    ) -> ContinuousResult<()> {
        self.blocked(true);
        let result = self
            .wait(async {
                writer
                    .write_all(bytes)
                    .await
                    .map_err(|e| output_error(e.into()))?;
                writer.flush().await.map_err(|e| output_error(e.into()))
            })
            .await;
        self.blocked(false);
        result
    }
    async fn complete_output<T: Target>(
        &self,
        target: &mut T,
        writer: &mut T::Writer,
        engine: &mut Engine,
    ) -> ContinuousResult<()> {
        self.wait(async { writer.flush().await.map_err(|e| output_error(e.into())) })
            .await?;
        let mut report = engine.output_report();
        self.wait(target.finish(writer, &mut report, true)).await?;
        engine.bytes = engine
            .bytes
            .checked_sub(engine.part_bytes)
            .and_then(|b| b.checked_add(report.media.bytes_written))
            .ok_or_else(|| fail(ContinuousErrorKind::TimeOverflow))?;
        self.record_output(&report);
        engine.outputs.push_back(report.clone());
        engine.retain_history(self);
        self.emit(ContinuousEvent::Output(report))
    }
    fn record_output(&self, report: &ContinuousOutputReport) {
        let mut state = self.shared.inner.lock().unwrap();
        state.completed.push_back(report.clone());
        while state.completed.len() > self.options.limits.history {
            state.completed.pop_front();
        }
    }
    async fn run<T: Target>(&self, target: &mut T) -> ContinuousResult<ContinuousReport> {
        self.emit(ContinuousEvent::State(ContinuousState::Preparing))?;
        self.pause_boundary().await?;
        let mut engine = match Engine::prepare(self).await {
            Ok(engine) => engine,
            Err(error) if error.kind() == ContinuousErrorKind::EmptyInput => {
                let state = self.shared.inner.lock().unwrap();
                if state.lanes.iter().any(|l| l.progress.accepted > 0) {
                    return Err(error);
                }
                return Ok(ContinuousReport {
                    reason: state.reason.unwrap_or(ContinuousEndReason::Eof),
                    inputs: state.lanes.iter().map(|l| l.progress.clone()).collect(),
                    bytes: 0,
                    duration: zero(),
                    requested: self.options.range,
                    actual: None,
                    gaps: 0,
                    outputs: vec![],
                    mappings: vec![],
                    truncated: false,
                    peaks: state.peaks.clone(),
                });
            }
            Err(error) => return Err(error),
        };
        if self.options.range.is_some() {
            engine.seek(self).await?;
        }
        let request = |engine: &Engine| ContinuousOutputRequest {
            index: engine.output,
            tracks: engine
                .tracks
                .iter()
                .map(|t| track_report(t, 0, 0))
                .collect(),
        };
        let mut writer = self.wait(target.acquire(request(&engine))).await?;
        let mut mux = FragmentedMp4Muxer::new(engine.tracks.clone());
        let header = mux
            .write_header_with_offsets(&engine.offsets)
            .map_err(output_error)?;
        self.write(&mut writer, &header).await?;
        engine.bytes += header.len() as u64;
        engine.part_bytes += header.len() as u64;
        while let Some((input, mut batch)) = engine.next(self).await? {
            if batch.descriptor.gap() {
                self.write(&mut writer, &[]).await?;
                engine.gaps += 1;
                self.emit(ContinuousEvent::Gap {
                    slot: batch.descriptor.slot().clone(),
                    duration: batch
                        .descriptor
                        .duration()
                        .media_time()
                        .map_err(media_error)?,
                    presentation_start: batch.shift,
                })?;
                self.commit(input, &batch.descriptor, engine.bytes)?;
                continue;
            }
            if batch.changed {
                let previous = engine.output_report();
                engine.split(self, input, batch)?;
                let next = self.wait(target.acquire(request(&engine))).await?;
                // Finish the old output only after acquisition of the new lease.
                let previous_bytes = previous.media.bytes_written;
                let mut old = previous;
                self.wait(async { writer.flush().await.map_err(|e| output_error(e.into())) })
                    .await?;
                self.wait(target.finish(&mut writer, &mut old, false))
                    .await?;
                engine.bytes = engine
                    .bytes
                    .checked_sub(previous_bytes)
                    .and_then(|b| b.checked_add(old.media.bytes_written))
                    .ok_or_else(|| fail(ContinuousErrorKind::TimeOverflow))?;
                self.record_output(&old);
                engine.outputs.push_back(old.clone());
                engine.retain_history(self);
                self.emit(ContinuousEvent::Output(old))?;
                writer = next;
                mux = FragmentedMp4Muxer::new(engine.tracks.clone());
                let header = mux
                    .write_header_with_offsets(&engine.offsets)
                    .map_err(output_error)?;
                self.write(&mut writer, &header).await?;
                engine.bytes += header.len() as u64;
                engine.part_bytes += header.len() as u64;
                continue;
            }
            let mapping_count = engine.mappings.len();
            let grouped = engine.samples(self, input, &mut batch)?;
            let fragment = if grouped.iter().any(|s| !s.is_empty()) {
                mux.write_fragment(&grouped).map_err(output_error)?
            } else {
                Vec::new()
            };
            self.write(&mut writer, &fragment).await?;
            engine.bytes = engine
                .bytes
                .checked_add(fragment.len() as u64)
                .ok_or_else(|| fail(ContinuousErrorKind::TimeOverflow))?;
            engine.part_bytes += fragment.len() as u64;
            engine.segments += 1;
            for mapping in engine.mappings.iter().skip(mapping_count) {
                self.emit(ContinuousEvent::Mapping(mapping.clone()))?;
            }
            engine.retain_history(self);
            self.commit(input, &batch.descriptor, engine.bytes)?;
            if self
                .options
                .duration
                .is_some_and(|d| cmp(engine.duration, d).is_ok_and(|v| !v.is_lt()))
            {
                self.handle().drain(ContinuousEndReason::DurationLimit);
            }
        }
        self.handle().drain(ContinuousEndReason::Eof);
        self.state(ContinuousState::Finalizing)?;
        // Validate fallible timeline arithmetic before a native publication boundary.
        let _ = engine.report(self)?;
        self.complete_output(target, &mut writer, &mut engine)
            .await?;
        engine.report(self)
    }
}

struct MemoryWriter {
    bytes: Vec<u8>,
    capacity: usize,
}
impl AsyncWrite for MemoryWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if bytes.len() > self.capacity - self.bytes.len() {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::OutOfMemory,
                "continuous output capacity exceeded",
            )));
        }
        self.bytes.extend_from_slice(bytes);
        Poll::Ready(Ok(bytes.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Err(std::io::Error::other("caller owns shutdown")))
    }
}
struct MemoryTarget {
    writer: Option<MemoryWriter>,
    format: OutputFormat,
    result: Option<Vec<u8>>,
    signal: Arc<Signal>,
}
impl Target for MemoryTarget {
    type Writer = MemoryWriter;
    fn acquire<'a>(
        &'a mut self,
        _: ContinuousOutputRequest,
    ) -> Pin<Box<dyn Future<Output = ContinuousResult<Self::Writer>> + 'a>> {
        Box::pin(async {
            self.writer
                .take()
                .ok_or_else(|| fail(ContinuousErrorKind::InvalidOptions))
        })
    }
    fn finish<'a>(
        &'a mut self,
        writer: &'a mut Self::Writer,
        report: &'a mut ContinuousOutputReport,
        _final_output: bool,
    ) -> Pin<Box<dyn Future<Output = ContinuousResult<()>> + 'a>> {
        Box::pin(async move {
            report.collected_bytes = writer.bytes.len() as u64;
            if self.format == OutputFormat::Mp4 {
                report.classic_index_samples = report
                    .media
                    .tracks
                    .iter()
                    .map(|t| t.sample_count as u64)
                    .sum();
                let check = || {
                    if self.signal.is_cancelled() {
                        Err(Error::Cancelled)
                    } else {
                        Ok(())
                    }
                };
                let data =
                    crate::isobmff::demux_isobmff_checked(&writer.bytes, &writer.bytes, &check)
                        .map_err(media_error)?;
                let tracks = build_fragmented_tracks(&data).map_err(media_error)?;
                let mut samples = vec![Vec::new(); tracks.len()];
                for packet in data.packets {
                    check().map_err(media_error)?;
                    let audio = matches!(packet.kind, StreamKind::Aac);
                    let index = tracks
                        .iter()
                        .position(|t| {
                            matches!(t.kind, crate::mp4::FragmentedTrackKind::Audio { .. }) == audio
                        })
                        .ok_or_else(|| fail(ContinuousErrorKind::Media))?;
                    samples[index].push(
                        packet_sample(packet, tracks[index].timescale, 0, Some((0, 1)))
                            .map_err(media_error)?,
                    );
                }
                let (bytes, tracks) = Mp4Muxer::new(
                    tracks
                        .into_iter()
                        .zip(samples)
                        .map(|(t, s)| t.into_classic(s))
                        .collect(),
                )
                .write_checked(&check)
                .map_err(output_error)?;
                if bytes.len() > writer.capacity {
                    return Err(fail(ContinuousErrorKind::BudgetExceeded));
                }
                writer.bytes = bytes;
                report.media.tracks = tracks;
                report.media.bytes_written = writer.bytes.len() as u64;
            }
            self.result = Some(std::mem::take(&mut writer.bytes));
            Ok(())
        })
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;
    struct Lease {
        file: Option<tokio::fs::File>,
        temp: TemporaryFile,
        target: std::path::PathBuf,
    }
    impl AsyncWrite for Lease {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Pin::new(self.file.as_mut().expect("active output file")).poll_write(cx, bytes)
        }
        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Pin::new(self.file.as_mut().expect("active output file")).poll_flush(cx)
        }
        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(self.file.as_mut().expect("active output file")).poll_shutdown(cx)
        }
    }
    struct Files<'a, P> {
        provider: &'a mut P,
        options: FileOutputOptions,
        shared: Arc<Shared>,
    }
    impl<P: ContinuousFileProvider> Target for Files<'_, P> {
        type Writer = Lease;
        fn acquire<'a>(
            &'a mut self,
            request: ContinuousOutputRequest,
        ) -> Pin<Box<dyn Future<Output = ContinuousResult<Self::Writer>> + 'a>> {
            Box::pin(async move {
                let target = self.provider.acquire(request).await?;
                // Do not overwrite an earlier split or an existing consumer file.
                if tokio::fs::try_exists(&target)
                    .await
                    .map_err(|e| output_error(e.into()))?
                {
                    return Err(fail(ContinuousErrorKind::Output));
                }
                let (temp, file) = temporary_file(&target).map_err(output_error)?;
                Ok(Lease {
                    file: Some(tokio::fs::File::from_std(file)),
                    temp,
                    target,
                })
            })
        }
        fn finish<'a>(
            &'a mut self,
            writer: &'a mut Self::Writer,
            report: &'a mut ContinuousOutputReport,
            _final_output: bool,
        ) -> Pin<Box<dyn Future<Output = ContinuousResult<()>> + 'a>> {
            Box::pin(async move {
                writer
                    .file
                    .as_mut()
                    .expect("active output file")
                    .flush()
                    .await
                    .map_err(|e| output_error(e.into()))?;
                // Release the original file before removing/replacing its temporary
                // pathname (required for cleanup on Windows).
                drop(writer.file.take());
                if self.options.format != OutputFormat::FragmentedMp4 {
                    report.classic_index_samples = report
                        .media
                        .tracks
                        .iter()
                        .map(|t| t.sample_count as u64)
                        .sum();
                    let source = writer.temp.0.clone();
                    let destination = writer.target.clone();
                    let signal = self.shared.signal.clone();
                    let backend = self.options.backend;
                    let previous = report.media.tracks.clone();
                    let (temp, bytes, tracks) =
                        tokio::task::spawn_blocking(move || -> ContinuousResult<_> {
                            let check = || {
                                if signal.is_cancelled() {
                                    Err(Error::Cancelled)
                                } else {
                                    Ok(())
                                }
                            };
                            check().map_err(media_error)?;
                            let (temp, mut output) =
                                temporary_file(&destination).map_err(output_error)?;
                            let mut input =
                                std::fs::File::open(&source).map_err(|e| output_error(e.into()))?;
                            let size = input.metadata().map_err(|e| output_error(e.into()))?.len();
                            let scan =
                                crate::isobmff::scan_file(&mut input, size, true, false, &check)
                                    .map_err(media_error)?;
                            let tracks =
                                crate::isobmff::file_tracks(&scan.init).map_err(media_error)?;
                            let (bytes, reports) = match backend {
                                FinalizeBackend::Native => Mp4Muxer::new(
                                    tracks
                                        .into_iter()
                                        .zip(scan.samples)
                                        .map(|(t, s)| t.into_classic(s))
                                        .collect(),
                                )
                                .write_file(&mut input, &mut output, &check)
                                .map_err(output_error)?,
                                #[cfg(feature = "ffmpeg-finalize")]
                                FinalizeBackend::Ffmpeg => {
                                    crate::ffmpeg_finalize::remux_blocking(
                                        &source, &temp.0, &check,
                                    )
                                    .map_err(output_error)?;
                                    (
                                        output
                                            .metadata()
                                            .map_err(|e| output_error(e.into()))?
                                            .len(),
                                        previous,
                                    )
                                }
                            };
                            #[cfg(not(feature = "ffmpeg-finalize"))]
                            let _ = previous;
                            std::io::Write::flush(&mut output)
                                .map_err(|e| output_error(e.into()))?;
                            check().map_err(media_error)?;
                            Ok((temp, bytes, reports))
                        })
                        .await
                        .map_err(|_| fail(ContinuousErrorKind::Output))??;
                    writer.temp = temp;
                    report.media.bytes_written = bytes;
                    report.media.tracks = tracks;
                }
                // Cancellation and final publication share one linearization lock.
                let mut state = self.shared.inner.lock().unwrap();
                if self.shared.signal.is_cancelled() {
                    return Err(fail(ContinuousErrorKind::Cancelled));
                }
                // A no-clobber publication boundary also prevents repeated split paths.
                std::fs::hard_link(&writer.temp.0, &writer.target)
                    .map_err(|e| output_error(e.into()))?;
                if _final_output {
                    state.state = ContinuousState::Completed;
                }
                Ok(())
            })
        }
    }
    impl ContinuousSession {
        pub async fn write_to_file(
            self,
            path: impl AsRef<Path>,
            options: FileOutputOptions,
        ) -> ContinuousResult<ContinuousReport> {
            struct OnePath(Option<std::path::PathBuf>);
            impl ContinuousFileProvider for OnePath {
                fn acquire<'a>(
                    &'a mut self,
                    _: ContinuousOutputRequest,
                ) -> Pin<Box<dyn Future<Output = ContinuousResult<std::path::PathBuf>> + 'a>>
                {
                    Box::pin(async {
                        self.0
                            .take()
                            .ok_or_else(|| fail(ContinuousErrorKind::InvalidOptions))
                    })
                }
            }
            if self.options.changes == TimelineChangePolicy::Split
                || self.options.missing == MissingSegmentPolicy::Split
            {
                return self.finish(Err(fail(ContinuousErrorKind::InvalidOptions)));
            }
            self.write_to_files(&mut OnePath(Some(path.as_ref().to_path_buf())), options)
                .await
        }
        pub async fn write_to_files<P: ContinuousFileProvider>(
            self,
            provider: &mut P,
            options: FileOutputOptions,
        ) -> ContinuousResult<ContinuousReport> {
            let result = self
                .run(&mut Files {
                    provider,
                    options,
                    shared: self.shared.clone(),
                })
                .await;
            self.finish(result)
        }
    }
}
