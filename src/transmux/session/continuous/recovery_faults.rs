//! Process-isolated filesystem boundary faults; compiled only by `cargo test --lib`.
use super::*;
use crate::crypto::key::*;
use crate::playlist::*;
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

static HITS: OnceLock<Mutex<BTreeMap<String, usize>>> = OnceLock::new();
pub(super) fn hit(stage: &str) -> std::io::Result<()> {
    if std::env::var_os("HLS_FAULT_CHILD").is_none() {
        return Ok(());
    }
    let mut hits = HITS.get_or_init(Mutex::default).lock().unwrap();
    let count = hits.entry(stage.to_owned()).or_default();
    *count += 1;
    if std::env::var("HLS_FAULT_POINT").ok().as_deref() == Some(&format!("{stage}:{count}")) {
        if std::env::var("HLS_FAULT_ACTION").unwrap() == "exit" {
            std::process::exit(83);
        }
        return Err(std::io::Error::other("injected filesystem failure"));
    }
    Ok(())
}
struct Provider;
impl KeyProvider for Provider {
    fn resolve(&self, _: KeyRequest) -> KeyFuture<KeyResolution> {
        panic!("clear recovery must not request keys")
    }
}
struct Clock;
impl KeyClock for Clock {
    fn now(&self) -> u64 {
        0
    }
}
fn session(checkpoint: Option<EngineCheckpoint>) -> MultiTrackSession {
    let terminal = checkpoint.as_ref().is_some_and(|c| c.is_finalizing());
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_crypto");
    let mut source = crate::MemorySource::new();
    for (name, fixture) in [
        ("a.init", "fmp4_avc_clear/init.mp4"),
        ("avc", "fmp4_avc_clear/seg1.m4s"),
        ("h.init", "fmp4_hevc_clear/init.mp4"),
        ("hevc", "fmp4_hevc_clear/seg1.m4s"),
    ] {
        source = source.segment(
            format!("https://fault.test/{name}"),
            std::fs::read(root.join(fixture)).unwrap(),
        );
    }
    let id = InputId::new("main").unwrap();
    let mut inputs = MultiTrackInputs::new(
        ContinuousInput::new(id.clone(), Arc::new(source)),
        EmbeddedAudio::Exclude,
    );
    let sidecar = std::env::var("HLS_FAULT_SIDECAR").ok().as_deref() == Some("1");
    let cc = InputId::new("cc").unwrap();
    if sidecar {
        inputs = inputs.with_subtitle(SubtitleTrack::new(
            cc.clone(),
            id.clone(),
            TrackMetadata::default(),
        ));
    }
    let keys = KeySession::new(
        "fault",
        "scope",
        Arc::new(Provider),
        Arc::new(Clock),
        KeySessionOptions::default(),
    )
    .unwrap();
    let options = ContinuousOptions::default().with_change_policy(TimelineChangePolicy::Split);
    let s = match checkpoint {
        Some(cp) => MultiTrackSession::restore(inputs, keys, options, cp),
        None => MultiTrackSession::new(inputs, keys, options),
    }
    .unwrap();
    let s = if sidecar {
        let dir = std::path::PathBuf::from(std::env::var_os("HLS_FAULT_CHILD").unwrap());
        s.with_recoverable_subtitle_sink(Arc::new(Files(SubtitleDestination::new(
            cc.clone(),
            dir.join("captions.vtt"),
        ))))
    } else {
        s
    };
    if !terminal {
        if sidecar {
            let track = s.handle().subtitle_track_id(&cc).unwrap();
            let start = MediaTime::new(0, 1000).unwrap();
            let end = MediaTime::new(2000, 1000).unwrap();
            s.handle()
                .accept_cues(
                    track,
                    &[
                        SubtitleCue::new(0, 0, start, end, "first"),
                        SubtitleCue::new(0, 1, start, end, "second"),
                    ],
                )
                .unwrap();
            s.handle().end_subtitles(track).unwrap();
        }
        let snapshot = parse_playlist_snapshot(&TextResource {
            location: SourceLocation::Url(url::Url::parse("https://fault.test/index").unwrap()),
            content: "#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXT-X-MAP:URI=\"a.init\"\n#EXTINF:2,\navc\n#EXT-X-DISCONTINUITY\n#EXT-X-MAP:URI=\"h.init\"\n#EXTINF:2,\nhevc\n#EXT-X-ENDLIST\n".into(),
        }, PlaylistContext::new(id.clone(), 0)).unwrap();
        s.handle().accept_snapshot(&id, &snapshot).unwrap();
    }
    s
}

