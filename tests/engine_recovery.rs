#![cfg(not(target_arch = "wasm32"))]
use hls_engine::{playlist::*, *};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
#[allow(dead_code)]
#[path = "support/sample_crypto.rs"]
mod sample;

struct Wait;
impl EngineWait for Wait {
    fn wait(
        &self,
        d: std::time::Duration,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(tokio::time::sleep(d))
    }
}
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "engine-recovery-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn output(&self) -> PathBuf {
        self.0.join("output.mp4")
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn partial(path: &Path) -> PathBuf {
    let mut p = path.as_os_str().to_os_string();
    p.push(".hls-partial");
    p.into()
}
fn id(s: &str) -> InputId {
    InputId::new(s).unwrap()
}
fn create_using(
    provider: Arc<dyn hls_engine::crypto::key::KeyProvider>,
    encrypted: bool,
    cp: Option<EngineCheckpoint>,
    feed: bool,
    subtitles: bool,
) -> EngineSession {
    let cases = sample::cases();
    let video = cases
        .iter()
        .find(|c| {
            c.name
                == if encrypted {
                    "fmp4_avc_cenc"
                } else {
                    "fmp4_avc_clear"
                }
        })
        .unwrap();
    let audio = cases
        .iter()
        .find(|c| {
            c.name
                == if encrypted {
                    "fmp4_aac_cbcs"
                } else {
                    "fmp4_aac_clear"
                }
        })
        .unwrap();
    let source = |case: &sample::Case| {
        let mut s = MemorySource::new();
        for (name, bytes) in case.files {
            s = s.segment(format!("https://fixture.test/{name}"), *bytes);
        }
        Arc::new(s) as Arc<dyn Source>
    };
    let mut inputs = EngineInputs::new(
        EngineInput::new(id("v"), source(video)),
        EmbeddedAudio::Exclude,
    )
    .with_audio(
        EngineInput::new(id("a"), source(audio)),
        TrackMetadata::new("en", "English"),
    )
    .with_audio(
        EngineInput::new(id("b"), source(audio)),
        TrackMetadata::new("ja", "Japanese"),
    );
    if subtitles {
        inputs = inputs.with_subtitle(SubtitleTrack::new(
            id("s"),
            id("v"),
            TrackMetadata::new("en", "Text"),
        ));
    }
    let options =
        EngineOptions::default().with_waiter(Arc::new(Wait), std::time::Duration::from_secs(1));
    let keys = sample::keys(provider);
    let s = match cp {
        Some(cp) => EngineSession::restore(inputs, keys, options, cp),
        None => EngineSession::new(inputs, keys, options),
    }
    .unwrap();
    if feed {
        for (input, case) in [("v", video), ("a", audio), ("b", audio)] {
            let snap = parse_playlist_snapshot(
                &TextResource {
                    location: SourceLocation::Url(
                        url::Url::parse("https://fixture.test/input").unwrap(),
                    ),
                    content: case.playlist.into(),
                },
                PlaylistContext::new(id(input), 0),
            )
            .unwrap();
            s.handle().accept_snapshot(&id(input), &snap).unwrap();
        }
        if subtitles {
            let h = s.handle();
            let track = h.subtitle_track_id(&id("s")).unwrap();
            h.accept_cues(
                track,
                &[
                    SubtitleCue::new(
                        0,
                        0,
                        MediaTime::new(0, 1).unwrap(),
                        MediaTime::new(4, 1).unwrap(),
                        "SECRET-CUE-PAYLOAD",
                    ),
                    SubtitleCue::new(
                        0,
                        0,
                        MediaTime::new(2, 1).unwrap(),
                        MediaTime::new(7, 1).unwrap(),
                        "TAIL-CUE",
                    ),
                ],
            )
            .unwrap();
            h.end_subtitles(track).unwrap();
        }
    }
    s
}
fn create_profile(
    encrypted: bool,
    cp: Option<EngineCheckpoint>,
    feed: bool,
    subtitles: bool,
) -> EngineSession {
    create_using(Arc::new(sample::Provider), encrypted, cp, feed, subtitles)
}
fn create(cp: Option<EngineCheckpoint>, feed: bool, subtitles: bool) -> EngineSession {
    create_profile(false, cp, feed, subtitles)
}
#[tokio::test]
async fn sample_encrypted_tracks_resume() {
    let temp = Temp::new();
    let path = temp.output();
    let (expected, _) = create_profile(true, None, true, true)
        .into_bytes(8 * 1024 * 1024, OutputFormat::FragmentedMp4)
        .await
        .unwrap();
    let (options, latest) = capture(true);
    assert!(
        create_profile(true, None, true, true)
            .write_recoverable_to_file(&path, options)
            .await
            .is_err()
    );
    let cp = latest.lock().unwrap().clone().unwrap();
    let (options, _) = capture(false);
    create_profile(true, Some(cp), true, true)
        .write_recoverable_to_file(&path, options)
        .await
        .unwrap();
    assert_eq!(
        sample::canonical(std::fs::read(path).unwrap()),
        sample::canonical(expected)
    );
}
fn capture(stop: bool) -> (RecoveryOptions, Arc<Mutex<Option<EngineCheckpoint>>>) {
    let latest = Arc::new(Mutex::new(None));
    let copy = latest.clone();
    let options = RecoveryOptions::new(Arc::new(move |cp| {
        if cp.bytes_written() == 0 {
            return Ok(());
        }
        *copy.lock().unwrap() = Some(cp);
        if stop {
            Err(EngineError::output(
                std::io::Error::other("injected checkpoint interruption").into(),
            ))
        } else {
            Ok(())
        }
    }));
    (options, latest)
}
#[tokio::test]
async fn multiple_tracks_and_subtitles_resume_without_loss_or_duplication() {
    for subtitles in [false, true] {
        let temp = Temp::new();
        let path = temp.output();
        let (expected, _) = create(None, true, subtitles)
            .into_bytes(8 * 1024 * 1024, OutputFormat::FragmentedMp4)
            .await
            .unwrap();
        let (options, latest) = capture(true);
        assert!(
            create(None, true, subtitles)
                .write_recoverable_to_file(&path, options)
                .await
                .is_err()
        );
        let cp = latest.lock().unwrap().clone().unwrap();
        let wire = cp.to_bytes();
        assert!(
            !wire
                .windows(b"SECRET-CUE-PAYLOAD".len())
                .any(|b| b == b"SECRET-CUE-PAYLOAD")
        );
        assert!(!wire.windows(b"https://".len()).any(|b| b == b"https://"));
        let cp = EngineCheckpoint::from_bytes(&wire).unwrap();
        #[cfg(feature = "serde")]
        let cp =
            serde_json::from_str::<EngineCheckpoint>(&serde_json::to_string(&cp).unwrap()).unwrap();
        let committed = cp.bytes_written();
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(partial(&path))
            .unwrap()
            .write_all(b"incomplete fragment tail")
            .unwrap();
        let (options, latest) = capture(false);
        let report = create(Some(cp), true, subtitles)
            .write_recoverable_to_file(&path, options)
            .await
            .unwrap();
        assert!(report.media().bytes_written() > committed);
        assert!(latest.lock().unwrap().as_ref().unwrap().is_completed());
        assert_eq!(
            sample::canonical(std::fs::read(&path).unwrap()),
            sample::canonical(expected)
        );
    }
}
#[tokio::test]
async fn corrupted_prefix_is_not_truncated_or_overwritten() {
    let temp = Temp::new();
    let path = temp.output();
    let (options, latest) = capture(true);
    assert!(
        create(None, true, false)
            .write_recoverable_to_file(&path, options)
            .await
            .is_err()
    );
    let cp = latest.lock().unwrap().clone().unwrap();
    let partial = partial(&path);
    let mut bytes = std::fs::read(&partial).unwrap();
    let n = bytes.len();
    bytes[n - 1] ^= 1;
    bytes.extend_from_slice(b"tail must remain");
    std::fs::write(&partial, &bytes).unwrap();
    let (options, _) = capture(false);
    let error = create(Some(cp), true, false)
        .write_recoverable_to_file(&path, options)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), EngineErrorKind::ResumeCorruption);
    assert_eq!(std::fs::read(partial).unwrap(), bytes);
    assert!(!path.exists());
}
#[tokio::test]
async fn native_classic_resumes_and_finalizes() {
    let temp = Temp::new();
    let path = temp.output();
    let (expected, _) = create(None, true, true)
        .into_bytes(8 * 1024 * 1024, OutputFormat::Mp4)
        .await
        .unwrap();
    let (options, latest) = capture(true);
    assert!(
        create(None, true, true)
            .write_recoverable_to_file(
                &path,
                options.with_output_format(OutputFormat::StreamingMp4)
            )
            .await
            .is_err()
    );
    let cp = latest.lock().unwrap().clone().unwrap();
    let (options, latest) = capture(false);
    create(Some(cp), true, true)
        .write_recoverable_to_file(
            &path,
            options.with_output_format(OutputFormat::StreamingMp4),
        )
        .await
        .unwrap();
    let cp = latest.lock().unwrap().clone().unwrap();
    assert!(cp.is_completed());
    assert_eq!(
        sample::canonical(std::fs::read(&path).unwrap()),
        sample::canonical(expected)
    );
    // Once Completed is durable, callers may clean all intermediate artifacts.
    for entry in std::fs::read_dir(&temp.0).unwrap() {
        let entry = entry.unwrap().path();
        if entry != path {
            std::fs::remove_file(entry).unwrap();
        }
    }
    let (options, _) = capture(false);
    create(Some(cp), false, true)
        .write_recoverable_to_file(
            &path,
            options.with_output_format(OutputFormat::StreamingMp4),
        )
        .await
        .unwrap();
}
#[tokio::test]
async fn finalizing_checkpoint_needs_no_input_replay() {
    let temp = Temp::new();
    let path = temp.output();
    let latest = Arc::new(Mutex::new(None));
    let copy = latest.clone();
    let options = RecoveryOptions::new(Arc::new(move |cp| {
        let stop = cp.is_finalizing();
        *copy.lock().unwrap() = Some(cp);
        if stop {
            Err(EngineError::output(
                std::io::Error::other("before publication").into(),
            ))
        } else {
            Ok(())
        }
    }))
    .with_durability(CheckpointDurability::SyncAll);
    assert!(
        create(None, true, true)
            .write_recoverable_to_file(&path, options)
            .await
            .is_err()
    );
    let cp = latest.lock().unwrap().clone().unwrap();
    assert!(cp.is_finalizing());
    assert!(!path.exists());
    let (options, _) = capture(false);
    create(Some(cp.clone()), false, true)
        .write_recoverable_to_file(&path, options)
        .await
        .unwrap();
    let before = std::fs::read(&path).unwrap();
    // Repeating an older finalizing checkpoint after publication is idempotent.
    let (options, _) = capture(false);
    create(Some(cp), false, true)
        .write_recoverable_to_file(&path, options)
        .await
        .unwrap();
    assert_eq!(std::fs::read(path).unwrap(), before);
}

