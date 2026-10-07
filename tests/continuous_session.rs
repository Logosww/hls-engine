use hls_engine::legacy::{playlist::*, *};
use std::sync::{Arc, Mutex};
#[allow(dead_code)]
#[path = "support/sample_crypto.rs"]
mod sample;
fn id() -> InputId {
    InputId::new("primary").unwrap()
}
fn snapshot(text: &str, revision: u64) -> PlaylistSnapshot {
    parse_playlist_snapshot(
        &TextResource {
            location: SourceLocation::Url(url::Url::parse("https://live.test/input.m3u8").unwrap()),
            content: text.into(),
        },
        PlaylistContext::new(id(), 1).with_revision(revision),
    )
    .unwrap()
}
fn open(text: &str) -> String {
    text.lines()
        .filter(|l| !l.starts_with("#EXT-X-ENDLIST") && !l.starts_with("#EXT-X-PLAYLIST-TYPE"))
        .collect::<Vec<_>>()
        .join("\n")
}
fn session(source: Arc<dyn Source>, options: ContinuousOptions) -> ContinuousSession {
    ContinuousSession::new(
        ContinuousInputs::new(ContinuousInput::new(id(), source)),
        sample::keys(Arc::new(sample::Provider)),
        options,
    )
    .unwrap()
}
fn source(case: &sample::Case) -> Arc<dyn Source> {
    let mut source = MemorySource::new();
    for (name, bytes) in case.files {
        source = source.segment(format!("https://live.test/{name}"), *bytes);
    }
    Arc::new(source)
}
#[tokio::test]
async fn every_sample_profile_outputs_before_endlist_and_drains_stop() {
    for case in sample::cases() {
        let holder = Arc::new(Mutex::new(None::<ContinuousHandle>));
        let callback = holder.clone();
        let events = Arc::new(Mutex::new(Vec::new()));
        let log = events.clone();
        let options = ContinuousOptions::default().with_on_event(Arc::new(move |event| {
            if matches!(event, ContinuousEvent::Committed { .. }) {
                callback.lock().unwrap().as_ref().unwrap().stop();
            }
            log.lock().unwrap().push(event);
        }));
        let session = session(source(&case), options);
        let handle = session.handle();
        *holder.lock().unwrap() = Some(handle.clone());
        let snapshot = snapshot(&open(case.playlist), 1);
        let expected = snapshot.segments().len();
        handle.accept_snapshot(&id(), &snapshot).unwrap();
        let mut bytes = Vec::new();
        let report = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            session.write_to(&mut bytes),
        )
        .await
        .unwrap()
        .unwrap_or_else(|e| {
            panic!(
                "{}: {e:?} {:?} {:?}",
                case.name,
                e.raw_cause(),
                e.sample_error()
            )
        });
        assert_eq!(
            report.end_reason(),
            ContinuousEndReason::Stop,
            "{}",
            case.name
        );
        assert_eq!(report.inputs()[0].committed(), expected as u64);
        assert_eq!(report.inputs()[0].total(), None);
        assert_eq!(report.bytes_written(), bytes.len() as u64);
        assert!(
            report.outputs()[0]
                .media()
                .tracks
                .iter()
                .all(|t| t.sample_count > 0)
        );
        assert_eq!(handle.state(), ContinuousState::Completed);
        handle.cancel();
        assert_eq!(handle.state(), ContinuousState::Completed);
        assert!(matches!(
            events.lock().unwrap().last(),
            Some(ContinuousEvent::State(ContinuousState::Completed))
        ));
    }
}
#[test]
fn admission_is_atomic_bounded_and_reconciles_maps() {
    let session = session(
        Arc::new(MemorySource::new()),
        ContinuousOptions::default().with_limits(ContinuousLimits::default().with_queue(2, 4096)),
    );
    let h = session.handle();
    let text = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:2,\na.m4s\n";
    assert_eq!(
        h.accept_snapshot(&id(), &snapshot(text, 1))
            .unwrap()
            .accepted(),
        1
    );
    assert_eq!(
        h.accept_snapshot(&id(), &snapshot(text, 2))
            .unwrap()
            .duplicates(),
        1
    );
    let rewritten = text.replace("a.m4s", "other.m4s");
    assert_eq!(
        h.accept_snapshot(&id(), &snapshot(&rewritten, 3))
            .unwrap_err()
            .kind(),
        ContinuousErrorKind::InputRewrite
    );
    let three = format!("{text}#EXTINF:2,\nb.m4s\n#EXTINF:2,\nc.m4s\n");
    assert_eq!(
        h.accept_snapshot(&id(), &snapshot(&three, 3))
            .unwrap_err()
            .kind(),
        ContinuousErrorKind::WouldBlock
    );
    assert_eq!(h.progress()[0].accepted(), 1);
    let two = format!("{text}#EXTINF:2,\nb.m4s\n");
    assert_eq!(
        h.accept_snapshot(&id(), &snapshot(&two, 3))
            .unwrap()
            .accepted(),
        1
    );
    assert_eq!(
        h.accept_snapshot(&id(), &snapshot(text, 2))
            .unwrap_err()
            .kind(),
        ContinuousErrorKind::RevisionRollback
    );
    h.stop();
    assert_eq!(
        h.accept_snapshot(&id(), &snapshot(text, 4))
            .unwrap_err()
            .kind(),
        ContinuousErrorKind::Closed
    );
}
#[tokio::test]
async fn capacity_limit_and_classic_outputs() {
    let case = sample::cases()
        .into_iter()
        .find(|c| c.name == "fmp4_avc_cenc")
        .unwrap();
    for format in [OutputFormat::FragmentedMp4, OutputFormat::Mp4] {
        let s = session(source(&case), ContinuousOptions::default());
        s.handle()
            .accept_snapshot(&id(), &snapshot(case.playlist, 1))
            .unwrap();
        let (bytes, report) = s.into_bytes(4 * 1024 * 1024, format).await.unwrap();
        assert_eq!(bytes.len() as u64, report.bytes_written());
        assert_eq!(report.end_reason(), ContinuousEndReason::Eof);
    }
    let s = session(source(&case), ContinuousOptions::default());
    let h = s.handle();
    h.accept_snapshot(&id(), &snapshot(case.playlist, 1))
        .unwrap();
    assert_eq!(
        s.into_bytes(100, OutputFormat::FragmentedMp4)
            .await
            .unwrap_err()
            .kind(),
        ContinuousErrorKind::BudgetExceeded
    );
    assert_eq!(h.state(), ContinuousState::Failed);
}
#[tokio::test]
async fn cancel_interrupts_empty_window_and_drop_seals_handle() {
    let s = session(Arc::new(MemorySource::new()), ContinuousOptions::default());
    let h = s.handle();
    h.accept_snapshot(&id(), &snapshot("#EXTM3U\n#EXT-X-TARGETDURATION:2\n", 1))
        .unwrap();
    let mut out = Vec::new();
    let mut run = Box::pin(s.write_to(&mut out));
    assert!(futures_util::poll!(&mut run).is_pending());
    h.cancel();
    assert_eq!(
        run.await.unwrap_err().kind(),
        ContinuousErrorKind::Cancelled
    );
    let s = session(Arc::new(MemorySource::new()), ContinuousOptions::default());
    let h = s.handle();
    drop(s);
    assert_eq!(h.state(), ContinuousState::Cancelled);
}
#[tokio::test]
async fn pause_acknowledges_boundary_and_stop_resumes_drain() {
    let case = sample::cases()
        .into_iter()
        .find(|c| c.name == "fmp4_aac_clear")
        .unwrap();
    let s = session(
        source(&case),
        ContinuousOptions::default().with_mode(ContinuousMode::Vod),
    );
    let h = s.handle();
    h.accept_snapshot(&id(), &snapshot(case.playlist, 1))
        .unwrap();
    h.pause().unwrap();
    let mut out = Vec::new();
    let mut run = Box::pin(s.write_to(&mut out));
    assert!(futures_util::poll!(&mut run).is_pending());
    h.wait_paused().await.unwrap();
    assert_eq!(h.state(), ContinuousState::Paused);
    h.stop();
    assert_eq!(run.await.unwrap().end_reason(), ContinuousEndReason::Stop);
}
#[tokio::test]
async fn native_classic_uses_no_clobber_publication() {
    let case = sample::cases()
        .into_iter()
        .find(|c| c.name == "fmp4_aac_clear")
        .unwrap();
    let path = std::env::temp_dir().join(format!("hls-continuous-{}.mp4", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let s = session(source(&case), ContinuousOptions::default());
    s.handle()
        .accept_snapshot(&id(), &snapshot(case.playlist, 1))
        .unwrap();
    let report = s
        .write_to_file(&path, FileOutputOptions::default())
        .await
        .unwrap();
    assert_eq!(
        report.bytes_written(),
        std::fs::metadata(&path).unwrap().len()
    );
    let s = session(source(&case), ContinuousOptions::default());
    s.handle()
        .accept_snapshot(&id(), &snapshot(case.playlist, 1))
        .unwrap();
    assert!(
        s.write_to_file(&path, FileOutputOptions::default())
            .await
            .is_err()
    );
    std::fs::remove_file(path).unwrap();
}
fn prefix(text: &str, count: usize, end: bool) -> String {
    let mut out = String::new();
    let mut n = 0;
    for line in open(text).lines() {
        out.push_str(line);
        out.push('\n');
        if !line.starts_with('#') && !line.is_empty() {
            n += 1;
            if n == count {
                break;
            }
        }
    }
    if end {
        out.push_str("#EXT-X-ENDLIST\n");
    }
    out
}
#[tokio::test]
async fn rolling_encrypted_snapshots_append_from_committed_callback() {
    let case = sample::cases()
        .into_iter()
        .find(|c| c.name == "fmp4_avc_cenc")
        .unwrap();
    let holder = Arc::new(Mutex::new(None::<ContinuousHandle>));
    let callbacks = holder.clone();
    let updates = Arc::new(Mutex::new(vec![
        snapshot(&prefix(case.playlist, 2, false), 2),
        snapshot(case.playlist, 3),
    ]));
    let list = updates.clone();
    let s = session(
        source(&case),
        ContinuousOptions::default()
            .with_limits(ContinuousLimits::default().with_queue(2, 4096))
            .with_on_event(Arc::new(move |event| {
                if let ContinuousEvent::Committed { .. } = event {
                    let mut list = list.lock().unwrap();
                    if !list.is_empty() {
                        let next = list.remove(0);
                        callbacks
                            .lock()
                            .unwrap()
                            .as_ref()
                            .unwrap()
                            .accept_snapshot(&id(), &next)
                            .unwrap();
                    }
                }
            })),
    );
    let h = s.handle();
    *holder.lock().unwrap() = Some(h.clone());
    h.accept_snapshot(&id(), &snapshot(&prefix(case.playlist, 1, false), 1))
        .unwrap();
    let (bytes, report) = s
        .into_bytes(4 * 1024 * 1024, OutputFormat::FragmentedMp4)
        .await
        .unwrap();
    assert_eq!(report.inputs()[0].committed(), 3);
    assert_eq!(report.end_reason(), ContinuousEndReason::Eof);
    assert_eq!(report.peaks().queued_descriptors(), 1);
    let all = session(source(&case), ContinuousOptions::default());
    all.handle()
        .accept_snapshot(&id(), &snapshot(case.playlist, 1))
        .unwrap();
    let (expected, _) = all
        .into_bytes(4 * 1024 * 1024, OutputFormat::FragmentedMp4)
        .await
        .unwrap();
    assert_eq!(sample::canonical(bytes), sample::canonical(expected));
}
struct Wait;
impl ContinuousWait for Wait {
    fn wait(
        &self,
        duration: std::time::Duration,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(tokio::time::sleep(duration))
    }
}
#[tokio::test]
async fn independent_dual_eof_preserves_audio_tail() {
    let cases = sample::cases();
    let video = cases.iter().find(|c| c.name == "fmp4_avc_cenc").unwrap();
    let audio = cases.iter().find(|c| c.name == "fmp4_aac_cbcs").unwrap();
    let aid = InputId::new("audio").unwrap();
    let s = ContinuousSession::new(
        ContinuousInputs::new(ContinuousInput::new(id(), source(video)))
            .with_audio(ContinuousInput::new(aid.clone(), source(audio))),
        sample::keys(Arc::new(sample::Provider)),
        ContinuousOptions::default().with_waiter(Arc::new(Wait), std::time::Duration::from_secs(1)),
    )
    .unwrap();
    let h = s.handle();
    h.accept_snapshot(&id(), &snapshot(video.playlist, 1))
        .unwrap();
    let a = parse_playlist_snapshot(
        &TextResource {
            location: SourceLocation::Url(url::Url::parse("https://live.test/input.m3u8").unwrap()),
            content: audio.playlist.into(),
        },
        PlaylistContext::new(aid.clone(), 1).with_revision(1),
    )
    .unwrap();
    h.accept_snapshot(&aid, &a).unwrap();
    let (_, r) = s
        .into_bytes(8 * 1024 * 1024, OutputFormat::FragmentedMp4)
        .await
        .unwrap();
    assert_eq!(
        r.inputs().iter().map(|p| p.committed()).collect::<Vec<_>>(),
        vec![3, 4]
    );
    assert_eq!(r.outputs()[0].media().tracks.len(), 2);
}
#[tokio::test]
async fn range_seek_and_duration_drain_are_distinct() {
    let case = sample::cases()
        .into_iter()
        .find(|c| c.name == "fmp4_avc_clear")
        .unwrap();
    let range = PresentationRange::new(
        MediaTime::new(1200, 1000).unwrap(),
        MediaTime::new(1800, 1000).unwrap(),
    )
    .unwrap();
    let s = session(
        source(&case),
        ContinuousOptions::default().with_range(range),
    );
    s.handle()
        .accept_snapshot(&id(), &snapshot(case.playlist, 1))
        .unwrap();
    let (_, r) = s
        .into_bytes(4 * 1024 * 1024, OutputFormat::FragmentedMp4)
        .await
        .unwrap();
    assert!(r.outputs()[0].media().tracks[0].sample_count > 0);
    let s = session(
        source(&case),
        ContinuousOptions::default().with_duration_limit(MediaTime::new(1, 1000).unwrap()),
    );
    s.handle()
        .accept_snapshot(&id(), &snapshot(case.playlist, 1))
        .unwrap();
    let (_, r) = s
        .into_bytes(4 * 1024 * 1024, OutputFormat::FragmentedMp4)
        .await
        .unwrap();
    assert_eq!(r.end_reason(), ContinuousEndReason::DurationLimit);
    assert_eq!(r.inputs()[0].committed(), 3);
}
struct FailFlush {
    writes: usize,
}
impl tokio::io::AsyncWrite for FailFlush {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        b: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.writes += 1;
        std::task::Poll::Ready(Ok(b.len()))
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if self.writes > 1 {
            std::task::Poll::Ready(Err(std::io::Error::other("flush failed")))
        } else {
            std::task::Poll::Ready(Ok(()))
        }
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        panic!("caller owns shutdown")
    }
}
#[tokio::test]
async fn failed_fragment_flush_never_commits() {
    let case = sample::cases().remove(0);
    let s = session(source(&case), ContinuousOptions::default());
    let h = s.handle();
    h.accept_snapshot(&id(), &snapshot(case.playlist, 1))
        .unwrap();
    assert_eq!(
        s.write_to(&mut FailFlush { writes: 0 })
            .await
            .unwrap_err()
            .kind(),
        ContinuousErrorKind::Output
    );
    assert_eq!(h.progress()[0].committed(), 0);
    assert_eq!(h.state(), ContinuousState::Failed);
}
#[tokio::test]
async fn empty_window_then_append_and_explicit_restart() {
    let case = sample::cases()
        .into_iter()
        .find(|c| c.name == "fmp4_aac_clear")
        .unwrap();
    let s = session(source(&case), ContinuousOptions::default());
    let h = s.handle();
    h.accept_snapshot(&id(), &snapshot("#EXTM3U\n#EXT-X-TARGETDURATION:2\n", 0))
        .unwrap();
    let mut out = Vec::new();
    let mut run = Box::pin(s.write_to(&mut out));
    assert!(futures_util::poll!(&mut run).is_pending());
    h.accept_snapshot(&id(), &snapshot(&prefix(case.playlist, 1, false), 1))
        .unwrap();
    assert!(futures_util::poll!(&mut run).is_pending());
    assert_eq!(h.progress()[0].committed(), 1);
    h.restart(&id(), 2).unwrap();
    let new = parse_playlist_snapshot(
        &TextResource {
            location: SourceLocation::Url(url::Url::parse("https://live.test/input.m3u8").unwrap()),
            content: prefix(case.playlist, 1, true),
        },
        PlaylistContext::new(id(), 2).with_revision(2),
    )
    .unwrap();
    h.accept_snapshot(&id(), &new).unwrap();
    let report = run.await.unwrap();
    assert_eq!(report.inputs()[0].committed(), 2);
    assert_eq!(report.mappings().len(), 2);
}
struct Parts(Arc<Mutex<Vec<Vec<u8>>>>);
struct PartWriter {
    parts: Arc<Mutex<Vec<Vec<u8>>>>,
    index: usize,
}
impl tokio::io::AsyncWrite for PartWriter {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        b: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.parts.lock().unwrap()[self.index].extend_from_slice(b);
        std::task::Poll::Ready(Ok(b.len()))
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        panic!("caller owns shutdown")
    }
}
impl ContinuousWriterProvider for Parts {
    type Writer = PartWriter;
    fn acquire<'a>(
        &'a mut self,
        r: ContinuousOutputRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ContinuousResult<Self::Writer>> + 'a>>
    {
        Box::pin(async move {
            let mut parts = self.0.lock().unwrap();
            assert_eq!(parts.len(), r.index() as usize);
            parts.push(Vec::new());
            Ok(PartWriter {
                parts: self.0.clone(),
                index: r.index() as usize,
            })
        })
    }
}
#[tokio::test]
async fn configuration_change_acquires_a_distinct_decodable_output() {
    let avc = include_bytes!("fixtures/sample_crypto/fmp4_avc_clear/seg1.m4s");
    let hevc = include_bytes!("fixtures/sample_crypto/fmp4_hevc_clear/seg1.m4s");
    let source = MemorySource::new()
        .segment("https://live.test/avc", avc)
        .segment("https://live.test/hevc", hevc)
        .segment(
            "https://live.test/a.init",
            include_bytes!("fixtures/sample_crypto/fmp4_avc_clear/init.mp4"),
        )
        .segment(
            "https://live.test/h.init",
            include_bytes!("fixtures/sample_crypto/fmp4_hevc_clear/init.mp4"),
        );
    let text = "#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXT-X-MAP:URI=\"a.init\"\n#EXTINF:2,\navc\n#EXT-X-DISCONTINUITY\n#EXT-X-MAP:URI=\"h.init\"\n#EXTINF:2,\nhevc\n#EXT-X-ENDLIST\n";
    let s = session(
        Arc::new(source),
        ContinuousOptions::default().with_change_policy(TimelineChangePolicy::Split),
    );
    s.handle()
        .accept_snapshot(&id(), &snapshot(text, 1))
        .unwrap();
    let parts = Arc::new(Mutex::new(Vec::new()));
    let report = s.write_to_outputs(&mut Parts(parts.clone())).await.unwrap();
    assert_eq!(parts.lock().unwrap().len(), 2);
    assert_eq!(report.outputs().len(), 2);
    assert_eq!(report.outputs()[0].media().tracks[0].codec, Codec::Avc);
    assert_eq!(report.outputs()[1].media().tracks[0].codec, Codec::Hevc);
}
#[tokio::test]
async fn slow_writer_blocks_acceptance_and_cancel_wins_stop() {
    struct Block;
    impl tokio::io::AsyncWrite for Block {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            _: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::task::Poll::Pending
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Pending
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            panic!("caller owns shutdown")
        }
    }
    let case = sample::cases().remove(0);
    let s = session(source(&case), ContinuousOptions::default());
    let h = s.handle();
    h.accept_snapshot(&id(), &snapshot(&prefix(case.playlist, 1, false), 1))
        .unwrap();
    let mut writer = Block;
    let mut run = Box::pin(s.write_to(&mut writer));
    assert!(futures_util::poll!(&mut run).is_pending());
    assert_eq!(
        h.accept_snapshot(&id(), &snapshot(&prefix(case.playlist, 2, false), 2))
            .unwrap_err()
            .kind(),
        ContinuousErrorKind::WouldBlock
    );
    h.stop();
    h.cancel();
    assert_eq!(
        run.await.unwrap_err().kind(),
        ContinuousErrorKind::Cancelled
    );
    assert_eq!(h.progress()[0].committed(), 0);
}
#[tokio::test]
async fn long_epoch_history_is_bounded() {
    for count in [8, 64, 256] {
        let source = MemorySource::new()
            .segment(
                "https://live.test/init",
                include_bytes!("fixtures/sample_crypto/fmp4_aac_clear/init.mp4"),
            )
            .segment(
                "https://live.test/media",
                include_bytes!("fixtures/sample_crypto/fmp4_aac_clear/seg1.m4s"),
            );
        let hbox = Arc::new(Mutex::new(None::<ContinuousHandle>));
        let callback = hbox.clone();
        let options=ContinuousOptions::default().with_limits(ContinuousLimits::default().with_history(4).with_queue(2,4096)).with_on_event(Arc::new(move|event| {
            if let ContinuousEvent::Committed {input,..}=event {
                let done=input.committed();let h=callback.lock().unwrap();let h=h.as_ref().unwrap();
                if done==count {h.stop();return;}
                let text=format!("#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:{}\n#EXT-X-DISCONTINUITY-SEQUENCE:{}\n#EXT-X-MAP:URI=\"init\"\n#EXTINF:2,\nmedia\n#EXT-X-DISCONTINUITY\n#EXTINF:2,\nmedia\n",done-1,done-1);
                h.accept_snapshot(&id(),&snapshot(&text,done+1)).unwrap();
            }
        }));
        let s = session(Arc::new(source), options);
        let h = s.handle();
        *hbox.lock().unwrap() = Some(h.clone());
        h.accept_snapshot(
            &id(),
            &snapshot(
                "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"init\"\n#EXTINF:2,\nmedia\n",
                1,
            ),
        )
        .unwrap();
        let r = s.write_to(&mut tokio::io::sink()).await.unwrap();
        assert_eq!(r.inputs()[0].committed(), count);
        assert_eq!(r.mappings().len(), 4);
        assert!(r.history_truncated());
        assert_eq!(r.peaks().queued_descriptors(), 1);
    }
}
#[allow(dead_code)]
#[path = "support/crypto_corpus.rs"]
mod aes;
#[tokio::test]
async fn aes128_rotations_and_clear_transitions_cover_ts_and_fmp4() {
    for name in [
        "ts_avc_regular",
        "ts_hevc_regular",
        "ts_aac_audio_only",
        "fmp4_avc_regular",
        "fmp4_hevc_regular",
        "fmp4_aac_audio_only",
    ] {
        let folder = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/crypto")
            .join(name);
        let mut media = MemorySource::new();
        for file in std::fs::read_dir(&folder).unwrap() {
            let file = file.unwrap();
            if file.path().is_file() {
                media = media.segment(
                    format!("https://live.test/{}", file.file_name().to_str().unwrap()),
                    std::fs::read(file.path()).unwrap(),
                );
            }
        }
        let extension = if name.starts_with("fmp4") {
            "m4s"
        } else {
            "ts"
        };
        media = media.segment(
            "https://live.test/clear.bin",
            std::fs::read(
                folder
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("media")
                    .join(name)
                    .join(format!("seg2.{extension}")),
            )
            .unwrap(),
        );
        let media: Arc<dyn Source> = Arc::new(media);
        let text = std::fs::read_to_string(folder.join("input.m3u8")).unwrap();
        for (format, label) in [
            (OutputFormat::FragmentedMp4, "fragmented"),
            (OutputFormat::Mp4, "classic"),
        ] {
            let s = ContinuousSession::new(
                ContinuousInputs::new(ContinuousInput::new(id(), media.clone())),
                sample::keys(Arc::new(aes::Provider)),
                ContinuousOptions::default(),
            )
            .unwrap();
            let h = s.handle();
            h.accept_snapshot(&id(), &snapshot(&open(&text), 1))
                .unwrap();
            h.stop();
            let (bytes, r) = s.into_bytes(8 * 1024 * 1024, format).await.unwrap();
            assert_eq!(r.inputs()[0].committed(), 3);
            if let Ok(dir) = std::env::var("HLS_CONTINUOUS_OUTPUT") {
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(
                    std::path::Path::new(&dir).join(format!("aes-{name}-{label}.mp4")),
                    bytes,
                )
                .unwrap();
            }
        }
    }
}
#[cfg(feature = "serde")]
#[tokio::test]
async fn continuous_reports_keep_wide_numbers_lossless() {
    let case = sample::cases().remove(0);
    let s = session(source(&case), ContinuousOptions::default());
    s.handle()
        .accept_snapshot(&id(), &snapshot(case.playlist, u64::MAX))
        .unwrap();
    let (_, r) = s
        .into_bytes(4 * 1024 * 1024, OutputFormat::FragmentedMp4)
        .await
        .unwrap();
    let value = serde_json::to_value(r).unwrap();
    assert!(value["bytes_written"].is_string());
    assert!(value["inputs"][0]["committed"].is_string());
    assert!(value["mappings"][0]["generation"].is_string());
    assert!(value["duration"]["ticks"].is_string());
}
#[tokio::test]
async fn explicit_gaps_preserve_collapse_or_split_and_default_fails() {
    for (policy, missing, expected) in [
        (GapPolicy::Preserve, MissingSegmentPolicy::Skip, 6_000),
        (GapPolicy::Collapse, MissingSegmentPolicy::Skip, 4_000),
        (GapPolicy::Preserve, MissingSegmentPolicy::Split, 6_000),
    ] {
        let source = MemorySource::new()
            .segment(
                "https://live.test/init",
                include_bytes!("fixtures/media/fmp4_avc_video_only/init.fmp4"),
            )
            .segment(
                "https://live.test/seg0",
                include_bytes!("fixtures/media/fmp4_avc_video_only/seg0.m4s"),
            )
            .segment(
                "https://live.test/seg2",
                include_bytes!("fixtures/media/fmp4_avc_video_only/seg2.m4s"),
            );
        let text = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"init\"\n#EXTINF:2,\nseg0\n#EXT-X-GAP\n#EXTINF:2,\nmissing\n#EXTINF:2,\nseg2\n#EXT-X-ENDLIST\n";
        let s = session(
            Arc::new(source),
            ContinuousOptions::default()
                .with_gap_policy(policy)
                .with_missing_segments(missing),
        );
        s.handle()
            .accept_snapshot(&id(), &snapshot(text, 1))
            .unwrap();
        let parts = Arc::new(Mutex::new(Vec::new()));
        let r = s.write_to_outputs(&mut Parts(parts.clone())).await.unwrap();
        assert_eq!(r.gap_count(), 1);
        assert_eq!(r.inputs()[0].committed(), 3);
        assert_eq!(
            r.duration().ticks() * 1000 / i128::from(r.duration().timescale()),
            expected
        );
        assert_eq!(
            r.outputs().len(),
            if missing == MissingSegmentPolicy::Split {
                2
            } else {
                1
            }
        );
    }
}
#[tokio::test]
async fn stop_before_any_media_is_a_successful_empty_recording() {
    let s = session(Arc::new(MemorySource::new()), ContinuousOptions::default());
    s.handle().stop();
    let mut out = Vec::new();
    let r = s.write_to(&mut out).await.unwrap();
    assert_eq!(r.end_reason(), ContinuousEndReason::Stop);
    assert!(out.is_empty());
    assert!(r.actual_range().is_none());
}