#[test]
#[ignore = "invoked in an isolated process by filesystem_fault_boundaries"]
fn filesystem_fault_child() {
    let dir = std::path::PathBuf::from(std::env::var_os("HLS_FAULT_CHILD").unwrap());
    let checkpoint_path = dir.join("checkpoint");
    let checkpoint = std::fs::read(&checkpoint_path)
        .ok()
        .map(|b| EngineCheckpoint::from_bytes(&b).unwrap());
    let format = if std::env::var("HLS_FAULT_CLASSIC").unwrap() == "1" {
        OutputFormat::StreamingMp4
    } else {
        OutputFormat::FragmentedMp4
    };
    let recovery = RecoveryOptions::new(Arc::new(move |cp| {
        let temporary = checkpoint_path.with_extension("new");
        let mut file = std::fs::File::create(&temporary).unwrap();
        std::io::Write::write_all(&mut file, &cp.to_bytes()).unwrap();
        file.sync_all().unwrap();
        drop(file);
        // Windows rename does not replace an existing file. Child processes only
        // terminate at engine I/O hooks, never inside this persistence callback.
        if checkpoint_path.exists() {
            std::fs::remove_file(&checkpoint_path).unwrap();
        }
        std::fs::rename(temporary, &checkpoint_path).unwrap();
        Ok(())
    }))
    .with_output_format(format)
    .with_durability(CheckpointDurability::SyncAll);
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(session(checkpoint).write_recoverable_to_file(&dir.join("output.mp4"), recovery));
    if std::env::var_os("HLS_FAULT_POINT").is_some() {
        assert!(matches!(
            result.unwrap_err().kind(),
            ContinuousErrorKind::Output | ContinuousErrorKind::SubtitleOutput
        ));
    } else {
        result.unwrap();
    }
    std::fs::write(
        dir.join("hits.json"),
        serde_json::to_vec(&*HITS.get().unwrap().lock().unwrap()).unwrap(),
    )
    .unwrap();
}

fn canonical(mut bytes: Vec<u8>) -> Vec<u8> {
    fn walk(bytes: &mut [u8]) {
        let mut offset = 0;
        while offset < bytes.len() {
            let size = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
            assert!(size >= 8 && offset + size <= bytes.len());
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
fn filesystem_fault_boundaries() {
    let root = std::env::temp_dir().join(format!("engine-io-faults-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let run = |dir: &Path, classic: bool, sidecar: bool, point: Option<&str>, action: &str| {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["filesystem_fault_child", "--ignored", "--nocapture"])
            .env("HLS_FAULT_CHILD", dir)
            .env("HLS_FAULT_CLASSIC", if classic { "1" } else { "0" })
            .env("HLS_FAULT_SIDECAR", if sidecar { "1" } else { "0" })
            .env_remove("HLS_FAULT_POINT")
            .env("HLS_FAULT_ACTION", action);
        if let Some(point) = point {
            command.env("HLS_FAULT_POINT", point);
        }
        let output = command.output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(if point.is_some() && action == "exit" {
                83
            } else {
                0
            }),
            "{classic} {point:?} {action}\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    };
    let mut windows = 0;
    for (classic, sidecar) in [(false, false), (true, false), (false, true), (true, true)] {
        let reference = root.join(format!("reference-{classic}-{sidecar}"));
        std::fs::create_dir(&reference).unwrap();
        run(&reference, classic, sidecar, None, "none");
        let hits: BTreeMap<String, usize> =
            serde_json::from_slice(&std::fs::read(reference.join("hits.json")).unwrap()).unwrap();
        for (stage, count) in hits {
            // Cover early and late writes (including the second split child), and
            // every flush/sync/finalization/publication boundary in between.
            let counts: Vec<_> = if stage.starts_with("write.") {
                vec![1, 2, count / 2]
            } else {
                (1..=count).collect()
            };
            for count in counts {
                for action in ["error", "exit"] {
                    let point = format!("{stage}:{count}");
                    let dir = root.join(format!("case-{windows}"));
                    std::fs::create_dir(&dir).unwrap();
                    run(&dir, classic, sidecar, Some(&point), action);
                    run(&dir, classic, sidecar, None, "none");
                    for name in ["output.mp4", "output.mp4.part-000001.mp4"] {
                        assert_eq!(
                            canonical(std::fs::read(dir.join(name)).unwrap()),
                            canonical(std::fs::read(reference.join(name)).unwrap()),
                            "{point} {action} {name}"
                        );
                    }
                    if sidecar {
                        for name in ["captions.vtt", "captions.vtt.part-000001.vtt"] {
                            assert_eq!(
                                std::fs::read(dir.join(name)).unwrap(),
                                std::fs::read(reference.join(name)).unwrap(),
                                "{point} {action} {name}"
                            );
                        }
                    }
                    std::fs::remove_dir_all(&dir).unwrap();
                    windows += 1;
                }
            }
        }
    }
    eprintln!("verified {windows} filesystem error/process-exit recovery windows");
    std::fs::remove_dir_all(root).unwrap();
}

struct Files(SubtitleDestination);
impl SubtitleSink for Files {
    fn commit<'a>(&'a self, batch: &'a SubtitleCommit) -> SubtitleSinkFuture<'a> {
        Box::pin(async move {
            use std::io::Write;
            for cue in batch.cues() {
                let mut file = std::fs::OpenOptions::new()
                    .append(true)
                    .open(self.0.partial_path(cue.output_index()))?;
                hit("sidecar.write.before")?;
                writeln!(
                    file,
                    "{}:{}:{}:{}",
                    cue.receipt(),
                    cue.cue().payload(),
                    cue.start().ticks(),
                    cue.end().ticks()
                )?;
                file.sync_all()?;
                hit("sidecar.write.after")?;
            }
            Ok(())
        })
    }
    fn finish(&self) -> SubtitleSinkFuture<'_> {
        Box::pin(async { Ok(()) })
    }
}
impl RecoverableSubtitleSink for Files {
    fn destinations(&self) -> Vec<SubtitleDestination> {
        vec![self.0.clone()]
    }
    fn format_identity(&self) -> &str {
        "fault-test-v1"
    }
    fn publish(&self, output: u64) -> SubtitleSinkFuture<'_> {
        Box::pin(async move {
            if !self.0.final_path(output).exists() {
                std::fs::hard_link(self.0.partial_path(output), self.0.final_path(output))?;
            }
            Ok(())
        })
    }
}