struct ChangedVersion;
impl hls_engine::crypto::key::KeyProvider for ChangedVersion {
    fn resolve(
        &self,
        request: hls_engine::crypto::key::KeyRequest,
    ) -> hls_engine::crypto::key::KeyFuture<hls_engine::crypto::key::KeyResolution> {
        use hls_engine::crypto::key::KeyResolution;
        let result = sample::Provider.resolve(request);
        Box::pin(async move {
            match result.await {
                KeyResolution::Available(key) => {
                    KeyResolution::Available(key.with_version("changed-version"))
                }
                other => other,
            }
        })
    }
}
#[tokio::test]
async fn changed_key_version_conflicts_before_modifying_prefix() {
    let temp = Temp::new();
    let path = temp.output();
    let (options, latest) = capture(true);
    assert!(
        create_profile(true, None, true, false)
            .write_recoverable_to_file(&path, options)
            .await
            .is_err()
    );
    let cp = latest.lock().unwrap().clone().unwrap();
    let before = std::fs::read(partial(&path)).unwrap();
    let (options, _) = capture(false);
    let error = create_using(Arc::new(ChangedVersion), true, Some(cp), true, false)
        .write_recoverable_to_file(&path, options)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), EngineErrorKind::ResumeConflict);
    assert_eq!(std::fs::read(partial(&path)).unwrap(), before);
    assert!(!path.exists());
}
#[test]
#[ignore = "child process invoked by process_exit_recovers_durable_checkpoint"]
fn recovery_child() {
    let path = PathBuf::from(std::env::var_os("HLS_ENGINE_CRASH_OUTPUT").unwrap());
    let checkpoint = path.with_extension("checkpoint");
    let options = RecoveryOptions::new(Arc::new(move |cp| {
        use std::io::Write;
        if cp.bytes_written() == 0 {
            return Ok(());
        }
        let temporary = checkpoint.with_extension("checkpoint.tmp");
        let mut f = std::fs::File::create(&temporary).unwrap();
        f.write_all(&cp.to_bytes()).unwrap();
        f.sync_all().unwrap();
        drop(f);
        std::fs::rename(temporary, &checkpoint).unwrap();
        std::process::exit(71);
    }))
    .with_durability(CheckpointDurability::SyncAll);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            create(None, true, true)
                .write_recoverable_to_file(path, options)
                .await
                .unwrap();
        });
}
#[tokio::test]
async fn process_exit_recovers_durable_checkpoint() {
    let temp = Temp::new();
    let path = temp.output();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "recovery_child", "--ignored"])
        .env("HLS_ENGINE_CRASH_OUTPUT", &path)
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(71));
    let cp =
        EngineCheckpoint::from_bytes(&std::fs::read(path.with_extension("checkpoint")).unwrap())
            .unwrap();
    let (options, _) = capture(false);
    create(Some(cp), true, true)
        .write_recoverable_to_file(&path, options)
        .await
        .unwrap();
    let (expected, _) = create(None, true, true)
        .into_bytes(8 * 1024 * 1024, OutputFormat::FragmentedMp4)
        .await
        .unwrap();
    assert_eq!(
        sample::canonical(std::fs::read(path).unwrap()),
        sample::canonical(expected)
    );
}