#[tokio::test]
async fn cancel_interrupts_key_wait_and_capacity_wait() {
    use hls_engine::legacy::crypto::key::*;
    struct Pending;
    impl KeyProvider for Pending {
        fn resolve(&self, _: KeyRequest) -> KeyFuture<KeyResolution> {
            Box::pin(std::future::pending())
        }
    }
    let case = sample::cases()
        .into_iter()
        .find(|c| c.name == "fmp4_aac_cenc")
        .unwrap();
    let s = ContinuousSession::new(
        ContinuousInputs::new(ContinuousInput::new(id(), source(&case))),
        sample::keys(Arc::new(Pending)),
        ContinuousOptions::default().with_limits(ContinuousLimits::default().with_queue(1, 4096)),
    )
    .unwrap();
    let h = s.handle();
    h.accept_snapshot(&id(), &snapshot(&prefix(case.playlist, 1, false), 1))
        .unwrap();
    let update = snapshot(&prefix(case.playlist, 2, false), 2);
    let input_id = id();
    let mut capacity = Box::pin(h.accept_when_ready(&input_id, &update));
    assert!(futures_util::poll!(&mut capacity).is_pending());
    let mut bytes = Vec::new();
    let mut run = Box::pin(s.write_to(&mut bytes));
    assert!(futures_util::poll!(&mut run).is_pending());
    h.cancel();
    assert_eq!(
        run.await.unwrap_err().kind(),
        ContinuousErrorKind::Cancelled
    );
    assert_eq!(
        capacity.await.unwrap_err().kind(),
        ContinuousErrorKind::Cancelled
    );
    assert_eq!(h.progress()[0].committed(), 0);
}

