//! Fixed destinations are supplied by the SDK; Engine owns prefix validation.
use super::super::super::recovery::SidecarCheckpoint;
use super::*;

#[derive(Clone)]
pub(super) struct SidecarFiles {
    sink: Arc<dyn RecoverableSubtitleSink>,
    destinations: Vec<SubtitleDestination>,
    pub(super) identity: [u8; 32],
}
pub(super) struct SidecarPrefix {
    path: std::path::PathBuf,
    file: Option<std::fs::File>,
    bytes: u64,
}
pub(super) fn regular_file(path: &Path, writable: bool) -> ContinuousResult<Option<std::fs::File>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() => Err(conflict()),
        Ok(_) => std::fs::OpenOptions::new()
            .read(true)
            .write(writable)
            .open(path)
            .map(Some)
            .map_err(|e| output_error(e.into())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(output_error(e.into())),
    }
}
impl SidecarFiles {
    pub(super) fn new(session: &ContinuousSession, media: &Path) -> ContinuousResult<Option<Self>> {
        let Some(sink) = &session.recoverable_subtitles else {
            return Ok(None);
        };
        let mut destinations = sink.destinations();
        let multi = session.multi.as_ref().unwrap().lock().unwrap();
        if destinations.is_empty()
            || destinations.len() != multi.subtitles.len()
            || sink.format_identity().is_empty()
            || sink.format_identity().len() > 4096
        {
            return Err(fail(ContinuousErrorKind::InvalidOptions));
        }
        destinations.sort_by(|a, b| a.input.as_str().cmp(b.input.as_str()));
        let mut identity = Vec::new();
        use crate::state_codec::StateCodec;
        sink.format_identity().to_owned().put(&mut identity);
        for (index, destination) in destinations.iter_mut().enumerate() {
            if !multi
                .subtitles
                .iter()
                .any(|lane| lane.config.id == destination.input)
            {
                return Err(conflict());
            }
            let parent = destination
                .path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            let parent = std::fs::canonicalize(parent).map_err(|e| output_error(e.into()))?;
            let name = destination.path.file_name().ok_or_else(conflict)?;
            destination.path = parent.join(name);
            destination.input.as_str().to_owned().put(&mut identity);
            destination
                .path
                .as_os_str()
                .as_encoded_bytes()
                .to_vec()
                .put(&mut identity);
            // Include the ordinal, so a reordered/changed selection cannot alias.
            index.put(&mut identity);
        }
        // Reserve each root's complete split/partial namespace, including media.
        let paths: Vec<_> = std::iter::once(media)
            .chain(destinations.iter().map(|d| d.path.as_path()))
            .collect();
        for (i, a) in paths.iter().enumerate() {
            for b in &paths[i + 1..] {
                let a = a.as_os_str().as_encoded_bytes();
                let b = b.as_os_str().as_encoded_bytes();
                let overlaps = |a: &[u8], b: &[u8]| {
                    b == a
                        || b.strip_prefix(a).is_some_and(|s| {
                            s.starts_with(b".part-") || s.starts_with(b".hls-partial")
                        })
                };
                if overlaps(a, b) || overlaps(b, a) {
                    return Err(conflict());
                }
            }
        }
        if destinations.windows(2).any(|d| d[0].input == d[1].input) {
            return Err(conflict());
        }
        Ok(Some(Self {
            sink: sink.clone(),
            destinations,
            identity: crate::resume::digest(&identity),
        }))
    }
    pub(super) fn snapshot(
        &self,
        previous: Option<&SidecarCheckpoint>,
        output: u64,
        acquiring: bool,
        sync: bool,
        signal: &Signal,
        next_receipt: u64,
    ) -> ContinuousResult<SidecarCheckpoint> {
        let start = usize::try_from(output)
            .ok()
            .and_then(|o| o.checked_mul(self.destinations.len()))
            .ok_or_else(corruption)?;
        let mut files = previous.map(|p| p.files.clone()).unwrap_or_default();
        if files.len() < start {
            return Err(corruption());
        }
        files.truncate(start);
        for destination in &self.destinations {
            let path = destination.partial_path(output);
            if acquiring {
                if std::fs::symlink_metadata(&path).is_ok()
                    || std::fs::symlink_metadata(destination.final_path(output)).is_ok()
                {
                    return Err(conflict());
                }
                files.push((0, crate::resume::digest(&[])));
            } else {
                let mut file = regular_file(&path, true)?.ok_or_else(corruption)?;
                let bytes = file.metadata().map_err(|e| output_error(e.into()))?.len();
                if sync {
                    fault!("sidecar.sync.before");
                    file.sync_all().map_err(|e| output_error(e.into()))?;
                    fault!("sidecar.sync.after");
                }
                files.push((bytes, prefix(&mut file, bytes, signal)?.finalize().into()));
            }
        }
        Ok(SidecarCheckpoint {
            configuration: self.identity,
            files,
            next_receipt,
        })
    }
    // Read-only across every sidecar. Return open handles, but mutate none here.
    pub(super) fn validate(
        &self,
        cp: &EngineCheckpoint,
        signal: &Signal,
        completed: bool,
    ) -> ContinuousResult<Vec<SidecarPrefix>> {
        let saved = cp.sidecars.as_ref().ok_or_else(conflict)?;
        let count = cp
            .output
            .checked_add(1)
            .and_then(|o| o.checked_mul(self.destinations.len() as u64))
            .ok_or_else(corruption)?;
        if saved.configuration != self.identity {
            return Err(conflict());
        }
        if saved.files.len() as u64 != count {
            return Err(corruption());
        }
        let mut active = Vec::new();
        for (index, (bytes, hash)) in saved.files.iter().enumerate() {
            let output = (index / self.destinations.len()) as u64;
            let destination = &self.destinations[index % self.destinations.len()];
            let final_file = regular_file(&destination.final_path(output), false)?;
            let final_exists = final_file.is_some();
            if let Some(mut file) = final_file {
                if output == cp.output && !(cp.finalizing || cp.sealed) {
                    return Err(conflict());
                }
                if file.metadata().map_err(|e| output_error(e.into()))?.len() != *bytes
                    || <[u8; 32]>::from(prefix(&mut file, *bytes, signal)?.finalize()) != *hash
                {
                    return Err(corruption());
                }
            } else if output < cp.output || completed {
                return Err(corruption());
            }
            if output < cp.output || completed {
                continue;
            }
            let path = destination.partial_path(output);
            let mut file = regular_file(&path, true)?;
            match &mut file {
                Some(file) => {
                    if <[u8; 32]>::from(prefix(file, *bytes, signal)?.finalize()) != *hash {
                        return Err(corruption());
                    }
                    if final_exists
                        && file.metadata().map_err(|e| output_error(e.into()))?.len() != *bytes
                    {
                        return Err(conflict());
                    }
                }
                None if *bytes != 0 || final_exists => return Err(corruption()),
                None => {}
            }
            active.push(SidecarPrefix {
                path,
                file,
                bytes: *bytes,
            });
        }
        Ok(active)
    }
    pub(super) fn truncate(prefixes: Vec<SidecarPrefix>) -> ContinuousResult<()> {
        for prefix in prefixes {
            let file = match prefix.file {
                Some(file) => file,
                None => std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(prefix.path)
                    .map_err(|e| output_error(e.into()))?,
            };
            fault!("sidecar.truncate.before");
            file.set_len(prefix.bytes)
                .map_err(|e| output_error(e.into()))?;
            fault!("sidecar.truncate.after");
        }
        Ok(())
    }
    pub(super) async fn publish(
        &self,
        cp: &EngineCheckpoint,
        signal: &Signal,
    ) -> ContinuousResult<()> {
        fault!("sidecar.publish.before");
        self.sink
            .publish(cp.output)
            .await
            .map_err(super::super::sidecar::sidecar_error)?;
        fault!("sidecar.publish.after");
        // Every selected track must have been published before media success.
        self.validate(cp, signal, true)?;
        Ok(())
    }
}

