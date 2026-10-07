//! Replay the shared media corpus through every persisted file checkpoint.
#![cfg_attr(target_arch = "wasm32", allow(dead_code))]
use super::*;
use hls_engine::legacy::{MultiTrackHandle as CoreHandle, MultiTrackSession as CoreSession};

#[derive(Clone)]
enum Command {
    Snapshot(InputId, Box<PlaylistSnapshot>),
    Cues(OutputTrackId, Vec<SubtitleCue>),
    End(OutputTrackId),
    Stop,
}
#[derive(Clone)]
pub struct Handle {
    core: CoreHandle,
    commands: Arc<Mutex<Vec<Command>>>,
}
impl Handle {
    pub fn accept_snapshot(
        &self,
        id: &InputId,
        snapshot: &PlaylistSnapshot,
    ) -> ContinuousResult<SnapshotAcceptance> {
        let result = self.core.accept_snapshot(id, snapshot)?;
        self.commands
            .lock()
            .unwrap()
            .push(Command::Snapshot(id.clone(), Box::new(snapshot.clone())));
        Ok(result)
    }
    pub fn subtitle_track_id(&self, id: &InputId) -> Option<OutputTrackId> {
        self.core.subtitle_track_id(id)
    }
    pub fn accept_cues(
        &self,
        track: OutputTrackId,
        cues: &[SubtitleCue],
    ) -> ContinuousResult<SubtitleAcceptance> {
        let result = self.core.accept_cues(track, cues)?;
        self.commands
            .lock()
            .unwrap()
            .push(Command::Cues(track, cues.to_vec()));
        Ok(result)
    }
    pub fn end_subtitles(&self, track: OutputTrackId) -> ContinuousResult<()> {
        self.core.end_subtitles(track)?;
        self.commands.lock().unwrap().push(Command::End(track));
        Ok(())
    }
    pub fn stop(&self) {
        self.core.stop();
        self.commands.lock().unwrap().push(Command::Stop);
    }
}
pub struct Session {
    core: CoreSession,
    inputs: MultiTrackInputs,
    provider: Arc<dyn KeyProvider>,
    options: ContinuousOptions,
    commands: Arc<Mutex<Vec<Command>>>,
}
impl Session {
    pub fn new(
        inputs: MultiTrackInputs,
        provider: Arc<dyn KeyProvider>,
        options: ContinuousOptions,
    ) -> ContinuousResult<Self> {
        let core = CoreSession::new(
            inputs.clone(),
            sample::keys(provider.clone()),
            options.clone(),
        )?;
        Ok(Self {
            core,
            inputs,
            provider,
            options,
            commands: Arc::new(Mutex::new(vec![])),
        })
    }
    pub fn handle(&self) -> Handle {
        Handle {
            core: self.core.handle(),
            commands: self.commands.clone(),
        }
    }
    pub async fn into_bytes(
        self,
        capacity: usize,
        format: OutputFormat,
    ) -> ContinuousResult<(Vec<u8>, MultiTrackReport)> {
        let result = self.core.into_bytes(capacity, format).await?;
        #[cfg(not(target_arch = "wasm32"))]
        if std::env::var_os("HLS_ENGINE_RECOVERY_MATRIX").is_some() {
            validate(
                self.inputs,
                self.provider,
                self.options,
                self.commands,
                format,
                Some(&result.0),
            )
            .await;
        }
        Ok(result)
    }
    pub async fn write_to<W: tokio::io::AsyncWrite + Unpin>(
        self,
        writer: &mut W,
    ) -> ContinuousResult<MultiTrackReport> {
        self.core.write_to(writer).await
    }
    pub async fn write_to_outputs<P: ContinuousWriterProvider>(
        self,
        provider: &mut P,
    ) -> ContinuousResult<MultiTrackReport> {
        let result = self.core.write_to_outputs(provider).await?;
        #[cfg(not(target_arch = "wasm32"))]
        if std::env::var_os("HLS_ENGINE_RECOVERY_MATRIX").is_some() {
            validate(
                self.inputs,
                self.provider,
                self.options,
                self.commands,
                OutputFormat::FragmentedMp4,
                None,
            )
            .await;
        }
        Ok(result)
    }
}