#[tokio::test]
async fn host_timeout_can_reenter_handle_without_a_lock_and_cancel_acquisition() {
    struct Immediate(Arc<Mutex<Option<ContinuousHandle>>>);
    impl ContinuousWait for Immediate {
        fn wait(
            &self,
            _: std::time::Duration,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
            let h = self.0.lock().unwrap();
            assert_eq!(h.as_ref().unwrap().state(), ContinuousState::Running);
            Box::pin(async {})
        }
    }
    let case = sample::cases()
        .into_iter()
        .find(|c| c.name == "fmp4_avc_clear")
        .unwrap();
    let aid = InputId::new("audio").unwrap();
    let holder = Arc::new(Mutex::new(None));
    let s = ContinuousSession::new(
        ContinuousInputs::new(ContinuousInput::new(id(), source(&case)))
            .with_audio(ContinuousInput::new(aid, source(&case))),
        sample::keys(Arc::new(sample::Provider)),
        ContinuousOptions::default().with_waiter(
            Arc::new(Immediate(holder.clone())),
            std::time::Duration::from_secs(1),
        ),
    )
    .unwrap();
    let h = s.handle();
    *holder.lock().unwrap() = Some(h.clone());
    h.accept_snapshot(&id(), &snapshot(&open(case.playlist), 1))
        .unwrap();
    assert_eq!(
        s.write_to(&mut Vec::new()).await.unwrap_err().kind(),
        ContinuousErrorKind::SkewTimeout
    );
    struct Never;
    impl ContinuousWriterProvider for Never {
        type Writer = Vec<u8>;
        fn acquire<'a>(
            &'a mut self,
            _: ContinuousOutputRequest,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ContinuousResult<Vec<u8>>> + 'a>>
        {
            Box::pin(std::future::pending())
        }
    }
    let s = session(source(&case), ContinuousOptions::default());
    let h = s.handle();
    h.accept_snapshot(&id(), &snapshot(case.playlist, 1))
        .unwrap();
    let mut provider = Never;
    let mut run = Box::pin(s.write_to_outputs(&mut provider));
    assert!(futures_util::poll!(&mut run).is_pending());
    h.cancel();
    assert_eq!(
        run.await.unwrap_err().kind(),
        ContinuousErrorKind::Cancelled
    );
}