#[tokio::test]
async fn completed_checkpoint_failure_reports_publication_without_false_completion() {
    let temp = Temp::new();
    let path = temp.output();
    let session = create(None, true, false);
    let handle = session.handle();
    let observing = handle.clone();
    let checkpoint = Arc::new(Mutex::new(None));
    let saved = checkpoint.clone();
    let options = RecoveryOptions::new(Arc::new(move |cp| {
        if cp.is_completed() {
            assert_eq!(observing.state(), EngineState::Finalizing);
            // Cancellation loses the publication race, even during persistence.
            observing.cancel();
            return Err(EngineError::output(
                std::io::Error::other("persistence failed").into(),
            ));
        }
        *saved.lock().unwrap() = Some(cp);
        Ok(())
    }));
    let error = session
        .write_recoverable_to_file(&path, options)
        .await
        .unwrap_err();
    assert_eq!(handle.state(), EngineState::Failed);
    assert_eq!(error.kind(), EngineErrorKind::Output);
    assert_eq!(error.completed_outputs().len(), 1);
    assert!(path.exists());
    let before = std::fs::read(&path).unwrap();
    let cp = checkpoint.lock().unwrap().clone().unwrap();
    let (options, _) = capture(false);
    create(Some(cp), false, false)
        .write_recoverable_to_file(&path, options)
        .await
        .unwrap();
    assert_eq!(before, std::fs::read(&path).unwrap());
}

#[tokio::test]
async fn malformed_checkpoint_is_rejected_before_tail_truncation() {
    use sha2::{Digest, Sha256};
    let temp = Temp::new();
    let path = temp.output();
    let (options, latest) = capture(true);
    create(None, true, false)
        .write_recoverable_to_file(&path, options)
        .await
        .unwrap_err();
    let cp = latest.lock().unwrap().clone().unwrap();
    let wire = cp.to_bytes();
    for n in 0..wire.len() {
        let mut changed = wire.clone();
        changed[n] ^= 1;
        assert!(EngineCheckpoint::from_bytes(&changed).is_err());
    }
    for size in [0, 1, 8, 100, wire.len() - 1] {
        assert!(EngineCheckpoint::from_bytes(&wire[..size]).is_err());
    }
    // Recompute the envelope checksum: state must still agree with its header.
    let mut changed = wire;
    let offset = 8 + 32 + 32; // committed bytes must agree with the saved engine
    changed[offset..offset + 8].copy_from_slice(&(cp.bytes_written() + 1).to_be_bytes());
    let end = changed.len() - 32;
    let digest = Sha256::digest(&changed[..end]);
    changed[end..].copy_from_slice(&digest);
    let cp = EngineCheckpoint::from_bytes(&changed).unwrap();
    let mut before = std::fs::read(partial(&path)).unwrap();
    before.extend_from_slice(b"uncommitted tail retained on failed validation");
    std::fs::write(partial(&path), &before).unwrap();
    let (options, _) = capture(false);
    let error = create(Some(cp), true, false)
        .write_recoverable_to_file(&path, options)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), EngineErrorKind::ResumeCorruption);
    assert_eq!(before, std::fs::read(partial(&path)).unwrap());
    assert!(!path.exists());
}

#[cfg(feature = "experimental-gcm")]
#[allow(dead_code)]
#[path = "support/engine_gcm.rs"]
mod gcm;
#[cfg(feature = "experimental-gcm")]
#[tokio::test]
async fn gcm_map_rotation_range_and_open_input_resume() {
    for open in [false, true] {
        for ranged in [false, true] {
            for format in [OutputFormat::FragmentedMp4, OutputFormat::StreamingMp4] {
                let options = || {
                    let mut options = EngineOptions::default().with_mode(if open {
                        EngineMode::Open
                    } else {
                        EngineMode::Vod
                    });
                    if ranged {
                        options = options.with_range(
                            PresentationRange::new(
                                MediaTime::new(1, 1).unwrap(),
                                MediaTime::new(5, 1).unwrap(),
                            )
                            .unwrap(),
                        );
                    }
                    options
                };
                let feed = |session: &EngineSession| {
                    session
                        .handle()
                        .accept_snapshot(&gcm::input_id(), &gcm::snapshot(true, !open))
                        .unwrap();
                    session.handle().end_input(&gcm::input_id()).unwrap();
                };
                let expected = gcm::rotation_session(true, options(), None);
                feed(&expected);
                let memory_format = if format == OutputFormat::FragmentedMp4 {
                    format
                } else {
                    OutputFormat::Mp4
                };
                let (expected, _) = expected
                    .into_bytes(8 * 1024 * 1024, memory_format)
                    .await
                    .unwrap();
                let temp = Temp::new();
                let path = temp.output();
                let session = gcm::rotation_session(true, options(), None);
                feed(&session);
                let (recovery, latest) = capture(true);
                session
                    .write_recoverable_to_file(&path, recovery.with_output_format(format))
                    .await
                    .unwrap_err();
                let cp = latest.lock().unwrap().clone().unwrap();
                let session = gcm::rotation_session(true, options(), Some(cp));
                feed(&session);
                let (recovery, _) = capture(false);
                session
                    .write_recoverable_to_file(&path, recovery.with_output_format(format))
                    .await
                    .unwrap();
                assert_eq!(
                    sample::canonical(expected),
                    sample::canonical(std::fs::read(path).unwrap())
                );
            }
        }
    }
}

