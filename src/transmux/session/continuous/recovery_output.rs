use super::*;
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, SeekFrom};

// Fault injection is absent from non-test builds, including release archives.
macro_rules! fault {
    ($stage:literal) => {{
        #[cfg(test)]
        faults::hit($stage).map_err(|e| output_error(e.into()))?;
    }};
}
#[cfg(test)]
#[path = "recovery_faults.rs"]
mod faults;

struct FileWriter {
    file: tokio::fs::File,
    hash: Sha256,
    bytes: u64,
}
impl AsyncWrite for FileWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        #[cfg(test)]
        if let Err(error) = faults::hit("write.before") {
            return Poll::Ready(Err(error));
        }
        match Pin::new(&mut self.file).poll_write(cx, data) {
            Poll::Ready(Ok(n)) => {
                self.hash.update(&data[..n]);
                self.bytes += n as u64;
                #[cfg(test)]
                if let Err(error) = faults::hit("write.after") {
                    return Poll::Ready(Err(error));
                }
                Poll::Ready(Ok(n))
            }
            other => other,
        }
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.file).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.file).poll_shutdown(cx)
    }
}
struct RecoverableFile {
    root: std::path::PathBuf,
    outputs: Vec<(u64, [u8; 32])>,
    path: std::path::PathBuf,
    partial: std::path::PathBuf,
    identity: [u8; 32],
    recovery: RecoveryOptions,
    checkpoint: Option<EngineCheckpoint>,
    last: Option<EngineCheckpoint>,
    shared: Arc<Shared>,
}
fn child_path(root: &Path, index: u64) -> std::path::PathBuf {
    if index == 0 {
        return root.to_owned();
    }
    let mut name = root.as_os_str().to_os_string();
    name.push(format!(".part-{index:06}.mp4"));
    name.into()
}
fn partial_path(path: &Path) -> std::path::PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".hls-partial");
    name.into()
}
fn verify_outputs(
    root: &Path,
    outputs: &[(u64, [u8; 32])],
    signal: &Signal,
) -> ContinuousResult<()> {
    for (index, (size, digest)) in outputs.iter().enumerate() {
        let mut file = std::fs::File::open(child_path(root, index as u64))
            .map_err(|e| output_error(e.into()))?;
        if file.metadata().map_err(|e| output_error(e.into()))?.len() != *size
            || <[u8; 32]>::from(prefix(&mut file, *size, signal)?.finalize()) != *digest
        {
            return Err(corruption());
        }
    }
    Ok(())
}
fn corruption() -> ContinuousError {
    fail(ContinuousErrorKind::ResumeCorruption)
}
fn conflict() -> ContinuousError {
    fail(ContinuousErrorKind::ResumeConflict)
}
fn prefix(file: &mut std::fs::File, size: u64, signal: &Signal) -> ContinuousResult<Sha256> {
    if file.metadata().map_err(|e| output_error(e.into()))?.len() < size {
        return Err(corruption());
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|e| output_error(e.into()))?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    let mut remaining = size;
    while remaining > 0 {
        if signal.is_cancelled() {
            return Err(fail(ContinuousErrorKind::Cancelled));
        }
        let n = remaining.min(buffer.len() as u64) as usize;
        file.read_exact(&mut buffer[..n])
            .map_err(|_| corruption())?;
        hash.update(&buffer[..n]);
        remaining -= n as u64;
    }
    Ok(hash)
}
impl Target for RecoverableFile {
    type Writer = FileWriter;
    fn recoverable(&self) -> bool {
        true
    }
    fn begin<'a>(
        &'a mut self,
        session: &'a ContinuousSession,
        engine: &'a Engine,
    ) -> Pin<Box<dyn Future<Output = ContinuousResult<()>> + 'a>> {
        Box::pin(async move {
            if self.checkpoint.is_some() {
                return Ok(());
            }
            if self.path.exists() || self.partial.exists() {
                return Err(conflict());
            }
            let configuration = session
                .multi
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .configuration;
            let cp = EngineCheckpoint {
                configuration,
                prefix: Sha256::new().finalize().into(),
                bytes: 0,
                output: engine.output,
                sequence: 1,
                state: engine.save(session)?,
                destination: self.identity,
                finalizing: false,
                completed: false,
                classic: self.recovery.format != OutputFormat::FragmentedMp4,
                publication: None,
                sealed: false,
                outputs: vec![],
                synced: self.recovery.durability == CheckpointDurability::SyncAll,
            };
            if cp.to_bytes().len() > session.options.limits.metadata {
                return Err(fail(ContinuousErrorKind::BudgetExceeded));
            }
            (self.recovery.callback)(cp.clone())?;
            self.last = Some(cp);
            Ok(())
        })
    }
    fn acquire<'a>(
        &'a mut self,
        request: ContinuousOutputRequest,
    ) -> Pin<Box<dyn Future<Output = ContinuousResult<FileWriter>> + 'a>> {
        Box::pin(async move {
            self.path = child_path(&self.root, request.index());
            self.partial = partial_path(&self.path);
            // A freshly persisted acquisition intent does not own an existing
            // file. Only restoration of this output may reopen its partial.
            let resuming = self
                .checkpoint
                .as_ref()
                .is_some_and(|cp| cp.output == request.index());
            self.checkpoint = self.last.clone().or_else(|| self.checkpoint.clone());
            if let Some(checkpoint) = &self.checkpoint {
                if checkpoint.destination != self.identity
                    || checkpoint.classic != (self.recovery.format != OutputFormat::FragmentedMp4)
                {
                    return Err(conflict());
                }
                if checkpoint.output != request.index() {
                    return Err(conflict());
                }
                let root = self.root.clone();
                let path = self.partial.clone();
                let final_path = self.path.clone();
                let cp = checkpoint.clone();
                let signal = self.shared.signal.clone();
                let (file, hash) = tokio::task::spawn_blocking(move || {
                    verify_outputs(&root, &cp.outputs, &signal)?;
                    if cp.bytes == 0 && final_path.exists() {
                        return Err(conflict());
                    }
                    let mut file = std::fs::OpenOptions::new()
                        .create_new(!resuming)
                        .create(resuming && cp.bytes == 0)
                        .truncate(false)
                        .read(true)
                        .write(true)
                        .open(&path)
                        .map_err(|e| output_error(e.into()))?;
                    let hash = prefix(&mut file, cp.bytes, &signal)?;
                    if <[u8; 32]>::from(hash.clone().finalize()) != cp.prefix {
                        return Err(corruption());
                    }
                    let check = || {
                        if signal.is_cancelled() {
                            Err(Error::Cancelled)
                        } else {
                            Ok(())
                        }
                    };
                    if cp.bytes != 0 {
                        let scan =
                            crate::isobmff::scan_file(&mut file, cp.bytes, false, false, &check)
                                .map_err(|_| corruption())?;
                        if scan.fragments as u64 + 1 != u64::from(cp.sequence) {
                            return Err(corruption());
                        }
                    }
                    if let Some((name, size, digest)) = &cp.publication {
                        let ready = Path::new(name);
                        if ready.parent() != final_path.parent() {
                            return Err(conflict());
                        }
                        let verify_path = if final_path.exists() {
                            &final_path
                        } else {
                            ready
                        };
                        let mut ready =
                            std::fs::File::open(verify_path).map_err(|e| output_error(e.into()))?;
                        if ready.metadata().map_err(|e| output_error(e.into()))?.len() != *size
                            || <[u8; 32]>::from(prefix(&mut ready, *size, &signal)?.finalize())
                                != *digest
                        {
                            return Err(corruption());
                        }
                    }
                    if final_path.exists() {
                        // A crash after publication but before checkpoint persistence.
                        if !(cp.finalizing || cp.sealed) {
                            return Err(conflict());
                        }
                        let mut final_file =
                            std::fs::File::open(&final_path).map_err(|e| output_error(e.into()))?;
                        let (size, digest) = cp
                            .publication
                            .as_ref()
                            .map(|(_, n, h)| (*n, *h))
                            .unwrap_or((cp.bytes, cp.prefix));
                        fault!("flush.after");
                        if cp.classic && cp.publication.is_none() {
                            return Err(conflict());
                        }
                        if final_file
                            .metadata()
                            .map_err(|e| output_error(e.into()))?
                            .len()
                            != size
                            || <[u8; 32]>::from(prefix(&mut final_file, size, &signal)?.finalize())
                                != digest
                        {
                            return Err(conflict());
                        }
                        if file.metadata().map_err(|e| output_error(e.into()))?.len() != cp.bytes {
                            return Err(conflict());
                        }
                    }
                    check().map_err(output_error)?;
                    file.set_len(cp.bytes).map_err(|e| output_error(e.into()))?;
                    file.seek(SeekFrom::Start(cp.bytes))
                        .map_err(|e| output_error(e.into()))?;
                    Ok((file, hash))
                })
                .await
                .map_err(|_| fail(ContinuousErrorKind::Output))??;
                Ok(FileWriter {
                    file: tokio::fs::File::from_std(file),
                    hash,
                    bytes: checkpoint.bytes,
                })
            } else {
                if self.path.exists() {
                    return Err(conflict());
                }
                let file = tokio::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .read(true)
                    .open(&self.partial)
                    .await
                    .map_err(|e| output_error(e.into()))?;
                Ok(FileWriter {
                    file,
                    hash: Sha256::new(),
                    bytes: 0,
                })
            }
        })
    }
    fn checkpoint<'a>(
        &'a mut self,
        writer: &'a mut FileWriter,
        session: &'a ContinuousSession,
        engine: &'a Engine,
        sequence: u32,
        sealing: bool,
    ) -> Pin<Box<dyn Future<Output = ContinuousResult<()>> + 'a>> {
        Box::pin(async move {
            let state = engine.save(session)?;
            fault!("flush.before");
            writer
                .file
                .flush()
                .await
                .map_err(|e| output_error(e.into()))?;
            fault!("flush.after");
            if self.recovery.durability == CheckpointDurability::SyncAll {
                fault!("sync.before");
                writer
                    .file
                    .sync_all()
                    .await
                    .map_err(|e| output_error(e.into()))?;
            }
            fault!("sync.after");
            session.check()?;
            let acquiring = engine.part_bytes == 0;
            if acquiring {
                let next = child_path(&self.root, engine.output);
                if next.exists() || partial_path(&next).exists() {
                    return Err(conflict());
                }
            }
            let cp = EngineCheckpoint {
                configuration: session
                    .multi
                    .as_ref()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .configuration,
                prefix: if acquiring {
                    Sha256::new().finalize().into()
                } else {
                    writer.hash.clone().finalize().into()
                },
                bytes: engine.part_bytes,
                output: engine.output,
                sequence,
                state,
                destination: self.identity,
                finalizing: session.shared.inner.lock().unwrap().state
                    == ContinuousState::Finalizing,
                completed: false,
                classic: self.recovery.format != OutputFormat::FragmentedMp4,
                publication: self
                    .last
                    .as_ref()
                    .or(self.checkpoint.as_ref())
                    .filter(|c| c.output == engine.output)
                    .and_then(|c| c.publication.clone()),
                sealed: sealing,
                outputs: self.outputs.clone(),
                synced: self.recovery.durability == CheckpointDurability::SyncAll,
            };
            if cp.to_bytes().len() > session.options.limits.metadata {
                return Err(fail(ContinuousErrorKind::BudgetExceeded));
            }
            (self.recovery.callback)(cp.clone())?;
            self.last = Some(cp);
            session.check()
        })
    }
    fn finish<'a>(
        &'a mut self,
        writer: &'a mut FileWriter,
        report: &'a mut ContinuousOutputReport,
        final_output: bool,
    ) -> Pin<Box<dyn Future<Output = ContinuousResult<()>> + 'a>> {
        Box::pin(async move {
            let mut cp = self.last.clone().ok_or_else(corruption)?;
            fault!("flush.before");
            writer
                .file
                .flush()
                .await
                .map_err(|e| output_error(e.into()))?;
            fault!("flush.after");
            if cp.classic && cp.publication.is_none() {
                let source = self.partial.clone();
                let target = self.path.clone();
                let signal = self.shared.signal.clone();
                let sync = self.recovery.durability == CheckpointDurability::SyncAll;
                let (mut temporary, size, digest, tracks, samples) =
                    tokio::task::spawn_blocking(move || -> ContinuousResult<_> {
                        let check = || {
                            if signal.is_cancelled() {
                                Err(Error::Cancelled)
                            } else {
                                Ok(())
                            }
                        };
                        let mut input =
                            std::fs::File::open(&source).map_err(|e| output_error(e.into()))?;
                        let limit = input.metadata().map_err(|e| output_error(e.into()))?.len();
                        let scan =
                            crate::isobmff::scan_file(&mut input, limit, true, false, &check)
                                .map_err(media_error)?;
                        let tracks =
                            crate::isobmff::file_tracks(&scan.init).map_err(media_error)?;
                        let samples = scan.sample_counts.iter().sum::<usize>();
                        let (temp, mut file) = temporary_file(&target).map_err(output_error)?;
                        fault!("classic.write.before");
                        let (size, tracks) = Mp4Muxer::from_fragments(tracks, scan.samples)
                            .write_file(&mut input, &mut file, &check)
                            .map_err(output_error)?;
                        fault!("classic.write.after");
                        fault!("classic.flush.before");
                        std::io::Write::flush(&mut file).map_err(|e| output_error(e.into()))?;
                        fault!("classic.flush.after");
                        if sync {
                            fault!("classic.sync.before");
                            file.sync_all().map_err(|e| output_error(e.into()))?;
                        }
                        fault!("classic.sync.after");
                        drop(file);
                        let mut read =
                            std::fs::File::open(&temp.0).map_err(|e| output_error(e.into()))?;
                        let digest: [u8; 32] = prefix(&mut read, size, &signal)?.finalize().into();
                        Ok((temp, size, digest, tracks, samples))
                    })
                    .await
                    .map_err(|_| fail(ContinuousErrorKind::Output))??;
                if self.shared.signal.is_cancelled() {
                    return Err(fail(ContinuousErrorKind::Cancelled));
                }
                let name = temporary
                    .0
                    .to_str()
                    .ok_or_else(|| fail(ContinuousErrorKind::InvalidOptions))?
                    .to_owned();
                // Transfer ownership before handing the checkpoint to external persistence.
                let _retained = std::mem::take(&mut temporary.0);
                report.media.bytes_written = size;
                report.media.tracks = tracks;
                report.classic_index_samples = samples as u64;
                cp.publication = Some((name, size, digest));
                (self.recovery.callback)(cp.clone())?;
            }
            if let Some((_, size, _)) = &cp.publication {
                report.media.bytes_written = *size;
                report.classic_index_samples = report
                    .media
                    .tracks
                    .iter()
                    .map(|t| t.sample_count as u64)
                    .sum();
            }

            {
                let mut state = self.shared.inner.lock().unwrap();
                if self.shared.signal.is_cancelled() {
                    return Err(fail(ContinuousErrorKind::Cancelled));
                }
                if !self.path.exists() {
                    fault!("publish.before");
                    std::fs::hard_link(
                        cp.publication
                            .as_ref()
                            .map(|p| Path::new(&p.0))
                            .unwrap_or(&self.partial),
                        &self.path,
                    )
                    .map_err(|e| output_error(e.into()))?;
                } else if self
                    .checkpoint
                    .as_ref()
                    .is_none_or(|c| !(c.finalizing || c.sealed))
                {
                    return Err(conflict());
                }
                state.published = final_output;
            }
            cp.completed = final_output;
            self.last = Some(cp.clone());
            #[cfg(test)]
            let publication = faults::hit("publish.after").map_err(|e| output_error(e.into()));
            #[cfg(not(test))]
            let publication: ContinuousResult<()> = Ok(());
            if let Err(error) = publication.and_then(|_| (self.recovery.callback)(cp)) {
                // Publication already succeeded. Preserve that result even if
                // checkpoint persistence fails; an older finalizing checkpoint
                // can verify and recover it without downloading media again.
                let mut state = self.shared.inner.lock().unwrap();
                state.completed.push_back(report.clone());
                while state.completed.len() > self.shared.options.limits.history {
                    state.completed.pop_front();
                }
                return Err(error);
            }
            if !final_output {
                let identity = self
                    .last
                    .as_ref()
                    .unwrap()
                    .publication
                    .as_ref()
                    .map(|(_, size, digest)| (*size, *digest))
                    .unwrap_or((writer.bytes, writer.hash.clone().finalize().into()));
                self.outputs.push(identity);
            }
            // The durable partial is retained so an older finalizing checkpoint can
            // verify a publication interrupted before its final callback persisted.
            Ok(())
        })
    }
}
impl ContinuousSession {
    async fn completed_file(
        &self,
        path: std::path::PathBuf,
        identity: [u8; 32],
        recovery: &RecoveryOptions,
        checkpoint: &EngineCheckpoint,
    ) -> ContinuousResult<ContinuousReport> {
        if checkpoint.destination != identity
            || checkpoint.classic != (recovery.format != OutputFormat::FragmentedMp4)
        {
            return Err(conflict());
        }
        self.state(ContinuousState::Preparing)?;
        let mut engine = Engine::restore(self, &checkpoint.state).await?;
        if engine.output != checkpoint.output || engine.part_bytes != checkpoint.bytes {
            return Err(corruption());
        }
        let (size, digest) = checkpoint
            .publication
            .as_ref()
            .map(|(_, n, h)| (*n, *h))
            .unwrap_or((checkpoint.bytes, checkpoint.prefix));
        let signal = self.shared.signal.clone();
        // A durable Completed checkpoint no longer needs intermediate files.
        // Verify the final artifact read-only, without any source/key/cue I/O.
        let outputs = checkpoint.outputs.clone();
        let index = checkpoint.output;
        tokio::task::spawn_blocking(move || {
            verify_outputs(&path, &outputs, &signal)?;
            let mut file = std::fs::File::open(child_path(&path, index))
                .map_err(|e| output_error(e.into()))?;
            if file.metadata().map_err(|e| output_error(e.into()))?.len() != size
                || <[u8; 32]>::from(prefix(&mut file, size, &signal)?.finalize()) != digest
            {
                return Err(corruption());
            }
            Ok(())
        })
        .await
        .map_err(|_| fail(ContinuousErrorKind::Output))??;
        {
            let mut state = self.shared.inner.lock().unwrap();
            self.check()?;
            state.published = true;
        }
        let mut report = engine.output_report();
        report.media.bytes_written = size;
        if checkpoint.classic {
            report.classic_index_samples = report
                .media
                .tracks
                .iter()
                .map(|t| t.sample_count as u64)
                .sum();
        }
        engine.bytes = engine
            .bytes
            .checked_sub(engine.part_bytes)
            .and_then(|n| n.checked_add(size))
            .ok_or_else(corruption)?;
        self.record_output(&report);
        engine.outputs.push_back(report.clone());
        engine.retain_history(self);
        self.emit(ContinuousEvent::Output(report))?;
        engine.report(self)
    }
    pub(crate) async fn recoverable_file(
        self,
        path: &Path,
        recovery: RecoveryOptions,
    ) -> ContinuousResult<ContinuousReport> {
        self.resources.enable_recovery();
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let directory = std::fs::canonicalize(parent).map_err(|e| output_error(e.into()))?;
        let name = path
            .file_name()
            .ok_or_else(|| fail(ContinuousErrorKind::InvalidOptions))?;
        let path = directory.join(name);
        let identity = crate::resume::digest(path.as_os_str().as_encoded_bytes());
        if let Some(checkpoint) = &self.recovery
            && checkpoint.completed
        {
            let result = self
                .completed_file(path, identity, &recovery, checkpoint)
                .await;
            return self.finish(result);
        }
        let mut partial = path.as_os_str().to_os_string();
        partial.push(".hls-partial");
        let result = self
            .run(&mut RecoverableFile {
                root: path.clone(),
                outputs: self
                    .recovery
                    .as_ref()
                    .map(|c| c.outputs.clone())
                    .unwrap_or_default(),
                path,
                partial: partial.into(),
                identity,
                recovery,
                checkpoint: self.recovery.clone(),
                last: None,
                shared: self.shared.clone(),
            })
            .await;
        self.finish(result)
    }
}
