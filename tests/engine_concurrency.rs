#![cfg(not(target_arch = "wasm32"))]
use hls_engine::{crypto::resource::ResourceOptions, playlist::*, *};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio::io::AsyncWrite;
#[allow(dead_code)]
#[path = "support/sample_crypto.rs"]
mod sample;

const CAP: u64 = 1024 * 1024;
#[derive(Debug, Default)]
struct Observed {
    active: AtomicUsize,
    peak: AtomicUsize,
    started: Mutex<Vec<String>>,
    finished: Mutex<Vec<String>>,
    changed: tokio::sync::Notify,
}
struct Active(Arc<Observed>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}
#[derive(Debug, Clone)]
struct Probe {
    memory: MemorySource,
    observed: Arc<Observed>,
    stall: bool,
    fail: Option<String>,
}
impl Source for Probe {
    fn create_session_with_options(
        &self,
        options: &SourceSessionOptions,
    ) -> Option<Arc<dyn Source>> {
        assert!(options.demand_driven());
        assert!(options.max_resource_bytes().is_some());
        Some(Arc::new(self.clone()))
    }
    fn read_text<'a>(
        &'a self,
        location: &'a SourceLocation,
    ) -> Pin<Box<dyn Future<Output = SourceResult<TextResource>> + Send + 'a>> {
        self.memory.read_text(location)
    }
    fn read_bytes<'a>(
        &'a self,
        location: &'a SourceLocation,
        range: Option<&'a ByteRange>,
    ) -> Pin<Box<dyn Future<Output = SourceResult<Vec<u8>>> + Send + 'a>> {
        Box::pin(async move {
            let SourceLocation::Url(url) = location else {
                unreachable!()
            };
            let name = url.path().to_string();
            let active = self.observed.active.fetch_add(1, Ordering::SeqCst) + 1;
            let _guard = Active(self.observed.clone());
            self.observed.peak.fetch_max(active, Ordering::SeqCst);
            self.observed.started.lock().unwrap().push(name.clone());
            self.observed.changed.notify_one();
            if self.stall {
                std::future::pending::<()>().await;
            }
            // The first TS segment finishes after its successors.
            tokio::time::sleep(Duration::from_millis(if name == "/seg0.ts" {
                40
            } else {
                2
            }))
            .await;
            if self.fail.as_ref() == Some(&name) {
                return Err(Error::invalid("injected read failure"));
            }
            let bytes = self.memory.read_bytes(location, range).await?;
            self.observed.finished.lock().unwrap().push(name);
            Ok(bytes)
        })
    }
}
fn fixture() -> (Probe, PlaylistSnapshot) {
    let base = url::Url::parse("https://probe.test/input.m3u8").unwrap();
    let snapshot = parse_playlist_snapshot(
        &TextResource {
            location: SourceLocation::Url(base.clone()),
            content: include_str!("fixtures/multi_input/ts/input.m3u8").into(),
        },
        PlaylistContext::new(InputId::new("primary").unwrap(), 0),
    )
    .unwrap();
    let mut memory = MemorySource::new();
    for index in 0..6 {
        let name = format!("seg{index}.ts");
        let bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/multi_input/ts")
                .join(&name),
        )
        .unwrap();
        memory = memory.segment(base.join(&name).unwrap().to_string(), bytes);
    }
    (
        Probe {
            memory,
            observed: Arc::new(Observed::default()),
            stall: false,
            fail: None,
        },
        snapshot,
    )
}
fn options(concurrency: usize, buffers: u64) -> EngineOptions {
    EngineOptions::default()
        .with_download_concurrency(concurrency)
        .with_limits(EngineLimits::default().with_download_buffer_bytes(CAP * buffers))
        .with_resources(ResourceOptions::default().with_limits(CAP, CAP * 2, 2))
}
fn session(
    source: Arc<dyn Source>,
    snapshot: &PlaylistSnapshot,
    options: EngineOptions,
    checkpoint: Option<EngineCheckpoint>,
) -> EngineSession {
    let id = InputId::new("primary").unwrap();
    let inputs = EngineInputs::new(EngineInput::new(id.clone(), source), EmbeddedAudio::Keep);
    let keys = sample::keys(Arc::new(sample::Provider));
    let session = match checkpoint {
        Some(cp) => EngineSession::restore(inputs, keys, options, cp),
        None => EngineSession::new(inputs, keys, options),
    }
    .unwrap();
    session.handle().accept_snapshot(&id, snapshot).unwrap();
    session.handle().end_input(&id).unwrap();
    session
}
async fn output(session: EngineSession) -> Vec<u8> {
    session
        .into_bytes(8 * 1024 * 1024, OutputFormat::FragmentedMp4)
        .await
        .unwrap()
        .0
}
async fn wait_started(observed: &Observed, count: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let wake = observed.changed.notified();
            if observed.started.lock().unwrap().len() >= count {
                return;
            }
            wake.await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn defaults_download_four_segments_concurrently() {
    let (probe, snapshot) = fixture();
    let expected = output(session(Arc::new(probe), &snapshot, options(1, 1), None)).await;
    let (probe, snapshot) = fixture();
    let observed = probe.observed.clone();
    let defaults = EngineOptions::default();
    assert_eq!(defaults.download_concurrency(), 4);
    assert_eq!(defaults.limits().download_buffer_bytes(), 64 * 1024 * 1024);
    let bytes = output(session(Arc::new(probe), &snapshot, defaults, None)).await;
    assert_eq!(observed.peak.load(Ordering::SeqCst), 4);
    assert_eq!(observed.started.lock().unwrap().len(), 6);
    assert_eq!(observed.active.load(Ordering::SeqCst), 0);
    assert_eq!(sample::canonical(bytes), sample::canonical(expected));
}

#[tokio::test]
async fn downloads_overlap_but_output_and_commits_keep_order() {
    let (probe, snapshot) = fixture();
    let expected = output(session(Arc::new(probe), &snapshot, options(1, 1), None)).await;
    for (concurrency, buffers, peak) in [(2, 2, 2), (4, 4, 4), (8, 2, 2), (4, 1, 1)] {
        let (probe, snapshot) = fixture();
        let observed = probe.observed.clone();
        let commits = Arc::new(Mutex::new(Vec::new()));
        let log = commits.clone();
        let options = options(concurrency, buffers).with_on_event(Arc::new(move |event| {
            if let EngineEvent::Committed { input, .. } = event {
                log.lock().unwrap().push(input.committed());
            }
        }));
        let bytes = output(session(Arc::new(probe), &snapshot, options, None)).await;
        assert_eq!(observed.peak.load(Ordering::SeqCst), peak);
        assert_eq!(observed.active.load(Ordering::SeqCst), 0);
        assert_eq!(observed.started.lock().unwrap().len(), 6);
        if peak > 1 {
            assert_ne!(observed.finished.lock().unwrap()[0], "/seg0.ts");
        }
        assert_eq!(*commits.lock().unwrap(), vec![1, 2, 3, 4, 5, 6]);
        assert_eq!(
            sample::canonical(bytes),
            sample::canonical(expected.clone())
        );
    }
}
#[tokio::test]
async fn cancellation_and_dropping_writer_release_all_downloads() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for cancel in [true, false] {
                let (mut probe, snapshot) = fixture();
                probe.stall = true;
                let observed = probe.observed.clone();
                let session = session(Arc::new(probe), &snapshot, options(4, 4), None);
                let handle = session.handle();
                let task = tokio::task::spawn_local(async move {
                    session
                        .into_bytes(8 * 1024 * 1024, OutputFormat::FragmentedMp4)
                        .await
                });
                wait_started(&observed, 4).await;
                assert_eq!(observed.active.load(Ordering::SeqCst), 4);
                if cancel {
                    handle.cancel();
                    let error = tokio::time::timeout(Duration::from_secs(5), task)
                        .await
                        .unwrap()
                        .unwrap()
                        .unwrap_err();
                    assert_eq!(error.kind(), EngineErrorKind::Cancelled);
                } else {
                    task.abort();
                    assert!(task.await.unwrap_err().is_cancelled());
                }
                assert_eq!(observed.active.load(Ordering::SeqCst), 0);
                assert_eq!(observed.started.lock().unwrap().len(), 4);
            }
        })
        .await;
}
struct BlockedWriter(Arc<tokio::sync::Notify>);
impl AsyncWrite for BlockedWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.0.notify_one();
        Poll::Pending
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
#[tokio::test]
async fn blocked_output_does_not_refill_the_download_window() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (probe, snapshot) = fixture();
            let observed = probe.observed.clone();
            let session = session(Arc::new(probe), &snapshot, options(3, 3), None);
            let handle = session.handle();
            let blocked = Arc::new(tokio::sync::Notify::new());
            let signal = blocked.clone();
            let task = tokio::task::spawn_local(async move {
                session.write_to(&mut BlockedWriter(signal)).await
            });
            tokio::time::timeout(Duration::from_secs(5), blocked.notified())
                .await
                .unwrap();
            let before = observed.started.lock().unwrap().len();
            tokio::time::sleep(Duration::from_millis(80)).await;
            assert_eq!(observed.started.lock().unwrap().len(), before);
            assert_eq!(before, 3);
            handle.cancel();
            assert_eq!(
                task.await.unwrap().unwrap_err().kind(),
                EngineErrorKind::Cancelled
            );
            assert_eq!(observed.active.load(Ordering::SeqCst), 0);
        })
        .await;
}
#[tokio::test]
async fn future_download_error_is_reported_at_its_consumption_position() {
    let (mut probe, snapshot) = fixture();
    probe.fail = Some("/seg4.ts".into());
    let observed = probe.observed.clone();
    let session = session(Arc::new(probe), &snapshot, options(6, 6), None);
    let handle = session.handle();
    let error = session
        .into_bytes(8 * 1024 * 1024, OutputFormat::FragmentedMp4)
        .await
        .unwrap_err();
    assert_eq!(error.slot().unwrap(), snapshot.segments()[4].slot());
    assert!(handle.progress()[0].committed() > 0);
    assert_eq!(observed.active.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn encrypted_profiles_have_identical_serial_and_parallel_output() {
    for case in sample::cases() {
        let id = InputId::new("primary").unwrap();
        let base = url::Url::parse("https://probe.test/input.m3u8").unwrap();
        let snapshot = parse_playlist_snapshot(
            &TextResource {
                location: SourceLocation::Url(base.clone()),
                content: case.playlist.into(),
            },
            PlaylistContext::new(id, 0),
        )
        .unwrap();
        let mut memory = MemorySource::new();
        for (name, bytes) in case.files {
            memory = memory.segment(base.join(name).unwrap().to_string(), *bytes);
        }
        let source = Arc::new(memory);
        let serial = output(session(source.clone(), &snapshot, options(1, 1), None)).await;
        let parallel = output(session(source, &snapshot, options(4, 4), None)).await;
        assert_eq!(
            sample::canonical(serial),
            sample::canonical(parallel),
            "{}",
            case.name
        );
    }
}
#[tokio::test]
async fn checkpoint_replays_prefetched_uncommitted_segments() {
    let (probe, snapshot) = fixture();
    let source = Arc::new(probe);
    let expected = output(session(source.clone(), &snapshot, options(1, 1), None)).await;
    let root = std::env::temp_dir().join(format!("hls-concurrent-recovery-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("output.mp4");
    let latest = Arc::new(Mutex::new(None));
    let capture = latest.clone();
    let recovery = RecoveryOptions::new(Arc::new(move |cp| {
        if cp.bytes_written() > 0 {
            *capture.lock().unwrap() = Some(cp);
            return Err(EngineError::output(
                std::io::Error::other("interrupted checkpoint").into(),
            ));
        }
        Ok(())
    }));
    assert!(
        session(source.clone(), &snapshot, options(4, 4), None)
            .write_recoverable_to_file(&path, recovery)
            .await
            .is_err()
    );
    assert_eq!(source.observed.active.load(Ordering::SeqCst), 0);
    let checkpoint =
        EngineCheckpoint::from_bytes(&latest.lock().unwrap().take().unwrap().to_bytes()).unwrap();
    // Download concurrency is operational and may change across recovery.
    let report = session(source, &snapshot, options(2, 2), Some(checkpoint))
        .write_recoverable_to_file(&path, RecoveryOptions::new(Arc::new(|_| Ok(()))))
        .await
        .unwrap();
    assert_eq!(report.media().inputs()[0].committed(), 6);
    assert_eq!(
        sample::canonical(std::fs::read(&path).unwrap()),
        sample::canonical(expected)
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn pause_keeps_the_window_bounded_and_resume_finishes() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (probe, snapshot) = fixture();
            let observed = probe.observed.clone();
            let session = session(
                Arc::new(probe),
                &snapshot,
                options(3, 3).with_mode(EngineMode::Vod),
                None,
            );
            let handle = session.handle();
            let task = tokio::task::spawn_local(output(session));
            wait_started(&observed, 3).await;
            handle.pause().unwrap();
            tokio::time::timeout(Duration::from_secs(5), handle.wait_paused())
                .await
                .unwrap()
                .unwrap();
            let started = observed.started.lock().unwrap().len();
            tokio::time::sleep(Duration::from_millis(80)).await;
            assert_eq!(observed.started.lock().unwrap().len(), started);
            handle.resume().unwrap();
            assert!(
                !tokio::time::timeout(Duration::from_secs(5), task)
                    .await
                    .unwrap()
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(handle.progress()[0].committed(), 6);
            assert_eq!(observed.active.load(Ordering::SeqCst), 0);
        })
        .await;
}

struct Wait;
impl EngineWait for Wait {
    fn wait(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(tokio::time::sleep(duration))
    }
}
#[tokio::test]
async fn multiple_encrypted_inputs_share_the_download_window() {
    async fn run(concurrency: usize) -> Vec<u8> {
        let (video, video_snapshot) = sample::input("fmp4_avc_cenc", "video");
        let (audio, audio_snapshot) = sample::input("fmp4_aac_cbcs", "audio");
        let inputs = EngineInputs::new(video, EmbeddedAudio::Exclude)
            .with_audio(audio, TrackMetadata::new("en", "English"));
        let session = EngineSession::new(
            inputs,
            sample::keys(Arc::new(sample::Provider)),
            options(concurrency, 2).with_waiter(Arc::new(Wait), Duration::from_secs(5)),
        )
        .unwrap();
        session
            .handle()
            .accept_snapshot(&InputId::new("video").unwrap(), &video_snapshot)
            .unwrap();
        session
            .handle()
            .accept_snapshot(&InputId::new("audio").unwrap(), &audio_snapshot)
            .unwrap();
        output(session).await
    }
    assert_eq!(
        sample::canonical(run(1).await),
        sample::canonical(run(4).await)
    );
}

#[tokio::test]
async fn ranges_on_one_url_have_distinct_buffers_and_exact_reservations() {
    let (probe, snapshot) = fixture();
    let expected = output(session(
        Arc::new(probe.clone()),
        &snapshot,
        options(1, 1),
        None,
    ))
    .await;
    let mut playlist = include_str!("fixtures/multi_input/ts/input.m3u8").to_string();
    let mut bundle = Vec::new();
    let mut sizes = Vec::new();
    for index in 0..6 {
        let name = format!("seg{index}.ts");
        let bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/multi_input/ts")
                .join(&name),
        )
        .unwrap();
        playlist = playlist.replace(
            &name,
            &format!(
                "#EXT-X-BYTERANGE:{}@{}\nbundle.ts",
                bytes.len(),
                bundle.len()
            ),
        );
        sizes.push(bytes.len() as u64);
        bundle.extend(bytes);
    }
    let snapshot = parse_playlist_snapshot(
        &TextResource {
            location: SourceLocation::Url("https://probe.test/input.m3u8".parse().unwrap()),
            content: playlist,
        },
        PlaylistContext::new(InputId::new("primary").unwrap(), 0),
    )
    .unwrap();
    let probe = Probe {
        memory: MemorySource::new().segment("https://probe.test/bundle.ts", bundle),
        observed: Arc::new(Observed::default()),
        stall: false,
        fail: None,
    };
    let observed = probe.observed.clone();
    let resource_cap = *sizes.iter().max().unwrap();
    let window = sizes.iter().take(3).sum();
    let options = EngineOptions::default()
        .with_download_concurrency(4)
        .with_limits(EngineLimits::default().with_download_buffer_bytes(window))
        .with_resources(ResourceOptions::default().with_limits(resource_cap, resource_cap * 2, 2));
    let bytes = output(session(Arc::new(probe), &snapshot, options, None)).await;
    assert!(observed.peak.load(Ordering::SeqCst) > 1);
    assert!(observed.peak.load(Ordering::SeqCst) <= 4);
    assert_eq!(observed.started.lock().unwrap().len(), 6);
    assert_eq!(sample::canonical(bytes), sample::canonical(expected));
}

#[cfg(feature = "default-source")]
#[tokio::test]
async fn reqwest_engine_downloads_overlap_without_source_prefetch() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = url::Url::parse(&format!(
        "http://{}/input.m3u8",
        listener.local_addr().unwrap()
    ))
    .unwrap();
    let observed = Arc::new(Observed::default());
    let log = observed.clone();
    let server = tokio::spawn(async move {
        let mut workers = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (mut socket, _) = accepted.unwrap();
                    let log = log.clone();
                    workers.spawn(async move {
                        let mut request = Vec::new();
                        while !request.windows(4).any(|b| b == b"\r\n\r\n") {
                            let mut chunk = [0; 1024];
                            let n = socket.read(&mut chunk).await.unwrap();
                            if n == 0 { return; }
                            request.extend_from_slice(&chunk[..n]);
                        }
                        let text = String::from_utf8(request).unwrap();
                        let path = text.split_whitespace().nth(1).unwrap();
                        assert!((0..6).any(|i| path == format!("/seg{i}.ts")));
                        let active = log.active.fetch_add(1, Ordering::SeqCst) + 1;
                        let _guard = Active(log.clone());
                        log.peak.fetch_max(active, Ordering::SeqCst);
                        log.started.lock().unwrap().push(path.into());
                        tokio::time::sleep(Duration::from_millis(30)).await;
                        let bytes = std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/multi_input/ts").join(&path[1..])).unwrap();
                        let headers = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len());
                        socket.write_all(headers.as_bytes()).await.unwrap();
                        socket.write_all(&bytes).await.unwrap();
                    });
                }
                Some(result) = workers.join_next(), if !workers.is_empty() => { result.unwrap(); }
            }
        }
    });
    let snapshot = parse_playlist_snapshot(
        &TextResource {
            location: SourceLocation::Url(base),
            content: include_str!("fixtures/multi_input/ts/input.m3u8").into(),
        },
        PlaylistContext::new(InputId::new("primary").unwrap(), 0),
    )
    .unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let source = Arc::new(ReqwestSource::with_client(client));
    let result = output(session(source, &snapshot, options(3, 3), None)).await;
    server.abort();
    let _ = server.await;
    assert!(!result.is_empty());
    assert_eq!(observed.peak.load(Ordering::SeqCst), 3);
    assert_eq!(observed.started.lock().unwrap().len(), 6);
}

