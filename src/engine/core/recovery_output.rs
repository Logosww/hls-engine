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

#[path = "recovery_sidecars.rs"]
mod sidecars;
use sidecars::*;

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
    sidecars: Option<SidecarFiles>,
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
// Kept independently of caller-owned checkpoints, including older snapshots.
// Its presence is a conservative barrier: even an untouched partial is refused.
fn in_place_marker(path: &Path) -> std::path::PathBuf {
    let mut name = partial_path(path).into_os_string();
    name.push(".in-place");
    name.into()
}
fn reject_in_place_marker(path: &Path) -> EngineResult<()> {
    match std::fs::symlink_metadata(in_place_marker(path)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(conflict()),
        Err(e) => Err(output_error(e.into())),
    }
}
fn mark_in_place(path: &Path) -> EngineResult<()> {
    let marker = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(in_place_marker(path))
        .map_err(|e| output_error(e.into()))?;
    marker.sync_all().map_err(|e| output_error(e.into()))?;
    #[cfg(unix)]
    std::fs::File::open(path.parent().ok_or_else(conflict)?)
        .and_then(|dir| dir.sync_all())
        .map_err(|e| output_error(e.into()))?;
    Ok(())
}
fn reserve_header(file: &mut std::fs::File, reserve: u64, hash: &mut Sha256) -> EngineResult<()> {
    use std::io::Write;
    if reserve == 0 {
        return Ok(());
    }
    let mut header = (reserve as u32).to_be_bytes().to_vec();
    header.extend_from_slice(b"free");
    file.write_all(&header)
        .map_err(|e| output_error(e.into()))?;
    file.set_len(reserve).map_err(|e| output_error(e.into()))?;
    hash.update(&header);
    let zero = [0u8; 8192];
    let mut left = reserve - 8;
    while left > 0 {
        let n = left.min(zero.len() as u64) as usize;
        hash.update(&zero[..n]);
        left -= n as u64;
    }
    file.seek(SeekFrom::Start(reserve))
        .map_err(|e| output_error(e.into()))?;
    Ok(())
}
fn verify_outputs(root: &Path, outputs: &[(u64, [u8; 32])], signal: &Signal) -> EngineResult<()> {
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
fn corruption() -> EngineError {
    fail(EngineErrorKind::ResumeCorruption)
}
fn conflict() -> EngineError {
    fail(EngineErrorKind::ResumeConflict)
}
fn prefix(file: &mut std::fs::File, size: u64, signal: &Signal) -> EngineResult<Sha256> {
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
            return Err(fail(EngineErrorKind::Cancelled));
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
    fn prefix_bytes(&self) -> u64 {
        if self.recovery.in_place() {
            u64::from(self.recovery.header_reserve)
        } else {
            0
        }
    }
    type Writer = FileWriter;
    fn recoverable(&self) -> bool {
        true
    }
    fn begin<'a>(
        &'a mut self,
        session: &'a SessionCore,
        engine: &'a Engine,
    ) -> Pin<Box<dyn Future<Output = EngineResult<()>> + 'a>> {
        Box::pin(async move {
            if self.checkpoint.is_some() {
                return Ok(());
            }
            let _admission = CheckpointAdmission::new(session);
            if self.path.exists() || self.partial.exists() {
                return Err(conflict());
            }
            let (configuration, next_receipt) = {
                let multi = session.multi.as_ref().unwrap().lock().unwrap();
                (multi.configuration, multi.next_receipt)
            };
            let cp = EngineCheckpoint {
                aac_repair: session.options.aac.enabled(),
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
                sidecars: self
                    .sidecars
                    .as_ref()
                    .map(|s| {
                        s.snapshot(
                            None,
                            engine.output,
                            true,
                            false,
                            &self.shared.signal,
                            next_receipt,
                        )
                    })
                    .transpose()?,
            };
            if cp.to_bytes().len() > session.options.limits.metadata {
                return Err(fail(EngineErrorKind::BudgetExceeded));
            }
            (self.recovery.callback)(cp.clone())?;
            self.last = Some(cp);
            Ok(())
        })
    }
    fn acquire<'a>(
        &'a mut self,
        request: EngineOutputRequest,
    ) -> Pin<Box<dyn Future<Output = EngineResult<FileWriter>> + 'a>> {
        Box::pin(async move {
            self.path = child_path(&self.root, request.index());
            self.partial = partial_path(&self.path);
            reject_in_place_marker(&self.path)?;
            let reserve = self.prefix_bytes();
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
                let sidecars = self.sidecars.clone();
                let (file, hash) = tokio::task::spawn_blocking(move || {
                    #[cfg(unix)]
                    verify_aliases(&root, &cp, sidecars.as_ref())?;
                    let sidecar_prefixes = sidecars
                        .as_ref()
                        .map(|s| s.validate(&cp, &signal, false))
                        .transpose()?;
                    verify_outputs(&root, &cp.outputs, &signal)?;
                    if cp.bytes == 0 && final_path.exists() {
                        return Err(conflict());
                    }
                    // Do not create an acquisition-intent file until every
                    // destination and committed prefix has passed validation.
                    let existing = regular_file(&path, true)?;
                    if existing.is_some() && !resuming {
                        return Err(output_error(
                            std::io::Error::new(
                                std::io::ErrorKind::AlreadyExists,
                                "output partial already exists",
                            )
                            .into(),
                        ));
                    }
                    if existing.is_none() && cp.bytes != 0 {
                        return Err(corruption());
                    }
                    let mut file = existing;
                    let mut hash = if let Some(file) = &mut file {
                        prefix(file, cp.bytes, &signal)?
                    } else {
                        Sha256::new()
                    };

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
                        let scan = crate::isobmff::scan_file(
                            &mut crate::mp4::ReservedReader::new(
                                file.as_mut().ok_or_else(corruption)?,
                                reserve,
                            ),
                            cp.bytes.checked_sub(reserve).ok_or_else(corruption)?,
                            false,
                            false,
                            &check,
                        )
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
                        if file
                            .as_ref()
                            .ok_or_else(corruption)?
                            .metadata()
                            .map_err(|e| output_error(e.into()))?
                            .len()
                            != cp.bytes
                        {
                            return Err(conflict());
                        }
                    }
                    check().map_err(output_error)?;
                    if let Some(prefixes) = sidecar_prefixes {
                        SidecarFiles::truncate(prefixes)?;
                    }
                    let mut file = match file {
                        Some(file) => file,
                        None => std::fs::OpenOptions::new()
                            .read(true)
                            .write(true)
                            .create_new(true)
                            .open(&path)
                            .map_err(|e| output_error(e.into()))?,
                    };
                    file.set_len(cp.bytes).map_err(|e| output_error(e.into()))?;
                    file.seek(SeekFrom::Start(cp.bytes))
                        .map_err(|e| output_error(e.into()))?;
                    if cp.bytes == 0 {
                        reserve_header(&mut file, reserve, &mut hash)?;
                    }
                    Ok((file, hash))
                })
                .await
                .map_err(|_| fail(EngineErrorKind::Output))??;
                Ok(FileWriter {
                    file: tokio::fs::File::from_std(file),
                    hash,
                    bytes: if checkpoint.bytes == 0 {
                        reserve
                    } else {
                        checkpoint.bytes
                    },
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
        session: &'a SessionCore,
        engine: &'a Engine,
        sequence: u32,
        sealing: bool,
    ) -> Pin<Box<dyn Future<Output = EngineResult<()>> + 'a>> {
        Box::pin(async move {
            let _admission = CheckpointAdmission::new(session);
            session.commit_subtitles().await?;
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
            let (configuration, next_receipt) = {
                let multi = session.multi.as_ref().unwrap().lock().unwrap();
                (multi.configuration, multi.next_receipt)
            };
            let cp = EngineCheckpoint {
                aac_repair: session.options.aac.enabled(),
                configuration,
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
                finalizing: session.shared.inner.lock().unwrap().state == EngineState::Finalizing,
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
                sidecars: self
                    .sidecars
                    .as_ref()
                    .map(|s| {
                        s.snapshot(
                            self.last
                                .as_ref()
                                .or(self.checkpoint.as_ref())
                                .and_then(|c| c.sidecars.as_ref()),
                            engine.output,
                            acquiring,
                            self.recovery.durability == CheckpointDurability::SyncAll,
                            &self.shared.signal,
                            next_receipt,
                        )
                    })
                    .transpose()?,
            };
            if cp.to_bytes().len() > session.options.limits.metadata {
                return Err(fail(EngineErrorKind::BudgetExceeded));
            }
            (self.recovery.callback)(cp.clone())?;
            self.last = Some(cp);
            session.check()
        })
    }
    fn finish<'a>(
        &'a mut self,
        writer: &'a mut FileWriter,
        report: &'a mut EngineOutputReport,
        final_output: bool,
    ) -> Pin<Box<dyn Future<Output = EngineResult<()>> + 'a>> {
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
                let in_place = self.recovery.in_place();
                let reserve = self.prefix_bytes();
                let sync = self.recovery.durability == CheckpointDurability::SyncAll;
                let (mut temporary, ready_path, size, digest, tracks, samples) =
                    tokio::task::spawn_blocking(move || -> EngineResult<_> {
                        let check = || {
                            if signal.is_cancelled() {
                                Err(Error::Cancelled)
                            } else {
                                Ok(())
                            }
                        };
                        let mut input = std::fs::OpenOptions::new()
                            .read(true)
                            .write(in_place)
                            .open(&source)
                            .map_err(|e| output_error(e.into()))?;
                        let limit = input.metadata().map_err(|e| output_error(e.into()))?.len();
                        let scan = crate::isobmff::scan_file(
                            &mut crate::mp4::ReservedReader::new(&mut input, reserve),
                            limit.checked_sub(reserve).ok_or_else(corruption)?,
                            true,
                            false,
                            &check,
                        )
                        .map_err(media_error)?;
                        let tracks =
                            crate::isobmff::file_tracks(&scan.init).map_err(media_error)?;
                        let samples = scan.sample_counts.iter().sum::<usize>();
                        let mux = Mp4Muxer::from_fragments(tracks, scan.samples);
                        fault!("classic.write.before");
                        let (temp, ready_path, mut file, size, tracks) = if in_place {
                            check().map_err(output_error)?;
                            mark_in_place(&target)?;
                            fault!("in_place.marked");
                            let (size, tracks) = mux
                                .finalize_in_place(&mut input, &scan.init, limit, reserve, &check)
                                .map_err(output_error)?;
                            (None, source, input, size, tracks)
                        } else {
                            let (temp, mut file) = temporary_file(&target).map_err(output_error)?;
                            let (size, tracks) = mux
                                .write_file(&mut input, &mut file, &check)
                                .map_err(output_error)?;
                            let ready_path = temp.0.clone();
                            (Some(temp), ready_path, file, size, tracks)
                        };
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
                            std::fs::File::open(&ready_path).map_err(|e| output_error(e.into()))?;
                        let digest: [u8; 32] = prefix(&mut read, size, &signal)?.finalize().into();
                        Ok((temp, ready_path, size, digest, tracks, samples))
                    })
                    .await
                    .map_err(|_| fail(EngineErrorKind::Output))??;
                if self.shared.signal.is_cancelled() {
                    return Err(fail(EngineErrorKind::Cancelled));
                }
                let name = ready_path
                    .to_str()
                    .ok_or_else(|| fail(EngineErrorKind::InvalidOptions))?
                    .to_owned();
                // Transfer ownership before handing the checkpoint to external persistence.
                if let Some(temp) = &mut temporary {
                    let _retained = std::mem::take(&mut temp.0);
                }
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

            if let Some(sidecars) = &self.sidecars {
                sidecars.publish(&cp, &self.shared.signal).await?;
            }
            {
                let mut state = self.shared.inner.lock().unwrap();
                if self.shared.signal.is_cancelled() {
                    return Err(fail(EngineErrorKind::Cancelled));
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
            let publication: EngineResult<()> = Ok(());
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
            // Copy retains the original partial for finalization recovery.
            // InPlace retains the converted partial and barrier until the caller
            // durably persists Completed or explicitly abandons this recording.
            Ok(())
        })
    }
}
impl SessionCore {
    async fn completed_file(
        &self,
        path: std::path::PathBuf,
        identity: [u8; 32],
        recovery: &RecoveryOptions,
        checkpoint: &EngineCheckpoint,
        sidecars: Option<SidecarFiles>,
    ) -> EngineResult<EngineMediaReport> {
        if checkpoint.destination != identity
            || checkpoint.classic != (recovery.format != OutputFormat::FragmentedMp4)
        {
            return Err(conflict());
        }
        self.state(EngineState::Preparing)?;
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
        let cp = checkpoint.clone();
        tokio::task::spawn_blocking(move || {
            if let Some(sidecars) = sidecars {
                sidecars.validate(&cp, &signal, true)?;
            }
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
        .map_err(|_| fail(EngineErrorKind::Output))??;
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
        self.emit(EngineEvent::Output(report))?;
        engine.report(self)
    }
    pub(crate) async fn recoverable_file(
        self,
        path: &Path,
        recovery: RecoveryOptions,
    ) -> EngineResult<EngineMediaReport> {
        if self.subtitle_sink.is_some() && !self.durable_subtitles() {
            return self.finish(Err(fail(EngineErrorKind::InvalidOptions)));
        }
        if recovery.in_place() && (1..8).contains(&recovery.header_reserve) {
            return self.finish(Err(fail(EngineErrorKind::InvalidOptions)));
        }
        self.resources.enable_recovery();
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let directory = std::fs::canonicalize(parent).map_err(|e| output_error(e.into()))?;
        let name = path
            .file_name()
            .ok_or_else(|| fail(EngineErrorKind::InvalidOptions))?;
        let path = directory.join(name);
        let mut identity_bytes = path.as_os_str().as_encoded_bytes().to_vec();
        if recovery.in_place() {
            identity_bytes.extend_from_slice(b"\0hls-in-place-v1\0");
            identity_bytes.extend_from_slice(&recovery.header_reserve.to_be_bytes());
        }
        let identity = crate::resume::digest(&identity_bytes);
        if !self.recovery.as_ref().is_some_and(|cp| cp.completed)
            && let Err(error) = reject_in_place_marker(&child_path(
                &path,
                self.recovery.as_ref().map_or(0, |cp| cp.output),
            ))
        {
            return self.finish(Err(error));
        }
        let sidecars = match SidecarFiles::new(&self, &path) {
            Ok(sidecars) => sidecars,
            Err(error) => return self.finish(Err(error)),
        };
        if let Some(cp) = &self.recovery
            && (cp.sidecars.is_some() != sidecars.is_some()
                || cp
                    .sidecars
                    .as_ref()
                    .zip(sidecars.as_ref())
                    .is_some_and(|(c, s)| c.configuration != s.identity))
        {
            return self.finish(Err(conflict()));
        }
        if let Some(checkpoint) = &self.recovery
            && checkpoint.completed
        {
            let result = self
                .completed_file(path, identity, &recovery, checkpoint, sidecars)
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
                sidecars,
            })
            .await;
        self.finish(result)
    }
}