#[cfg(feature = "experimental-gcm")]
#[tokio::test]
async fn rolling_live_manifest_recovers_without_whole_manifest_identity() {
    for encrypted in [false, true] {
        let options = || EngineOptions::default().with_mode(EngineMode::Open);
        let expected = gcm::rotation_session(encrypted, options(), None);
        expected
            .handle()
            .accept_snapshot(&gcm::input_id(), &gcm::snapshot(encrypted, true))
            .unwrap();
        let (expected, _) = expected
            .into_bytes(8 * 1024 * 1024, OutputFormat::FragmentedMp4)
            .await
            .unwrap();
        let temp = Temp::new();
        let path = temp.output();
        let session = gcm::rotation_session(encrypted, options(), None);
        session
            .handle()
            .accept_snapshot(
                &gcm::input_id(),
                &gcm::snapshot_window(encrypted, 0, 2, false),
            )
            .unwrap();
        let (recovery, latest) = capture(true);
        session
            .write_recoverable_to_file(&path, recovery)
            .await
            .unwrap_err();
        let cp = latest.lock().unwrap().clone().unwrap();
        let before = std::fs::read(partial(&path)).unwrap();
        // Required lookahead has disappeared: fail without changing the prefix.
        let session = gcm::rotation_session(encrypted, options(), Some(cp.clone()));
        session
            .handle()
            .accept_snapshot(
                &gcm::input_id(),
                &gcm::snapshot_window(encrypted, 2, 1, true),
            )
            .unwrap();
        let (recovery, _) = capture(false);
        assert_eq!(
            session
                .write_recoverable_to_file(&path, recovery)
                .await
                .unwrap_err()
                .kind(),
            EngineErrorKind::ReplayRequired
        );
        assert_eq!(std::fs::read(partial(&path)).unwrap(), before);
        // The committed segment is evicted, lookahead remains, and a new segment arrives.
        let session = gcm::rotation_session(encrypted, options(), Some(cp));
        session
            .handle()
            .accept_snapshot(
                &gcm::input_id(),
                &gcm::snapshot_window(encrypted, 1, 2, true),
            )
            .unwrap();
        let (recovery, _) = capture(false);
        let report = session
            .write_recoverable_to_file(&path, recovery)
            .await
            .unwrap();
        assert_eq!(report.media().inputs()[0].accepted(), 3);
        assert_eq!(report.media().inputs()[0].committed(), 3);
        assert_eq!(
            sample::canonical(expected),
            sample::canonical(std::fs::read(path).unwrap())
        );
    }
}

#[tokio::test]
async fn missing_subtitle_replay_preserves_uncommitted_tail() {
    let temp = Temp::new();
    let path = temp.output();
    let (options, latest) = capture(true);
    create(None, true, true)
        .write_recoverable_to_file(&path, options)
        .await
        .unwrap_err();
    let cp = latest.lock().unwrap().clone().unwrap();
    let mut before = std::fs::read(partial(&path)).unwrap();
    before.extend_from_slice(b"tail");
    std::fs::write(partial(&path), &before).unwrap();
    let session = create(Some(cp), false, true);
    let cases = sample::cases();
    for (input, name) in [
        ("v", "fmp4_avc_clear"),
        ("a", "fmp4_aac_clear"),
        ("b", "fmp4_aac_clear"),
    ] {
        let case = cases.iter().find(|c| c.name == name).unwrap();
        let snapshot = parse_playlist_snapshot(
            &TextResource {
                location: SourceLocation::Url(
                    url::Url::parse("https://fixture.test/input").unwrap(),
                ),
                content: case.playlist.into(),
            },
            PlaylistContext::new(id(input), 0),
        )
        .unwrap();
        session
            .handle()
            .accept_snapshot(&id(input), &snapshot)
            .unwrap();
    }
    let (options, _) = capture(false);
    assert_eq!(
        session
            .write_recoverable_to_file(&path, options)
            .await
            .unwrap_err()
            .kind(),
        EngineErrorKind::ReplayRequired
    );
    assert_eq!(before, std::fs::read(partial(&path)).unwrap());
    assert!(!path.exists());
}

#[tokio::test]
async fn short_committed_file_is_corruption_and_remains_untouched() {
    let temp = Temp::new();
    let path = temp.output();
    let (options, latest) = capture(true);
    create(None, true, false)
        .write_recoverable_to_file(&path, options)
        .await
        .unwrap_err();
    let cp = latest.lock().unwrap().clone().unwrap();
    let mut before = std::fs::read(partial(&path)).unwrap();
    before.truncate(before.len() - 1);
    std::fs::write(partial(&path), &before).unwrap();
    let (options, _) = capture(false);
    assert_eq!(
        create(Some(cp), true, false)
            .write_recoverable_to_file(&path, options)
            .await
            .unwrap_err()
            .kind(),
        EngineErrorKind::ResumeCorruption
    );
    assert_eq!(before, std::fs::read(partial(&path)).unwrap());
}

#[tokio::test]
async fn completed_fmp4_verifies_final_file_after_partial_cleanup() {
    let temp = Temp::new();
    let path = temp.output();
    let (options, latest) = capture(false);
    create(None, true, false)
        .write_recoverable_to_file(&path, options)
        .await
        .unwrap();
    let cp = latest.lock().unwrap().clone().unwrap();
    assert!(cp.is_completed());
    std::fs::remove_file(partial(&path)).unwrap();
    let (options, _) = capture(false);
    let report = create(Some(cp.clone()), false, false)
        .write_recoverable_to_file(&path, options)
        .await
        .unwrap();
    let mut before = std::fs::read(&path).unwrap();
    assert_eq!(report.media().bytes_written(), before.len() as u64);
    before[32] ^= 1;
    std::fs::write(&path, &before).unwrap();
    let (options, _) = capture(false);
    assert_eq!(
        create(Some(cp), false, false)
            .write_recoverable_to_file(&path, options)
            .await
            .unwrap_err()
            .kind(),
        EngineErrorKind::ResumeCorruption
    );
    assert_eq!(before, std::fs::read(&path).unwrap());
    assert!(!partial(&path).exists());
}

fn split_session(cp: Option<EngineCheckpoint>, feed: bool) -> EngineSession {
    let src = MemorySource::new()
        .segment(
            "https://multi.test/avc",
            include_bytes!("fixtures/sample_crypto/fmp4_avc_clear/seg1.m4s"),
        )
        .segment(
            "https://multi.test/hevc",
            include_bytes!("fixtures/sample_crypto/fmp4_hevc_clear/seg1.m4s"),
        )
        .segment(
            "https://multi.test/a.init",
            include_bytes!("fixtures/sample_crypto/fmp4_avc_clear/init.mp4"),
        )
        .segment(
            "https://multi.test/h.init",
            include_bytes!("fixtures/sample_crypto/fmp4_hevc_clear/init.mp4"),
        );
    let inputs = EngineInputs::new(
        EngineInput::new(id("main"), Arc::new(src)),
        EmbeddedAudio::Exclude,
    )
    .with_subtitle(SubtitleTrack::new(
        id("cc"),
        id("main"),
        TrackMetadata::new("en", "Captions"),
    ));
    let options = EngineOptions::default().with_change_policy(TimelineChangePolicy::Split);
    let s = match cp {
        Some(cp) => EngineSession::restore(
            inputs,
            sample::keys(Arc::new(sample::Provider)),
            options,
            cp,
        ),
        None => EngineSession::new(inputs, sample::keys(Arc::new(sample::Provider)), options),
    }
    .unwrap();
    if feed {
        let snapshot = parse_playlist_snapshot(&TextResource {
            location: SourceLocation::Url(url::Url::parse("https://multi.test/index.m3u8").unwrap()),
            content: "#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXT-X-MAP:URI=\"a.init\"\n#EXTINF:2,\navc\n#EXT-X-DISCONTINUITY\n#EXT-X-MAP:URI=\"h.init\"\n#EXTINF:2,\nhevc\n#EXT-X-ENDLIST\n".into(),
        }, PlaylistContext::new(id("main"), 0)).unwrap();
        let h = s.handle();
        h.accept_snapshot(&id("main"), &snapshot).unwrap();
        let track = h.subtitle_track_id(&id("cc")).unwrap();
        h.accept_cues(
            track,
            &[SubtitleCue::new(
                0,
                0,
                MediaTime::new(0, 1).unwrap(),
                MediaTime::new(5, 1).unwrap(),
                "split replay",
            )],
        )
        .unwrap();
        h.end_subtitles(track).unwrap();
    }
    s
}
fn child(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".part-000001.mp4");
    name.into()
}