#[tokio::test]
async fn encrypted_checkpoint_recovery_preserves_order_and_key_state() {
    for name in ["ts_avc_sample", "fmp4_avc_cenc", "fmp4_aac_cbcs"] {
        let case = sample::cases()
            .into_iter()
            .find(|case| case.name == name)
            .unwrap();
        let base = url::Url::parse("https://probe.test/input.m3u8").unwrap();
        let snapshot = parse_playlist_snapshot(
            &TextResource {
                location: SourceLocation::Url(base.clone()),
                content: case.playlist.into(),
            },
            PlaylistContext::new(InputId::new("primary").unwrap(), 0),
        )
        .unwrap();
        let mut memory = MemorySource::new();
        for (file, bytes) in case.files {
            memory = memory.segment(base.join(file).unwrap().to_string(), *bytes);
        }
        let source = Arc::new(memory);
        let expected = output(session(source.clone(), &snapshot, options(1, 1), None)).await;
        let root = std::env::temp_dir().join(format!(
            "hls-parallel-encrypted-{}-{name}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("output.mp4");
        let latest = Arc::new(Mutex::new(None));
        let capture = latest.clone();
        let recovery = RecoveryOptions::new(Arc::new(move |cp| {
            if cp.bytes_written() > 0 {
                *capture.lock().unwrap() = Some(cp);
                return Err(EngineError::output(
                    std::io::Error::other("interruption").into(),
                ));
            }
            Ok(())
        }));
        assert!(
            session(source.clone(), &snapshot, options(4, 4), None)
                .write_recoverable_to_file(&path, recovery)
                .await
                .is_err()
        );
        let checkpoint = latest.lock().unwrap().take().unwrap();
        session(source, &snapshot, options(4, 4), Some(checkpoint))
            .write_recoverable_to_file(&path, RecoveryOptions::new(Arc::new(|_| Ok(()))))
            .await
            .unwrap();
        assert_eq!(
            sample::canonical(std::fs::read(&path).unwrap()),
            sample::canonical(expected),
            "{name}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn invalid_download_windows_are_rejected_at_construction() {
    let (source, _) = fixture();
    for options in [
        options(0, 1),
        options(2, 0),
        options(2, 1).with_resources(ResourceOptions::default()),
    ] {
        let result = EngineSession::new(
            EngineInputs::new(
                EngineInput::new(InputId::new("primary").unwrap(), Arc::new(source.clone())),
                EmbeddedAudio::Keep,
            ),
            sample::keys(Arc::new(sample::Provider)),
            options,
        );
        assert!(matches!(result, Err(error) if error.kind() == EngineErrorKind::InvalidOptions));
    }
}

#[tokio::test]
async fn live_snapshot_updates_refill_only_admitted_segments() {
    let (probe, snapshot) = fixture();
    let expected = output(session(Arc::new(probe), &snapshot, options(1, 1), None)).await;
    let (probe, _) = fixture();
    let observed = probe.observed.clone();
    let id = InputId::new("primary").unwrap();
    let parse = |text: String, revision| {
        parse_playlist_snapshot(
            &TextResource {
                location: SourceLocation::Url("https://probe.test/input.m3u8".parse().unwrap()),
                content: text,
            },
            PlaylistContext::new(InputId::new("primary").unwrap(), 0).with_revision(revision),
        )
        .unwrap()
    };
    let text = include_str!("fixtures/multi_input/ts/input.m3u8");
    let first = parse(
        format!("{}seg2.ts\n", text.split("seg2.ts").next().unwrap()),
        0,
    );
    let next = parse(text.into(), 1);
    let holder = Arc::new(Mutex::new(None::<EngineHandle>));
    let callback = holder.clone();
    let configured = options(3, 3).with_on_event(Arc::new(move |event| {
        if let EngineEvent::Committed { input, .. } = event
            && input.committed() == 1
        {
            callback
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .accept_snapshot(&InputId::new("primary").unwrap(), &next)
                .unwrap();
        }
    }));
    let session = EngineSession::new(
        EngineInputs::new(
            EngineInput::new(id.clone(), Arc::new(probe)),
            EmbeddedAudio::Keep,
        ),
        sample::keys(Arc::new(sample::Provider)),
        configured,
    )
    .unwrap();
    let handle = session.handle();
    *holder.lock().unwrap() = Some(handle.clone());
    handle.accept_snapshot(&id, &first).unwrap();
    let actual = tokio::time::timeout(Duration::from_secs(5), output(session))
        .await
        .unwrap();
    assert_eq!(observed.started.lock().unwrap().len(), 6);
    assert_eq!(observed.peak.load(Ordering::SeqCst), 3);
    assert_eq!(handle.progress()[0].committed(), 6);
    assert_eq!(sample::canonical(actual), sample::canonical(expected));
}