#[cfg(not(target_arch = "wasm32"))]
async fn validate(
    inputs: MultiTrackInputs,
    provider: Arc<dyn KeyProvider>,
    options: ContinuousOptions,
    commands: Arc<Mutex<Vec<Command>>>,
    format: OutputFormat,
    expected: Option<&Vec<u8>>,
) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let index = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = std::path::PathBuf::from(std::env::var_os("HLS_ENGINE_RECOVERY_MATRIX").unwrap())
        .join(format!("case-{index:03}"));
    std::fs::create_dir_all(&root).unwrap();
    let commands = commands.lock().unwrap().clone();
    let session = |cp: Option<EngineCheckpoint>| {
        let terminal = cp.as_ref().is_some_and(|c| c.is_finalizing());
        let s = match cp {
            Some(cp) => CoreSession::restore(
                inputs.clone(),
                sample::keys(provider.clone()),
                options.clone(),
                cp,
            ),
            None => CoreSession::new(
                inputs.clone(),
                sample::keys(provider.clone()),
                options.clone(),
            ),
        }
        .unwrap();
        if !terminal {
            let h = s.handle();
            for command in &commands {
                match command {
                    Command::Snapshot(id, snapshot) => {
                        h.accept_snapshot(id, snapshot).unwrap();
                    }
                    Command::Cues(track, cues) => {
                        h.accept_cues(*track, cues).unwrap();
                    }
                    Command::End(track) => {
                        h.end_subtitles(*track).unwrap();
                    }
                    Command::Stop => h.stop(),
                }
            }
        }
        s
    };
    let count = Arc::new(AtomicUsize::new(0));
    let copy = count.clone();
    let path = root.join("reference.mp4");
    let report = session(None)
        .write_recoverable_to_file(
            &path,
            RecoveryOptions::new(Arc::new(move |_| {
                copy.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }))
            .with_output_format(format),
        )
        .await
        .unwrap_or_else(|e| panic!("recovery case {index} reference: {e:?}"));
    let child = |path: &std::path::Path, n| {
        if n == 0 {
            return path.to_owned();
        }
        let mut name = path.as_os_str().to_os_string();
        name.push(format!(".part-{n:06}.mp4"));
        name.into()
    };
    let reference: Vec<_> = (0..report.media().outputs().len())
        .map(|n| sample::canonical(std::fs::read(child(&path, n)).unwrap()))
        .collect();
    if let Some(expected) = expected {
        assert_eq!(
            reference[0],
            sample::canonical(expected.clone()),
            "case {index}"
        );
    }
    let total = count.load(Ordering::Relaxed);
    for stop in 1..=total {
        let path = root.join(format!("interrupted-{stop}.mp4"));
        let latest = Arc::new(Mutex::new(None));
        let copy = latest.clone();
        let calls = AtomicUsize::new(0);
        let result = session(None)
            .write_recoverable_to_file(
                &path,
                RecoveryOptions::new(Arc::new(move |cp| {
                    *copy.lock().unwrap() = Some(cp);
                    if calls.fetch_add(1, Ordering::Relaxed) + 1 == stop {
                        return Err(ContinuousError::output(
                            std::io::Error::other("matrix interruption").into(),
                        ));
                    }
                    Ok(())
                }))
                .with_output_format(format),
            )
            .await;
        assert!(result.is_err(), "case {index} stop {stop}");
        let cp = latest.lock().unwrap().clone().unwrap();
        let cp = EngineCheckpoint::from_bytes(&cp.to_bytes()).unwrap();
        let recovered = session(Some(cp))
            .write_recoverable_to_file(
                &path,
                RecoveryOptions::new(Arc::new(|_| Ok(()))).with_output_format(format),
            )
            .await
            .unwrap_or_else(|e| panic!("case {index} stop {stop}: {e:?}"));
        assert_eq!(recovered.media().outputs().len(), reference.len());
        for (n, expected) in reference.iter().enumerate() {
            assert_eq!(
                &sample::canonical(std::fs::read(child(&path, n)).unwrap()),
                expected,
                "case {index} stop {stop} part {n}"
            );
        }
    }
    std::fs::write(
        root.join("evidence.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "case": index, "outputs":reference.len(), "checkpointWindows":total,
            "classic":format != OutputFormat::FragmentedMp4, "allRecoveredEqual":true
        }))
        .unwrap(),
    )
    .unwrap();
}