#[tokio::test]
async fn final_flush_cancel_wins_and_never_closes_borrowed_writer() {
    use std::{
        pin::Pin,
        sync::atomic::{AtomicBool, Ordering},
        task::{Context, Poll},
    };
    let finalizing = Arc::new(AtomicBool::new(false));
    let observed = finalizing.clone();
    struct Writer(Arc<AtomicBool>);
    impl tokio::io::AsyncWrite for Writer {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Poll::Ready(Ok(bytes.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            if self.0.load(Ordering::Acquire) {
                Poll::Pending
            } else {
                Poll::Ready(Ok(()))
            }
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            panic!("borrowed shutdown");
        }
    }
    let case = sample::cases().remove(0);
    let s = session(
        source(&case),
        ContinuousOptions::default().with_on_event(Arc::new(move |e| {
            if matches!(e, ContinuousEvent::State(ContinuousState::Finalizing)) {
                observed.store(true, Ordering::Release);
            }
        })),
    );
    let h = s.handle();
    h.accept_snapshot(&id(), &snapshot(case.playlist, 1))
        .unwrap();
    let mut sink = Writer(finalizing.clone());
    let mut run = Box::pin(s.write_to(&mut sink));
    assert!(futures_util::poll!(&mut run).is_pending());
    assert!(finalizing.load(Ordering::Acquire));
    assert_eq!(h.progress()[0].committed(), 4);
    h.cancel();
    assert_eq!(
        run.await.unwrap_err().kind(),
        ContinuousErrorKind::Cancelled
    );
    assert_eq!(h.state(), ContinuousState::Cancelled);
}

#[test]
fn event_prefix_eviction_key_redeclaration_and_window_jump_are_explicit() {
    let s = session(
        Arc::new(MemorySource::new()),
        ContinuousOptions::default().with_limits(ContinuousLimits::default().with_history(1)),
    );
    let h = s.handle();
    let base = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-PLAYLIST-TYPE:EVENT\n#EXT-X-KEY:METHOD=AES-128,URI=\"key\"\n#EXTINF:2,\na.ts\n";
    h.accept_snapshot(&id(), &snapshot(base, 1)).unwrap();
    let next = format!("{base}#EXT-X-KEY:METHOD=AES-128,URI=\"key\"\n#EXTINF:2,\nb.ts\n");
    assert_eq!(
        h.accept_snapshot(&id(), &snapshot(&next, 2))
            .unwrap()
            .accepted(),
        1
    );
    assert_eq!(
        h.accept_snapshot(&id(), &snapshot(&next, 3))
            .unwrap()
            .duplicates(),
        2
    );
    let jump = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:3\n#EXTINF:2,\nc.ts\n";
    assert_eq!(
        h.accept_snapshot(&id(), &snapshot(jump, 4))
            .unwrap_err()
            .kind(),
        ContinuousErrorKind::MissingSegment
    );
    let contiguous = jump
        .replace("SEQUENCE:3", "SEQUENCE:2")
        .replace("#EXTINF", "#EXT-X-KEY:METHOD=AES-128,URI=\"key\"\n#EXTINF");
    assert_eq!(
        h.accept_snapshot(&id(), &snapshot(&contiguous, 4))
            .unwrap_err()
            .kind(),
        ContinuousErrorKind::NeedsReconciliation
    );
    assert_eq!(
        h.pause().unwrap_err().kind(),
        ContinuousErrorKind::PauseUnsupported
    );
}

#[tokio::test]
async fn native_continuous_finalize_backends() {
    let backends = vec![FinalizeBackend::Native];
    #[cfg(feature = "ffmpeg-finalize")]
    let backends = {
        let mut b = backends;
        b.push(FinalizeBackend::Ffmpeg);
        b
    };
    for name in ["fmp4_hevc_cbcs", "fmp4_avc_cenc", "ts_aac_sample"] {
        let case = sample::cases()
            .into_iter()
            .find(|c| c.name == name)
            .unwrap();
        for backend in &backends {
            let filename = format!("native-{name}-{backend:?}.mp4");
            let dir = std::env::var("HLS_CONTINUOUS_OUTPUT")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|_| {
                    std::env::temp_dir()
                        .join(format!("hls-continuous-finalize-{}", std::process::id()))
                });
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(filename);
            let _ = std::fs::remove_file(&path);
            let s = session(source(&case), ContinuousOptions::default());
            let h = s.handle();
            h.accept_snapshot(&id(), &snapshot(&open(case.playlist), 1))
                .unwrap();
            h.stop();
            let report = s
                .write_to_file(
                    &path,
                    FileOutputOptions::default().with_finalize_backend(*backend),
                )
                .await
                .unwrap();
            assert!(report.outputs()[0].classic_index_samples() > 0);
            assert_eq!(report.outputs()[0].collected_bytes(), 0);
            assert_eq!(
                report.bytes_written(),
                std::fs::metadata(&path).unwrap().len()
            );
            if std::env::var_os("HLS_CONTINUOUS_OUTPUT").is_none() {
                std::fs::remove_file(path).unwrap();
            }
        }
    }
}
