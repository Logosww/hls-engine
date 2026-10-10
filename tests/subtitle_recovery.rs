//! SDK-like fixed-file adapter and process-isolated joint recovery contract.
#[path = "support/subtitle_contract.rs"]
mod suite;
use hls_engine::*;
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

static HITS: OnceLock<Mutex<BTreeMap<String, usize>>> = OnceLock::new();
fn hit(stage: &str) {
    if std::env::var_os("HLS_SIDECAR_CHILD").is_none() {
        return;
    }
    let mut hits = HITS.get_or_init(Mutex::default).lock().unwrap();
    let count = hits.entry(stage.into()).or_default();
    *count += 1;
    if std::env::var("HLS_SIDECAR_KILL").ok().as_deref() == Some(&format!("{stage}:{count}")) {
        #[cfg(unix)]
        {
            let status = std::process::Command::new("kill")
                .args(["-KILL", &std::process::id().to_string()])
                .status()
                .expect("send SIGKILL to fault child");
            assert!(status.success(), "SIGKILL command failed");
            // Do not race signal delivery with a normal process exit.
            loop {
                std::thread::park();
            }
        }
        #[cfg(not(unix))]
        std::process::exit(83);
    }
}
struct Files {
    destinations: Vec<SubtitleDestination>,
}
impl Files {
    fn new(dir: &Path) -> Self {
        Self {
            destinations: ["cc", "cc2"]
                .into_iter()
                .map(|id| SubtitleDestination::new(suite::id(id), dir.join(format!("{id}.vtt"))))
                .collect(),
        }
    }
}
fn timestamp(t: MediaTime) -> String {
    let ms = t.ticks() * 1000 / i128::from(t.timescale());
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}
impl SubtitleSink for Files {
    fn commit<'a>(&'a self, batch: &'a SubtitleCommit) -> SubtitleSinkFuture<'a> {
        Box::pin(async move {
            hit("commit.before"); // Media has been written, but no sidecar yet.
            for d in &self.destinations {
                let outputs: std::collections::BTreeSet<_> = batch
                    .frontiers()
                    .iter()
                    .filter(|f| f.input_id() == d.input_id())
                    .map(|f| f.output_index())
                    .chain(
                        batch
                            .cues()
                            .iter()
                            .filter(|c| c.input_id() == d.input_id())
                            .map(|c| c.output_index()),
                    )
                    .collect();
                for output in outputs {
                    let path = d.partial_path(output);
                    let mut file = std::fs::OpenOptions::new().append(true).open(&path)?;
                    let mut bytes = String::new();
                    if file.metadata()?.len() == 0 {
                        bytes.push_str("WEBVTT\n\n");
                    }
                    for cue in batch
                        .cues()
                        .iter()
                        .filter(|c| c.input_id() == d.input_id() && c.output_index() == output)
                    {
                        if matches!(
                            cue.disposition(),
                            SubtitleDisposition::Written | SubtitleDisposition::Clipped
                        ) {
                            bytes.push_str(&format!(
                                "NOTE receipt={}\n\n{}\n{} --> {} {}\n{}\n\n",
                                cue.receipt(),
                                cue.cue().identifier(),
                                timestamp(cue.start()),
                                timestamp(cue.end()),
                                cue.cue().settings(),
                                cue.cue().payload()
                            ));
                        }
                    }
                    for f in batch
                        .frontiers()
                        .iter()
                        .filter(|f| f.input_id() == d.input_id() && f.output_index() == output)
                    {
                        bytes.push_str(&format!(
                            "NOTE frontier={}/{}/{}/{}\n\n",
                            f.generation(),
                            f.epoch(),
                            f.output_index(),
                            f.end().ticks()
                        ));
                    }
                    let bytes = bytes.as_bytes();
                    file.write_all(&bytes[..bytes.len() / 2])?;
                    file.sync_all()?;
                    hit("sidecar.write.middle");
                    file.write_all(&bytes[bytes.len() / 2..])?;
                    file.sync_all()?;
                    hit("sidecar.write.after");
                }
            }
            hit("commit.after");
            Ok(())
        })
    }
    fn finish(&self) -> SubtitleSinkFuture<'_> {
        Box::pin(async {
            hit("close.before");
            hit("close.after");
            Ok(())
        })
    }
}
impl RecoverableSubtitleSink for Files {
    fn destinations(&self) -> Vec<SubtitleDestination> {
        self.destinations.clone()
    }
    fn format_identity(&self) -> &str {
        "test-sdk-webvtt-v1-ms"
    }
    fn publish(&self, output: u64) -> SubtitleSinkFuture<'_> {
        Box::pin(async move {
            for d in &self.destinations {
                hit("sidecar.publish.before");
                if !d.final_path(output).exists() {
                    std::fs::hard_link(d.partial_path(output), d.final_path(output))?;
                }
                hit("sidecar.publish.after");
            }
            Ok(())
        })
    }
}
fn session(
    dir: &Path,
    checkpoint: Option<EngineCheckpoint>,
    embedded: bool,
    replay: bool,
) -> EngineSession {
    let terminal = checkpoint.as_ref().is_some_and(|c| c.is_finalizing());
    let inputs = suite::inputs(embedded).with_subtitle(
        SubtitleTrack::new(
            suite::id("cc2"),
            suite::id("media"),
            TrackMetadata::new("fr", "French"),
        )
        .with_embedded(embedded),
    );
    let keys = suite::keys(Arc::new(suite::Provider::default()));
    let options = EngineOptions::default();
    let s = match checkpoint {
        Some(cp) => EngineSession::restore(inputs, keys, options, cp),
        None => EngineSession::new(inputs, keys, options),
    }
    .unwrap()
    .with_recoverable_subtitle_sink(Arc::new(Files::new(dir)));
    if !terminal && replay {
        suite::feed(&s);
        let h = s.handle();
        let track = h.subtitle_track_id(&suite::id("cc2")).unwrap();
        h.accept_cues(
            track,
            &[
                SubtitleCue::new(7, 0, suite::time(250), suite::time(8000), "second")
                    .with_identifier("same"),
            ],
        )
        .unwrap();
        h.end_subtitles(track).unwrap();
    }
    s
}
fn persist(path: &Path, cp: &EngineCheckpoint) {
    hit("checkpoint.before");
    let bytes = cp.to_bytes();
    assert_eq!(
        EngineCheckpoint::from_bytes(&bytes).unwrap().to_bytes(),
        bytes
    );
    assert_eq!(cp.schema_version(), 3);
    let temporary = path.with_extension("new");
    let mut file = std::fs::File::create(&temporary).unwrap();
    file.write_all(&bytes[..bytes.len() / 2]).unwrap();
    file.sync_all().unwrap();
    hit("checkpoint.write.middle");
    file.write_all(&bytes[bytes.len() / 2..]).unwrap();
    file.sync_all().unwrap();
    drop(file);
    #[cfg(windows)]
    if path.exists() {
        std::fs::remove_file(path).unwrap();
    }
    std::fs::rename(temporary, path).unwrap();
    hit("checkpoint.after");
}
fn options(dir: &Path, classic: bool) -> RecoveryOptions {
    let path = dir.join("checkpoint");
    RecoveryOptions::new(Arc::new(move |cp| {
        persist(&path, &cp);
        Ok(())
    }))
    .with_durability(CheckpointDurability::SyncAll)
    .with_output_format(if classic {
        OutputFormat::StreamingMp4
    } else {
        OutputFormat::FragmentedMp4
    })
}
#[test]
#[ignore = "process child for joint_sidecar_sigkill_windows"]
fn subtitle_recovery_child() {
    let dir = PathBuf::from(std::env::var_os("HLS_SIDECAR_CHILD").unwrap());
    let cp = std::fs::read(dir.join("checkpoint"))
        .ok()
        .map(|b| EngineCheckpoint::from_bytes(&b).unwrap());
    let embedded = std::env::var("HLS_SIDECAR_EMBEDDED").unwrap() == "1";
    let classic = std::env::var("HLS_SIDECAR_CLASSIC").unwrap() == "1";
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(
            session(&dir, cp, embedded, true)
                .write_recoverable_to_file(dir.join("media.mp4"), options(&dir, classic)),
        )
        .unwrap();
    std::fs::write(
        dir.join("hits.json"),
        serde_json::to_vec(&*HITS.get_or_init(Mutex::default).lock().unwrap()).unwrap(),
    )
    .unwrap();
}
fn canonical(mut bytes: Vec<u8>) -> Vec<u8> {
    fn walk(bytes: &mut [u8]) {
        let mut offset = 0;
        while offset < bytes.len() {
            let size = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
            let kind: [u8; 4] = bytes[offset + 4..offset + 8].try_into().unwrap();
            let content = &mut bytes[offset + 8..offset + size];
            match &kind {
                b"moov" | b"trak" | b"mdia" => walk(content),
                b"mvhd" | b"tkhd" | b"mdhd" => {
                    let end = if content[0] == 1 { 20 } else { 12 };
                    content[4..end].fill(0);
                }
                _ => {}
            }
            offset += size;
        }
    }
    walk(&mut bytes);
    bytes
}
#[test]
fn joint_sidecar_sigkill_windows() {
    let root = std::env::temp_dir().join(format!("hls-sidecar-kill-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let run = |dir: &Path, embedded: bool, classic: bool, point: Option<&str>| {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["subtitle_recovery_child", "--ignored", "--nocapture"])
            .env("HLS_SIDECAR_CHILD", dir)
            .env("HLS_SIDECAR_EMBEDDED", if embedded { "1" } else { "0" })
            .env("HLS_SIDECAR_CLASSIC", if classic { "1" } else { "0" })
            .env_remove("HLS_SIDECAR_KILL");
        if let Some(point) = point {
            command.env("HLS_SIDECAR_KILL", point);
        }
        let output = command.output().unwrap();
        if point.is_some() {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                assert_eq!(
                    output.status.signal(),
                    Some(9),
                    "{point:?}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            #[cfg(not(unix))]
            assert_eq!(output.status.code(), Some(83));
        } else {
            assert!(
                output.status.success(),
                "{embedded} {classic} {point:?}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    };
    let mut windows = 0;
    for (embedded, classic) in [(true, false), (false, false), (true, true), (false, true)] {
        let reference = root.join(format!("reference-{embedded}-{classic}"));
        std::fs::create_dir(&reference).unwrap();
        run(&reference, embedded, classic, None);
        let hits: BTreeMap<String, usize> =
            serde_json::from_slice(&std::fs::read(reference.join("hits.json")).unwrap()).unwrap();
        for (stage, count) in hits {
            for number in 1..=count {
                let point = format!("{stage}:{number}");
                let dir = root.join(format!("case-{windows}"));
                std::fs::create_dir(&dir).unwrap();
                run(&dir, embedded, classic, Some(&point));
                run(&dir, embedded, classic, None);
                for file in ["cc.vtt", "cc2.vtt", "media.mp4"] {
                    let actual = std::fs::read(dir.join(file)).unwrap();
                    let expected = std::fs::read(reference.join(file)).unwrap();
                    if file.ends_with("mp4") {
                        assert_eq!(canonical(actual), canonical(expected), "{point} {file}");
                    } else {
                        assert_eq!(
                            String::from_utf8(actual).unwrap(),
                            String::from_utf8(expected).unwrap(),
                            "{point} {file}"
                        );
                    }
                }
                // Completed is offline and works after all partials disappear.
                for file in [
                    "cc.vtt.hls-partial",
                    "cc2.vtt.hls-partial",
                    "media.mp4.hls-partial",
                ] {
                    std::fs::remove_file(dir.join(file)).unwrap();
                }
                run(&dir, embedded, classic, None);
                std::fs::remove_dir_all(dir).unwrap();
                windows += 1;
            }
        }
    }
    eprintln!("verified {windows} joint SIGKILL windows");
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn joint_validation_precedes_every_truncation() {
    let root = std::env::temp_dir().join(format!("hls-sidecar-validation-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let reference = root.join("reference");
    std::fs::create_dir(&reference).unwrap();
    session(&reference, None, true, true)
        .write_recoverable_to_file(reference.join("media.mp4"), options(&reference, false))
        .await
        .unwrap();
    let names = [
        "media.mp4.hls-partial",
        "cc.vtt.hls-partial",
        "cc2.vtt.hls-partial",
    ];
    for case in [
        "media-corrupt",
        "first-corrupt",
        "last-corrupt",
        "last-short",
        "missing-replay",
        "destination",
        "invalid-selection",
        "stray-final",
        "valid",
        "new-cue",
    ] {
        let dir = root.join(case);
        std::fs::create_dir(&dir).unwrap();
        let latest = Arc::new(Mutex::new(None));
        let capture = latest.clone();
        let result = session(&dir, None, true, true)
            .write_recoverable_to_file(
                dir.join("media.mp4"),
                RecoveryOptions::new(Arc::new(move |cp| {
                    if cp.bytes_written() > 0 {
                        *capture.lock().unwrap() = Some(cp);
                        return Err(EngineError::output(
                            std::io::Error::other("checkpoint interrupted").into(),
                        ));
                    }
                    Ok(())
                })),
            )
            .await;
        assert!(result.is_err());
        let cp: EngineCheckpoint = latest.lock().unwrap().take().unwrap();
        #[cfg(feature = "serde")]
        assert_eq!(
            serde_json::from_slice::<EngineCheckpoint>(&serde_json::to_vec(&cp).unwrap())
                .unwrap()
                .to_bytes(),
            cp.to_bytes()
        );
        for name in names {
            std::fs::OpenOptions::new()
                .append(true)
                .open(dir.join(name))
                .unwrap()
                .write_all(b"UNCOMMITTED TAIL")
                .unwrap();
        }
        match case {
            "media-corrupt" | "first-corrupt" | "last-corrupt" => {
                let index = match case {
                    "media-corrupt" => 0,
                    "first-corrupt" => 1,
                    _ => 2,
                };
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(dir.join(names[index]))
                    .unwrap()
                    .write_all(b"!")
                    .unwrap();
            }
            "last-short" => {
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(dir.join(names[2]))
                    .unwrap()
                    .set_len(1)
                    .unwrap();
            }
            "stray-final" => std::fs::write(dir.join("cc2.vtt"), b"unrelated").unwrap(),
            _ => {}
        }
        let before: Vec<_> = names
            .iter()
            .map(|n| std::fs::read(dir.join(n)).unwrap())
            .collect();
        let mut s = session(
            &dir,
            Some(cp),
            true,
            case != "missing-replay" && case != "new-cue",
        );
        let new_receipt = if case == "new-cue" {
            let h = s.handle();
            let track = h.subtitle_track_id(&suite::id("cc")).unwrap();
            let receipt = h
                .accept_cues(
                    track,
                    &[SubtitleCue::new(
                        7,
                        0,
                        suite::time(8000),
                        suite::time(9000),
                        "new after restore",
                    )
                    .with_identifier("new")],
                )
                .unwrap()
                .first_receipt()
                .unwrap();
            assert!(receipt >= 3, "new submissions cannot reuse a saved receipt");
            suite::feed(&s);
            let track = h.subtitle_track_id(&suite::id("cc2")).unwrap();
            h.accept_cues(
                track,
                &[
                    SubtitleCue::new(7, 0, suite::time(250), suite::time(8000), "second")
                        .with_identifier("same"),
                ],
            )
            .unwrap();
            h.end_subtitles(track).unwrap();
            Some(receipt)
        } else {
            None
        };
        if case == "destination" {
            let mut files = Files::new(&dir);
            files.destinations[1] =
                SubtitleDestination::new(suite::id("cc2"), dir.join("other.vtt"));
            s = s.with_recoverable_subtitle_sink(Arc::new(files));
        }
        if case == "invalid-selection" {
            let mut files = Files::new(&dir);
            files.destinations[1] = files.destinations[0].clone();
            s = s.with_recoverable_subtitle_sink(Arc::new(files));
        }
        let handle = s.handle();
        let result = s
            .write_recoverable_to_file(dir.join("media.mp4"), options(&dir, false))
            .await;
        if let Some(receipt) = new_receipt {
            result.unwrap();
            let text = std::fs::read_to_string(dir.join("cc.vtt")).unwrap();
            assert!(
                text.contains(&format!("NOTE receipt={receipt}\n\nnew\n")),
                "new cue receipt changed after restore: {text}"
            );
        } else if case == "valid" {
            result.unwrap();
            for file in ["cc.vtt", "cc2.vtt"] {
                assert_eq!(
                    std::fs::read(dir.join(file)).unwrap(),
                    std::fs::read(reference.join(file)).unwrap()
                );
            }
            assert_eq!(
                canonical(std::fs::read(dir.join("media.mp4")).unwrap()),
                canonical(std::fs::read(reference.join("media.mp4")).unwrap())
            );
        } else {
            let kind = result.unwrap_err().kind();
            assert_eq!(handle.state(), EngineState::Failed);
            assert!(
                matches!(
                    kind,
                    EngineErrorKind::ResumeConflict
                        | EngineErrorKind::ResumeCorruption
                        | EngineErrorKind::ReplayRequired
                ),
                "{case}: {kind:?}"
            );
            for (name, bytes) in names.iter().zip(before) {
                assert_eq!(
                    std::fs::read(dir.join(name)).unwrap(),
                    bytes,
                    "{case}: {name} mutated"
                );
            }
            assert!(!dir.join("other.vtt.hls-partial").exists());
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn subtitle_key_change_preserves_all_tails_and_completed_is_offline() {
    use hls_engine::crypto::resource::ResourceRequest;
    use std::sync::atomic::Ordering;
    async fn bound(dir: &Path, cp: Option<EngineCheckpoint>, version: usize) -> EngineSession {
        let provider = Arc::new(suite::Provider::default());
        provider.version.store(version, Ordering::SeqCst);
        let inputs = suite::inputs(true).with_subtitle(SubtitleTrack::new(
            suite::id("cc2"),
            suite::id("media"),
            TrackMetadata::new("fr", "French"),
        ));
        let keys = suite::keys(provider);
        let session = match cp {
            Some(cp) => EngineSession::restore(inputs, keys, EngineOptions::default(), cp),
            None => EngineSession::new(inputs, keys, EngineOptions::default()),
        }
        .unwrap()
        .with_recoverable_subtitle_sink(Arc::new(Files::new(dir)));
        let h = session.handle();
        let p = suite::snapshot(
            "cc",
            "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:42\n#EXT-X-KEY:METHOD=AES-128,URI=\"key\"\n#EXTINF:1,\nseq\n",
        );
        let clear = h
            .read_subtitle_resource(
                Arc::new(MemorySource::new().segment("https://subtitle.test/seq", suite::SEQUENCE)),
                ResourceRequest::webvtt_media(&p, 0).unwrap(),
            )
            .await
            .unwrap();
        let track = h.subtitle_track_id(&suite::id("cc")).unwrap();
        h.accept_cues(
            track,
            &[
                SubtitleCue::new(7, 0, suite::time(0), suite::time(8000), "encrypted")
                    .with_resource(&clear)
                    .unwrap(),
            ],
        )
        .unwrap();
        suite::feed(&session);
        h.end_subtitles(h.subtitle_track_id(&suite::id("cc2")).unwrap())
            .unwrap();
        session
    }
    let dir = std::env::temp_dir().join(format!("hls-sidecar-keys-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let latest = Arc::new(Mutex::new(None));
    let capture = latest.clone();
    assert!(
        bound(&dir, None, 1)
            .await
            .write_recoverable_to_file(
                dir.join("media.mp4"),
                RecoveryOptions::new(Arc::new(move |cp| {
                    if cp.bytes_written() > 0 {
                        *capture.lock().unwrap() = Some(cp);
                        return Err(EngineError::output(
                            std::io::Error::other("interrupted").into(),
                        ));
                    }
                    Ok(())
                }))
            )
            .await
            .is_err()
    );
    let cp = latest.lock().unwrap().take().unwrap();
    let names = [
        "media.mp4.hls-partial",
        "cc.vtt.hls-partial",
        "cc2.vtt.hls-partial",
    ];
    for name in names {
        std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join(name))
            .unwrap()
            .write_all(b"tail")
            .unwrap();
    }
    let before: Vec<_> = names
        .iter()
        .map(|n| std::fs::read(dir.join(n)).unwrap())
        .collect();
    let error = bound(&dir, Some(cp.clone()), 2)
        .await
        .write_recoverable_to_file(dir.join("media.mp4"), options(&dir, false))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), EngineErrorKind::ReplayRequired);
    for (name, bytes) in names.iter().zip(before) {
        assert_eq!(std::fs::read(dir.join(name)).unwrap(), bytes);
    }
    bound(&dir, Some(cp), 1)
        .await
        .write_recoverable_to_file(dir.join("media.mp4"), options(&dir, false))
        .await
        .unwrap();
    let completed =
        EngineCheckpoint::from_bytes(&std::fs::read(dir.join("checkpoint")).unwrap()).unwrap();
    assert!(completed.is_completed());
    for name in names {
        std::fs::remove_file(dir.join(name)).unwrap();
    }
    // Empty transport and changed provider: completed restoration must use neither.
    let inputs = EngineInputs::new(
        EngineInput::new(suite::id("media"), Arc::new(MemorySource::new())),
        EmbeddedAudio::Keep,
    )
    .with_subtitle(SubtitleTrack::new(
        suite::id("cc"),
        suite::id("media"),
        TrackMetadata::new("en", "Captions"),
    ))
    .with_subtitle(SubtitleTrack::new(
        suite::id("cc2"),
        suite::id("media"),
        TrackMetadata::new("fr", "French"),
    ));
    let provider = Arc::new(suite::Provider::default());
    provider.version.store(99, Ordering::SeqCst);
    EngineSession::restore(
        inputs,
        suite::keys(provider.clone()),
        EngineOptions::default(),
        completed.clone(),
    )
    .unwrap()
    .with_recoverable_subtitle_sink(Arc::new(Files::new(&dir)))
    .write_recoverable_to_file(dir.join("media.mp4"), options(&dir, false))
    .await
    .unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    std::fs::write(dir.join("cc2.vtt"), b"corrupted final").unwrap();
    let before = std::fs::read(dir.join("media.mp4")).unwrap();
    assert_eq!(
        session(&dir, Some(completed), true, false)
            .write_recoverable_to_file(dir.join("media.mp4"), options(&dir, false))
            .await
            .unwrap_err()
            .kind(),
        EngineErrorKind::ResumeCorruption
    );
    assert_eq!(std::fs::read(dir.join("media.mp4")).unwrap(), before);
    assert!(!dir.join("media.mp4.hls-partial").exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn late_disposition_is_acknowledged_before_joint_checkpoint() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct ObservedFiles {
        files: Files,
        rejected: Arc<AtomicUsize>,
    }
    impl SubtitleSink for ObservedFiles {
        fn commit<'a>(&'a self, batch: &'a SubtitleCommit) -> SubtitleSinkFuture<'a> {
            Box::pin(async move {
                self.files.commit(batch).await?;
                self.rejected.fetch_add(
                    batch
                        .cues()
                        .iter()
                        .filter(|c| c.disposition() == SubtitleDisposition::RejectedLate)
                        .count(),
                    Ordering::SeqCst,
                );
                Ok(())
            })
        }
        fn finish(&self) -> SubtitleSinkFuture<'_> {
            self.files.finish()
        }
    }
    impl RecoverableSubtitleSink for ObservedFiles {
        fn destinations(&self) -> Vec<SubtitleDestination> {
            self.files.destinations()
        }
        fn format_identity(&self) -> &str {
            self.files.format_identity()
        }
        fn publish(&self, output: u64) -> SubtitleSinkFuture<'_> {
            self.files.publish(output)
        }
    }
    let dir = std::env::temp_dir().join(format!("hls-sidecar-late-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let holder: Arc<Mutex<Option<EngineHandle>>> = Arc::new(Mutex::new(None));
    let copy = holder.clone();
    let commits = AtomicUsize::new(0);
    let options = EngineOptions::default().with_on_event(Arc::new(move |event| {
        if matches!(event, EngineEvent::Committed { .. })
            && commits.fetch_add(1, Ordering::SeqCst) == 0
        {
            let guard = copy.lock().unwrap();
            let h = guard.as_ref().unwrap();
            // This arrives after the normal sink acknowledgement, immediately
            // before persistence. The checkpoint must not skip its disposition.
            h.accept_cues(
                h.subtitle_track_id(&suite::id("cc")).unwrap(),
                &[SubtitleCue::new(
                    7,
                    0,
                    suite::time(0),
                    suite::time(1),
                    "late",
                )],
            )
            .unwrap();
        }
    }));
    let rejected = Arc::new(AtomicUsize::new(0));
    let inputs = suite::inputs(true).with_subtitle(SubtitleTrack::new(
        suite::id("cc2"),
        suite::id("media"),
        TrackMetadata::new("fr", "French"),
    ));
    let s = EngineSession::new(
        inputs,
        suite::keys(Arc::new(suite::Provider::default())),
        options,
    )
    .unwrap()
    .with_recoverable_subtitle_sink(Arc::new(ObservedFiles {
        files: Files::new(&dir),
        rejected: rejected.clone(),
    }));
    let h = s.handle();
    *holder.lock().unwrap() = Some(h.clone());
    h.accept_snapshot(&suite::id("media"),&suite::snapshot("media","#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXT-X-MAP:URI=\"init\"\n#EXTINF:2,\nseg0\n#EXT-X-ENDLIST\n")).unwrap();
    s.write_recoverable_to_file(
        dir.join("media.mp4"),
        RecoveryOptions::new(Arc::new(move |cp| {
            if cp.bytes_written() > 0 {
                assert_eq!(rejected.load(Ordering::SeqCst), 1);
            }
            Ok(())
        })),
    )
    .await
    .unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}