// A final and its own retained partial may be hard links. Different outputs or
// tracks must never alias the same inode: truncating one would mutate another.
#[cfg(unix)]
pub(super) fn verify_aliases(
    root: &Path,
    cp: &EngineCheckpoint,
    sidecars: Option<&SidecarFiles>,
) -> ContinuousResult<()> {
    use std::os::unix::fs::MetadataExt;
    let mut owners = std::collections::BTreeMap::new();
    let mut check = |path: std::path::PathBuf, owner: (usize, u64)| -> ContinuousResult<()> {
        match std::fs::symlink_metadata(&path) {
            Ok(m) => {
                if !m.file_type().is_file() {
                    return Err(conflict());
                }
                if owners
                    .insert((m.dev(), m.ino()), owner)
                    .is_some_and(|previous| previous != owner)
                {
                    return Err(conflict());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(output_error(e.into())),
        }
        Ok(())
    };
    for output in 0..=cp.output {
        let path = child_path(root, output);
        check(partial_path(&path), (0, output))?;
        check(path, (0, output))?;
        if let Some(sidecars) = sidecars {
            for (index, destination) in sidecars.destinations.iter().enumerate() {
                check(destination.partial_path(output), (index + 1, output))?;
                check(destination.final_path(output), (index + 1, output))?;
            }
        }
    }
    if let Some((path, _, _)) = &cp.publication {
        check(path.into(), (0, cp.output))?;
    }
    Ok(())
}

// Keep admission closed across the final sink acknowledgement, state snapshot
// and persistence callback. Otherwise a concurrently rejected late cue could be
// assigned a receipt that the checkpoint records without delivering its result.
pub(super) struct CheckpointAdmission(Arc<Shared>);
impl CheckpointAdmission {
    pub(super) fn new(session: &ContinuousSession) -> Self {
        session.shared.inner.lock().unwrap().checkpointing = true;
        Self(session.shared.clone())
    }
}
impl Drop for CheckpointAdmission {
    fn drop(&mut self) {
        self.0.inner.lock().unwrap().checkpointing = false;
        self.0.signal.wake();
    }
}