#[tokio::test]
async fn split_recovers_every_checkpoint_and_publication_window() {
    for format in [OutputFormat::FragmentedMp4, OutputFormat::StreamingMp4] {
        let complete = Temp::new();
        let count = Arc::new(AtomicUsize::new(0));
        let copy = count.clone();
        let report = split_session(None, true)
            .write_recoverable_to_file(
                complete.output(),
                RecoveryOptions::new(Arc::new(move |_| {
                    copy.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                }))
                .with_output_format(format),
            )
            .await
            .unwrap();
        assert_eq!(report.media().outputs().len(), 2);
        let expected = [complete.output(), child(&complete.output())]
            .map(|p| sample::canonical(std::fs::read(p).unwrap()));
        for stop in 1..=count.load(Ordering::Relaxed) {
            let temp = Temp::new();
            let latest = Arc::new(Mutex::new(None));
            let copy = latest.clone();
            let calls = AtomicUsize::new(0);
            let result = split_session(None, true)
                .write_recoverable_to_file(
                    temp.output(),
                    RecoveryOptions::new(Arc::new(move |cp| {
                        *copy.lock().unwrap() = Some(cp);
                        if calls.fetch_add(1, Ordering::Relaxed) + 1 == stop {
                            return Err(EngineError::output(
                                std::io::Error::other("split crash").into(),
                            ));
                        }
                        Ok(())
                    }))
                    .with_output_format(format),
                )
                .await;
            assert!(result.is_err(), "stop {stop}");
            let cp = latest.lock().unwrap().clone().unwrap();
            let cp = EngineCheckpoint::from_bytes(&cp.to_bytes()).unwrap();
            let terminal = cp.is_finalizing();
            split_session(Some(cp), !terminal)
                .write_recoverable_to_file(
                    temp.output(),
                    RecoveryOptions::new(Arc::new(|_| Ok(()))).with_output_format(format),
                )
                .await
                .unwrap_or_else(|e| panic!("{format:?} stop {stop}: {e:?}"));
            for (i, p) in [temp.output(), child(&temp.output())]
                .into_iter()
                .enumerate()
            {
                assert_eq!(
                    sample::canonical(std::fs::read(p).unwrap()),
                    expected[i],
                    "{format:?} stop {stop} part {i}"
                );
            }
        }
    }
}

#[cfg(feature = "experimental-gcm")]
#[tokio::test]
async fn live_eviction_reports_gap_and_resumes_at_independent_boundary() {
    for encrypted in [false, true] {
        for policy in [MissingSegmentPolicy::Skip, MissingSegmentPolicy::Split] {
            let options = || {
                EngineOptions::default()
                    .with_mode(EngineMode::Open)
                    .with_missing_segments(policy)
            };
            let temp = Temp::new();
            let session = gcm::rotation_session(encrypted, options(), None);
            session
                .handle()
                .accept_snapshot(
                    &gcm::input_id(),
                    &gcm::snapshot_window(encrypted, 0, 2, false),
                )
                .unwrap();
            let (recovery, latest) = capture(true);
            session
                .write_recoverable_to_file(temp.output(), recovery)
                .await
                .unwrap_err();
            let cp = latest.lock().unwrap().clone().unwrap();
            let session = gcm::rotation_session(encrypted, options(), Some(cp));
            session
                .handle()
                .accept_snapshot(
                    &gcm::input_id(),
                    &gcm::snapshot_window(encrypted, 2, 1, true),
                )
                .unwrap();
            let (recovery, _) = capture(false);
            let report = session
                .write_recoverable_to_file(temp.output(), recovery)
                .await
                .unwrap();
            assert_eq!(report.media().gap_count(), 1);
            assert_eq!(report.media().inputs()[0].committed(), 3);
            assert_eq!(
                report.media().outputs().len(),
                if policy == MissingSegmentPolicy::Split {
                    2
                } else {
                    1
                }
            );
        }
    }
}

#[test]
#[ignore = "child process invoked by split_process_crash_windows"]
fn split_recovery_child() {
    let path = PathBuf::from(std::env::var_os("HLS_ENGINE_CRASH_OUTPUT").unwrap());
    let stop: usize = std::env::var("HLS_ENGINE_SPLIT_STOP")
        .unwrap()
        .parse()
        .unwrap();
    let before = std::env::var_os("HLS_ENGINE_BEFORE_PERSIST").is_some();
    let classic = std::env::var_os("HLS_ENGINE_CLASSIC").is_some();
    let checkpoint = path.with_extension("checkpoint");
    let calls = AtomicUsize::new(0);
    let options = RecoveryOptions::new(Arc::new(move |cp| {
        use std::io::Write;
        let n = calls.fetch_add(1, Ordering::Relaxed) + 1;
        if before && n == stop {
            std::process::exit(72);
        }
        let temporary = checkpoint.with_extension("checkpoint.tmp");
        let mut file = std::fs::File::create(&temporary).unwrap();
        file.write_all(&cp.to_bytes()).unwrap();
        file.sync_all().unwrap();
        drop(file);
        // The child only tests process loss, not directory durability on power loss.
        #[cfg(target_os = "windows")]
        if checkpoint.exists() {
            std::fs::remove_file(&checkpoint).unwrap();
        }
        std::fs::rename(temporary, &checkpoint).unwrap();
        if n == stop {
            std::process::exit(72);
        }
        Ok(())
    }))
    .with_durability(CheckpointDurability::SyncAll)
    .with_output_format(if classic {
        OutputFormat::StreamingMp4
    } else {
        OutputFormat::FragmentedMp4
    });
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(split_session(None, true).write_recoverable_to_file(path, options))
        .unwrap();
}

#[tokio::test]
async fn split_process_crash_windows() {
    for format in [OutputFormat::FragmentedMp4, OutputFormat::StreamingMp4] {
        let expected = Temp::new();
        let count = Arc::new(AtomicUsize::new(0));
        let copy = count.clone();
        split_session(None, true)
            .write_recoverable_to_file(
                expected.output(),
                RecoveryOptions::new(Arc::new(move |_| {
                    copy.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                }))
                .with_output_format(format),
            )
            .await
            .unwrap();
        let parts = [expected.output(), child(&expected.output())]
            .map(|p| sample::canonical(std::fs::read(p).unwrap()));
        for before in [false, true] {
            for stop in (if before { 2 } else { 1 })..=count.load(Ordering::Relaxed) {
                let temp = Temp::new();
                let mut command = std::process::Command::new(std::env::current_exe().unwrap());
                command
                    .args(["--exact", "split_recovery_child", "--ignored"])
                    .env("HLS_ENGINE_CRASH_OUTPUT", temp.output())
                    .env("HLS_ENGINE_SPLIT_STOP", stop.to_string());
                if before {
                    command.env("HLS_ENGINE_BEFORE_PERSIST", "1");
                }
                if format == OutputFormat::StreamingMp4 {
                    command.env("HLS_ENGINE_CLASSIC", "1");
                }
                let output = command.output().unwrap();
                assert_eq!(
                    output.status.code(),
                    Some(72),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let cp = EngineCheckpoint::from_bytes(
                    &std::fs::read(temp.output().with_extension("checkpoint")).unwrap(),
                )
                .unwrap();
                let terminal = cp.is_finalizing();
                split_session(Some(cp), !terminal)
                    .write_recoverable_to_file(
                        temp.output(),
                        RecoveryOptions::new(Arc::new(|_| Ok(()))).with_output_format(format),
                    )
                    .await
                    .unwrap_or_else(|e| panic!("{format:?} stop={stop} before={before}: {e:?}"));
                for (i, path) in [temp.output(), child(&temp.output())]
                    .into_iter()
                    .enumerate()
                {
                    assert_eq!(
                        sample::canonical(std::fs::read(path).unwrap()),
                        parts[i],
                        "stop={stop} before={before} part={i}"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn split_ledger_corruption_preserves_active_partial() {
    let temp = Temp::new();
    let latest = Arc::new(Mutex::new(None));
    let copy = latest.clone();
    split_session(None, true)
        .write_recoverable_to_file(
            temp.output(),
            RecoveryOptions::new(Arc::new(move |cp| {
                let stop = cp.output_index() == 1 && cp.bytes_written() == 0;
                *copy.lock().unwrap() = Some(cp);
                if stop {
                    return Err(EngineError::output(
                        std::io::Error::other("lease interruption").into(),
                    ));
                }
                Ok(())
            })),
        )
        .await
        .unwrap_err();
    let cp = latest.lock().unwrap().clone().unwrap();
    assert_eq!(cp.completed_outputs(), 1);
    let tail = b"partial header left by a process crash";
    std::fs::write(partial(&child(&temp.output())), tail).unwrap();
    let old = std::fs::read(temp.output()).unwrap();
    std::fs::write(temp.output(), b"corrupted published child").unwrap();
    let error = split_session(Some(cp.clone()), true)
        .write_recoverable_to_file(temp.output(), RecoveryOptions::new(Arc::new(|_| Ok(()))))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), EngineErrorKind::ResumeCorruption);
    assert_eq!(
        std::fs::read(partial(&child(&temp.output()))).unwrap(),
        tail
    );
    std::fs::write(temp.output(), &old).unwrap();
    split_session(Some(cp), true)
        .write_recoverable_to_file(temp.output(), RecoveryOptions::new(Arc::new(|_| Ok(()))))
        .await
        .unwrap();
    assert_eq!(std::fs::read(temp.output()).unwrap(), old);
}

#[test]
fn native_recovery_capabilities_require_files_and_persistence() {
    use hls_engine::capabilities::*;
    for output in [
        KeyedOutput::FragmentedFile,
        KeyedOutput::Mp4File,
        KeyedOutput::NativeStreamingFile,
        KeyedOutput::FragmentedWriter,
        KeyedOutput::Mp4Bytes,
        KeyedOutput::FfmpegStreamingFile,
    ] {
        let query = CapabilityQuery::new(
            id("primary"),
            KeyedInputCapability::new(
                KeyedContainer::FragmentedMp4,
                KeyedEncryption::Aes128,
                vec![KeyedCodec::Avc],
            ),
            output,
            EmbeddedAudio::Exclude,
        )
        .with_resume(true)
        .with_memory_capacity(1024);
        let decision = query_capability(&query);
        let native = matches!(
            output,
            KeyedOutput::FragmentedFile | KeyedOutput::Mp4File | KeyedOutput::NativeStreamingFile
        );
        assert_eq!(decision.container_supported(), native, "{output:?}");
        if native {
            assert!(
                decision
                    .requirements()
                    .contains(&CapabilityRequirement::NativeCheckpointPersistence)
            );
            assert!(
                decision
                    .requirements()
                    .contains(&CapabilityRequirement::ReplayIdentityValidation)
            );
            assert!(
                decision
                    .requirements()
                    .contains(&CapabilityRequirement::StableKeyVersion)
            );
        }
    }
}

#[cfg(feature = "experimental-gcm")]
#[tokio::test]
async fn gcm_multitrack_recovery_covers_each_checkpoint() {
    for open in [false, true] {
        for ranged in [false, true] {
            for format in [OutputFormat::FragmentedMp4, OutputFormat::StreamingMp4] {
                let create = |cp: Option<EngineCheckpoint>| {
                    let terminal = cp.as_ref().is_some_and(|c| c.is_finalizing());
                    let mut options = EngineOptions::default().with_mode(if open {
                        EngineMode::Open
                    } else {
                        EngineMode::Vod
                    });
                    if ranged {
                        options = options.with_range(
                            PresentationRange::new(
                                MediaTime::new(1, 1).unwrap(),
                                MediaTime::new(5, 1).unwrap(),
                            )
                            .unwrap(),
                        );
                    }
                    let s = gcm::rotation_multitrack(true, options, cp);
                    if !terminal {
                        s.handle()
                            .accept_snapshot(&gcm::input_id(), &gcm::snapshot(true, !open))
                            .unwrap();
                        s.handle().end_input(&gcm::input_id()).unwrap();
                        gcm::feed_additional_tracks(&s, true, open);
                    }
                    s
                };
                let complete = Temp::new();
                let calls = Arc::new(AtomicUsize::new(0));
                let copy = calls.clone();
                create(None)
                    .write_recoverable_to_file(
                        complete.output(),
                        RecoveryOptions::new(Arc::new(move |_| {
                            copy.fetch_add(1, Ordering::Relaxed);
                            Ok(())
                        }))
                        .with_output_format(format),
                    )
                    .await
                    .unwrap();
                let expected = sample::canonical(std::fs::read(complete.output()).unwrap());
                for stop in 1..=calls.load(Ordering::Relaxed) {
                    let temp = Temp::new();
                    let saved = Arc::new(Mutex::new(None));
                    let copy = saved.clone();
                    let seen = AtomicUsize::new(0);
                    create(None)
                        .write_recoverable_to_file(
                            temp.output(),
                            RecoveryOptions::new(Arc::new(move |cp| {
                                *copy.lock().unwrap() = Some(cp);
                                if seen.fetch_add(1, Ordering::Relaxed) + 1 == stop {
                                    return Err(EngineError::output(
                                        std::io::Error::other("GCM recovery interruption").into(),
                                    ));
                                }
                                Ok(())
                            }))
                            .with_output_format(format),
                        )
                        .await
                        .unwrap_err();
                    let cp = saved.lock().unwrap().clone().unwrap();
                    create(Some(cp)).write_recoverable_to_file(temp.output(), RecoveryOptions::new(Arc::new(|_| Ok(()))).with_output_format(format)).await
                        .unwrap_or_else(|e| panic!("GCM open={open} ranged={ranged} format={format:?} stop={stop}: {e:?}"));
                    assert_eq!(
                        sample::canonical(std::fs::read(temp.output()).unwrap()),
                        expected
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn evicted_ts_lookahead_preserves_gap_timing_and_collapse() {
    for policy in [MissingSegmentPolicy::Skip, MissingSegmentPolicy::Split] {
        for gaps in [GapPolicy::Preserve, GapPolicy::Collapse] {
            let make = |cp: Option<EngineCheckpoint>, start: u64, explicit_gap: bool| {
                let source = MemorySource::new()
                    .segment(
                        "https://live.test/s0",
                        include_bytes!("fixtures/media/ts_avc_regular/seg0.ts"),
                    )
                    .segment(
                        "https://live.test/s1",
                        include_bytes!("fixtures/media/ts_avc_regular/seg1.ts"),
                    )
                    .segment(
                        "https://live.test/s2",
                        include_bytes!("fixtures/media/ts_avc_regular/seg2.ts"),
                    );
                let inputs = EngineInputs::new(
                    EngineInput::new(id("v"), Arc::new(source)),
                    EmbeddedAudio::Keep,
                );
                let options = EngineOptions::default()
                    .with_mode(EngineMode::Open)
                    .with_missing_segments(policy)
                    .with_gap_policy(gaps);
                let keys = sample::keys(Arc::new(sample::Provider));
                let s = match cp {
                    Some(cp) => EngineSession::restore(inputs, keys, options, cp),
                    None => EngineSession::new(inputs, keys, options),
                }
                .unwrap();
                let mut text =
                    format!("#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:{start}\n");
                for n in start..3 {
                    if explicit_gap && n == 1 {
                        text.push_str("#EXT-X-GAP\n");
                    }
                    text.push_str(&format!("#EXTINF:2,\ns{n}\n"));
                }
                text.push_str("#EXT-X-ENDLIST\n");
                s.handle()
                    .accept_snapshot(
                        &id("v"),
                        &parse_playlist_snapshot(
                            &TextResource {
                                location: SourceLocation::Url(
                                    url::Url::parse("https://live.test/list").unwrap(),
                                ),
                                content: text,
                            },
                            PlaylistContext::new(id("v"), 0),
                        )
                        .unwrap(),
                    )
                    .unwrap();
                s
            };
            let temp = Temp::new();
            let (options, latest) = capture(true);
            make(None, 0, false)
                .write_recoverable_to_file(temp.output(), options)
                .await
                .unwrap_err();
            let cp = latest.lock().unwrap().clone().unwrap();
            let (options, _) = capture(false);
            let report = make(Some(cp), 2, false)
                .write_recoverable_to_file(temp.output(), options)
                .await
                .unwrap_or_else(|e| panic!("{policy:?} {gaps:?}: {e:?}"));
            assert_eq!(report.media().gap_count(), 1);
            assert_eq!(report.media().inputs()[0].committed(), 3);
            let reference = Temp::new();
            let (options, _) = capture(false);
            let expected = make(None, 0, true)
                .write_recoverable_to_file(reference.output(), options)
                .await
                .unwrap();
            assert_eq!(
                report.media().outputs().len(),
                expected.media().outputs().len()
            );
            // Samples outside the missing interval stay byte-identical. A TS tail
            // learned before eviction can retain more precise timing than a
            // manifest gap, so compare packet payload hashes independently below.
            for (actual, expected) in [
                (temp.output(), reference.output()),
                (child(&temp.output()), child(&reference.output())),
            ] {
                if actual.exists() {
                    let packets = |path: PathBuf| {
                        let bytes = std::fs::read(path).unwrap();
                        let mut p = 0;
                        let mut data = vec![];
                        while p < bytes.len() {
                            let n =
                                u32::from_be_bytes(bytes[p..p + 4].try_into().unwrap()) as usize;
                            if &bytes[p + 4..p + 8] == b"mdat" {
                                data.extend_from_slice(&bytes[p + 8..p + n]);
                            }
                            p += n;
                        }
                        data
                    };
                    assert_eq!(packets(actual), packets(expected));
                }
            }
        }
    }
}

#[tokio::test]
async fn split_ledger_growth_is_bounded_by_checkpoint_metadata() {
    fn snapshot(start: u64, count: u64) -> PlaylistSnapshot {
        let mut text = format!(
            "#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXT-X-MEDIA-SEQUENCE:{start}\n#EXT-X-DISCONTINUITY-SEQUENCE:{start}\n"
        );
        for n in start..start + count {
            if n != start {
                text.push_str("#EXT-X-DISCONTINUITY\n");
            }
            text.push_str(if n.is_multiple_of(2) {
                "#EXT-X-MAP:URI=\"a.init\"\n#EXTINF:2,\navc\n"
            } else {
                "#EXT-X-MAP:URI=\"h.init\"\n#EXTINF:2,\nhevc\n"
            });
        }
        parse_playlist_snapshot(
            &TextResource {
                location: SourceLocation::Url(
                    url::Url::parse("https://ledger.test/index").unwrap(),
                ),
                content: text,
            },
            PlaylistContext::new(id("main"), 0).with_revision(start + count),
        )
        .unwrap()
    }
    fn create(metadata: usize) -> EngineSession {
        let source = MemorySource::new()
            .segment(
                "https://ledger.test/a.init",
                include_bytes!("fixtures/sample_crypto/fmp4_avc_clear/init.mp4"),
            )
            .segment(
                "https://ledger.test/avc",
                include_bytes!("fixtures/sample_crypto/fmp4_avc_clear/seg1.m4s"),
            )
            .segment(
                "https://ledger.test/h.init",
                include_bytes!("fixtures/sample_crypto/fmp4_hevc_clear/init.mp4"),
            )
            .segment(
                "https://ledger.test/hevc",
                include_bytes!("fixtures/sample_crypto/fmp4_hevc_clear/seg1.m4s"),
            );
        let holder = Arc::new(Mutex::new(None::<EngineHandle>));
        let callback = holder.clone();
        let options = EngineOptions::default()
            .with_change_policy(TimelineChangePolicy::Split)
            .with_limits(
                EngineLimits::default()
                    .with_queue(2, metadata)
                    .with_history(1),
            )
            .with_on_event(Arc::new(move |event| {
                if let EngineEvent::Committed { input, .. } = event {
                    let n = input.committed();
                    let guard = callback.lock().unwrap();
                    let h = guard.as_ref().unwrap();
                    if n == 64 {
                        h.end_input(&id("main")).unwrap();
                    } else {
                        h.accept_snapshot(&id("main"), &snapshot(n - 1, 2)).unwrap();
                    }
                }
            }));
        let s = EngineSession::new(
            EngineInputs::new(
                EngineInput::new(id("main"), Arc::new(source)),
                EmbeddedAudio::Exclude,
            ),
            sample::keys(Arc::new(sample::Provider)),
            options,
        )
        .unwrap();
        *holder.lock().unwrap() = Some(s.handle());
        s.handle()
            .accept_snapshot(&id("main"), &snapshot(0, 1))
            .unwrap();
        s
    }
    let reference = Temp::new();
    let sizes = Arc::new(Mutex::new(vec![]));
    let copy = sizes.clone();
    create(1024 * 1024)
        .write_recoverable_to_file(
            reference.output(),
            RecoveryOptions::new(Arc::new(move |cp| {
                copy.lock()
                    .unwrap()
                    .push((cp.completed_outputs(), cp.to_bytes().len()));
                Ok(())
            })),
        )
        .await
        .unwrap();
    let rows = sizes.lock().unwrap().clone();
    assert_eq!(rows.last().unwrap().0, 63);
    let early = rows
        .iter()
        .filter(|(n, _)| *n < 4)
        .map(|(_, bytes)| *bytes)
        .max()
        .unwrap();
    let peak = rows.iter().map(|(_, bytes)| *bytes).max().unwrap();
    let budget = early + 512;
    assert!(
        peak > budget,
        "ledger growth must be accounted for, {rows:?}"
    );
    let temp = Temp::new();
    let saved = Arc::new(Mutex::new(None));
    let copy = saved.clone();
    let error = create(budget)
        .write_recoverable_to_file(
            temp.output(),
            RecoveryOptions::new(Arc::new(move |cp| {
                assert!(cp.to_bytes().len() <= budget);
                *copy.lock().unwrap() = Some(cp);
                Ok(())
            })),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind(), EngineErrorKind::BudgetExceeded);
    let cp = saved.lock().unwrap().clone().unwrap();
    assert!(cp.completed_outputs() > 0 && cp.completed_outputs() < 63);
    for index in 0..cp.completed_outputs() {
        let path = if index == 0 {
            temp.output()
        } else {
            temp.0.join(format!("output.mp4.part-{index:06}.mp4"))
        };
        let bytes = std::fs::read(path).unwrap();
        let (size, hash) = cp.completed_output(index).unwrap();
        use sha2::{Digest, Sha256};
        assert_eq!(size, bytes.len() as u64);
        assert_eq!(hash.as_slice(), Sha256::digest(bytes).as_slice());
    }
    let evidence = serde_json::json!({"outputs":64,"earlyCheckpointBytes":early,"peakCheckpointBytes":peak,"restrictedBudget":budget,"completedChildrenAtBudgetFailure":cp.completed_outputs(),"publishedChildrenPreserved":true});
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/engine-ledger-evidence.json");
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    std::fs::write(out, serde_json::to_vec_pretty(&evidence).unwrap()).unwrap();
}

#[tokio::test]
#[ignore = "explicit schema fixture maintenance; never regenerate during normal tests"]
async fn generate_schema2_fixture() {
    let temp = Temp::new();
    let (options, saved) = capture(true);
    create(None, true, true)
        .write_recoverable_to_file(temp.output(), options)
        .await
        .unwrap_err();
    let cp = saved.lock().unwrap().clone().unwrap();
    std::fs::write(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/checkpoint/engine-schema2.bin"),
        cp.to_bytes(),
    )
    .unwrap();
}

#[test]
fn frozen_schema2_archive_roundtrips_without_reencoding_drift() {
    let bytes = include_bytes!("fixtures/checkpoint/engine-schema2.bin");
    let checkpoint = EngineCheckpoint::from_bytes(bytes).unwrap();
    assert_eq!(checkpoint.schema_version(), 2);
    assert!(checkpoint.bytes_written() > 0);
    assert_eq!(checkpoint.output_index(), 0);
    assert!(!checkpoint.is_completed() && !checkpoint.is_sealed());
    assert_eq!(checkpoint.to_bytes(), bytes);
    #[cfg(feature = "serde")]
    {
        let json = serde_json::to_value(&checkpoint).unwrap();
        assert_eq!(json["schema_version"], 2);
        assert!(
            json["archive"]
                .as_str()
                .unwrap()
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        let restored: EngineCheckpoint = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(restored.to_bytes(), bytes);
        let mut future = json;
        future["schema_version"] = 3.into();
        assert!(serde_json::from_value::<EngineCheckpoint>(future).is_err());
    }
}

#[tokio::test]
async fn fresh_acquisition_never_truncates_a_competing_partial() {
    for format in [OutputFormat::FragmentedMp4, OutputFormat::StreamingMp4] {
        for index in [0, 1] {
            let temp = Temp::new();
            let path = temp.output();
            let target = if index == 0 {
                path.clone()
            } else {
                child(&path)
            };
            let other = partial(&target);
            let collision = other.clone();
            let marker = b"another writer acquired this path after the intent checkpoint";
            let error = split_session(None, true)
                .write_recoverable_to_file(
                    &path,
                    RecoveryOptions::new(Arc::new(move |cp| {
                        if cp.output_index() == index && cp.bytes_written() == 0 {
                            std::fs::write(&collision, marker).unwrap();
                        }
                        Ok(())
                    }))
                    .with_output_format(format),
                )
                .await
                .unwrap_err();
            assert_eq!(error.kind(), EngineErrorKind::Output);
            assert_eq!(std::fs::read(&other).unwrap(), marker);
            assert!(!target.exists());
            if index == 1 {
                assert!(path.exists());
            }
        }
    }
}
