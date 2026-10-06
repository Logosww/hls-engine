use hls_transmux::*;
use std::sync::Arc;
#[path = "support/keyed_corpus.rs"]
mod suite;
#[tokio::test]
async fn clear_and_aes_single_dual_all_container_pairs_bytes_and_writer() {
    let report = suite::run(Arc::new(suite::corpus::Provider)).await;
    assert_eq!(report["cases"].as_array().unwrap().len(), 96);
}
#[tokio::test]
async fn native_files_match_clear_outputs() {
    for v in [
        "ts_avc_regular",
        "ts_hevc_regular",
        "fmp4_avc_regular",
        "fmp4_hevc_regular",
    ] {
        for a in [None, Some("ts_aac_audio_only"), Some("fmp4_aac_audio_only")] {
            for format in [
                OutputFormat::Mp4,
                OutputFormat::FragmentedMp4,
                OutputFormat::StreamingMp4,
            ] {
                for backend in [
                    FinalizeBackend::Native,
                    #[cfg(feature = "ffmpeg-finalize")]
                    FinalizeBackend::Ffmpeg,
                ] {
                    if !matches!(backend, FinalizeBackend::Native)
                        && format != OutputFormat::StreamingMp4
                    {
                        continue;
                    }
                    let base = std::env::temp_dir().join(format!(
                        "keyed-{}-{v}-{}-{format:?}-{backend:?}",
                        std::process::id(),
                        a.unwrap_or("none")
                    ));
                    std::fs::create_dir_all(&base).unwrap();
                    let (clear, keyed) = suite::pair(v, a, [true, true], true);
                    let options = FileOutputOptions::default()
                        .with_format(format)
                        .with_finalize_backend(backend);
                    let old = prepare_hls(clear, PrepareOptions::default())
                        .await
                        .unwrap()
                        .write_to_file(base.join("clear.mp4"), options.clone())
                        .await
                        .unwrap();
                    let new = prepare_hls_with_keys(
                        keyed,
                        suite::keys(Arc::new(suite::corpus::Provider)),
                        suite::options(),
                    )
                    .await
                    .unwrap()
                    .write_to_file(base.join("keyed.mp4"), options)
                    .await
                    .unwrap();
                    assert_eq!(old.media().tracks, new.media().tracks);
                    assert!(
                        suite::canonical(std::fs::read(base.join("clear.mp4")).unwrap())
                            == suite::canonical(std::fs::read(base.join("keyed.mp4")).unwrap()),
                        "{v}/{a:?}/{format:?}/{backend:?}"
                    );
                    std::fs::remove_dir_all(base).unwrap();
                }
            }
        }
    }
}

use futures_util::{FutureExt, task::noop_waker};
use hls_transmux::crypto::{key::*, resource::*};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};
use tokio::io::AsyncWrite;
#[derive(Debug, Default)]
struct Metrics {
    reads: Mutex<Vec<String>>,
    active: AtomicUsize,
    sessions: AtomicUsize,
    stops: AtomicUsize,
}
#[derive(Debug)]
struct Observed {
    source: Arc<dyn Source>,
    metrics: Arc<Metrics>,
    hang: bool,
    fail_after: usize,
}
struct Guard(Arc<Metrics>);
impl Drop for Guard {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}
impl Source for Observed {
    fn create_session_with_options(
        &self,
        options: &SourceSessionOptions,
    ) -> Option<Arc<dyn Source>> {
        assert!(options.demand_driven());
        assert!(options.max_resource_bytes().is_some());
        self.metrics.sessions.fetch_add(1, Ordering::SeqCst);
        Some(Arc::new(Self {
            source: self.source.clone(),
            metrics: self.metrics.clone(),
            hang: self.hang,
            fail_after: self.fail_after,
        }))
    }
    fn stop_session(&self) {
        self.metrics.stops.fetch_add(1, Ordering::SeqCst);
    }
    fn read_text<'a>(
        &'a self,
        _: &'a SourceLocation,
    ) -> Pin<Box<dyn Future<Output = Result<TextResource>> + Send + 'a>> {
        panic!("snapshot already parsed")
    }
    fn read_bytes<'a>(
        &'a self,
        location: &'a SourceLocation,
        range: Option<&'a ByteRange>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>>> + Send + 'a>> {
        Box::pin(async move {
            self.metrics
                .reads
                .lock()
                .unwrap()
                .push(format!("{location:?}"));
            self.metrics.active.fetch_add(1, Ordering::SeqCst);
            let _guard = Guard(self.metrics.clone());
            if self.hang {
                return std::future::pending().await;
            }
            if self.metrics.reads.lock().unwrap().len() > self.fail_after {
                return Err(Error::invalid("secret source cause ?token=HIDDEN"));
            }
            self.source.read_bytes(location, range).await
        })
    }
}
fn observed(
    name: &str,
    metrics: Arc<Metrics>,
    hang: bool,
    fail_after: usize,
    replacement: Option<String>,
) -> KeyedInputs {
    let (legacy, input) = suite::corpus::input(name, "primary", true, false);
    let HlsInput::Custom(source, _) = legacy else {
        unreachable!()
    };
    let snapshot = if let Some(content) = replacement {
        parse_playlist_snapshot(
            &TextResource {
                content,
                location: input.snapshot().location().location().clone(),
            },
            input.snapshot().context().clone(),
        )
        .unwrap()
    } else {
        input.snapshot().clone()
    };
    KeyedInputs::new(KeyedInput::new(
        snapshot,
        Arc::new(Observed {
            source,
            metrics,
            hang,
            fail_after,
        }),
    ))
}
fn reads(m: &Metrics) -> usize {
    m.reads.lock().unwrap().len()
}
fn map_reads(m: &Metrics) -> usize {
    m.reads
        .lock()
        .unwrap()
        .iter()
        .filter(|s| s.contains("init.cbc"))
        .count()
}
#[derive(Debug)]
struct Cancel(tokio::sync::watch::Sender<bool>);
impl Cancel {
    fn new() -> Self {
        Self(tokio::sync::watch::channel(false).0)
    }
    fn cancel(&self) {
        self.0.send_replace(true);
    }
}
impl CancelToken for Cancel {
    fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        let mut rx = self.0.subscribe();
        Box::pin(async move {
            rx.wait_for(|v| *v).await.unwrap();
        })
    }
}
#[derive(Default)]
struct Clock(AtomicU64);
impl KeyClock for Clock {
    fn now(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}
struct CountingProvider {
    clock: Arc<Clock>,
    calls: AtomicUsize,
    aborts: AtomicUsize,
    pending: bool,
    bad_map_after_expiry: bool,
}
impl KeyProvider for CountingProvider {
    fn resolve(&self, request: KeyRequest) -> KeyFuture<KeyResolution> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let pending = self.pending;
        let now = self.clock.now();
        let key = if (request.resource().kind() == KeyResourceKind::Media
            && request.resource().slot().sequence() == suite::corpus::SEQUENCE + 1)
            || (request.resource().kind() == KeyResourceKind::Map
                && self.bad_map_after_expiry
                && now > 0)
        {
            suite::corpus::KEY_B
        } else {
            suite::corpus::KEY_A
        };
        Box::pin(async move {
            if pending {
                return std::future::pending().await;
            }
            KeyResolution::Available(
                AvailableKey::aes128(SecretKey::new(key.to_vec()).unwrap())
                    .with_valid_until(now + 1)
                    .with_version(format!("version-{now}")),
            )
        })
    }
    fn abort(&self, _: &KeyRequest) {
        self.aborts.fetch_add(1, Ordering::SeqCst);
    }
}
fn provider(pending: bool, bad_map_after_expiry: bool) -> Arc<CountingProvider> {
    Arc::new(CountingProvider {
        clock: Arc::new(Clock::default()),
        calls: AtomicUsize::new(0),
        aborts: AtomicUsize::new(0),
        pending,
        bad_map_after_expiry,
    })
}
fn counted_keys(p: Arc<CountingProvider>) -> KeySession {
    KeySession::new(
        "test",
        "test",
        p.clone(),
        p.clock.clone(),
        KeySessionOptions::default(),
    )
    .unwrap()
}
#[tokio::test]
async fn map_cache_probe_reuse_invalidation_expiry_and_same_uri_redeclaration() {
    for mode in [
        "cached",
        "invalidate",
        "expired",
        "redeclaration",
        "bad-new-key",
    ] {
        let m = Arc::new(Metrics::default());
        let p = provider(false, mode == "bad-new-key");
        let replacement=(mode=="redeclaration").then(||include_str!("fixtures/crypto/fmp4_avc_regular/input.m3u8").replace("#EXT-X-KEY:METHOD=NONE", "#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\",IV=0x000102030405060708090a0b0c0d0e0f\n#EXT-X-MAP:URI=\"init.cbc\"\n#EXT-X-KEY:METHOD=NONE"));
        let prepared = prepare_hls_with_keys(
            observed(
                "fmp4_avc_regular",
                m.clone(),
                false,
                usize::MAX,
                replacement,
            ),
            counted_keys(p.clone()),
            suite::options(),
        )
        .await
        .unwrap();
        assert_eq!(reads(&m), 2);
        assert_eq!(map_reads(&m), 1);
        if mode == "invalidate" {
            prepared.invalidate_keys().unwrap();
        }
        if matches!(mode, "expired" | "bad-new-key") {
            p.clock.0.store(1, Ordering::SeqCst);
        }
        let result = prepared.into_mp4_bytes().await;
        if mode == "bad-new-key" {
            let error = result.unwrap_err();
            assert_eq!(error.phase(), KeyedSessionPhase::Initialization);
            assert!(matches!(
                error.resource_error().unwrap().kind(),
                ResourceErrorKind::Decrypt | ResourceErrorKind::MediaValidation
            ));
        } else {
            let (_, report) = result.unwrap();
            assert_eq!(report.inputs()[0].processed_segments(), 3);
            assert_eq!(reads(&m), if mode == "cached" { 4 } else { 5 });
        }
        assert_eq!(map_reads(&m), if mode == "cached" { 1 } else { 2 });
        assert_eq!(
            m.sessions.load(Ordering::SeqCst),
            m.stops.load(Ordering::SeqCst)
        );
    }
}
#[tokio::test]
async fn cancellation_and_drop_during_read_or_provider_wait_release_leases() {
    for key_wait in [false, true] {
        for cancel_it in [false, true] {
            let m = Arc::new(Metrics::default());
            let p = provider(key_wait, false);
            let cancel = Arc::new(Cancel::new());
            let mut future = Box::pin(prepare_hls_with_keys(
                observed("ts_avc_regular", m.clone(), !key_wait, usize::MAX, None),
                counted_keys(p.clone()),
                suite::options().with_cancel(cancel.clone()),
            ));
            assert!(
                future
                    .poll_unpin(&mut Context::from_waker(&noop_waker()))
                    .is_pending()
            );
            assert_eq!(reads(&m), 1);
            if cancel_it {
                cancel.cancel();
                assert_eq!(
                    future.await.err().unwrap().kind(),
                    KeyedSessionErrorKind::Cancelled
                );
            } else {
                drop(future);
            }
            assert_eq!(m.active.load(Ordering::SeqCst), 0);
            assert_eq!(
                m.sessions.load(Ordering::SeqCst),
                m.stops.load(Ordering::SeqCst)
            );
            assert_eq!(p.aborts.load(Ordering::SeqCst), usize::from(key_wait));
        }
    }
}
struct Writer {
    data: Vec<u8>,
    block: bool,
    fail: bool,
    flush_block: bool,
    flush_fail: bool,
    flushes: usize,
    final_flush_block: bool,
    local: std::rc::Rc<()>,
}
impl Writer {
    fn new() -> Self {
        Self {
            data: vec![],
            block: false,
            fail: false,
            flush_block: false,
            flush_fail: false,
            flushes: 0,
            final_flush_block: false,
            local: std::rc::Rc::new(()),
        }
    }
}
impl AsyncWrite for Writer {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if self.block {
            return Poll::Pending;
        }
        if self.fail {
            return Poll::Ready(Err(std::io::Error::other("secret writer cause")));
        }
        let n = bytes.len().min(157);
        self.data.extend_from_slice(&bytes[..n]);
        Poll::Ready(Ok(n))
    }
    fn poll_flush(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.flushes += 1;
        assert_eq!(std::rc::Rc::strong_count(&self.local), 1);
        if self.flush_block || (self.final_flush_block && self.flushes >= 4) {
            return Poll::Pending;
        }
        if self.flush_fail {
            return Poll::Ready(Err(std::io::Error::other("flush failure")));
        }
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        panic!("borrowed writer never closed")
    }
}
#[tokio::test]
async fn writer_backpressure_short_writes_flush_failure_cancel_and_drop() {
    for mode in [
        "success",
        "block-cancel",
        "block-drop",
        "write-fail",
        "flush-fail",
        "flush-cancel",
        "final-flush-cancel",
    ] {
        let m = Arc::new(Metrics::default());
        let cancel = Arc::new(Cancel::new());
        let events = Arc::new(Mutex::new(Vec::new()));
        let copy = events.clone();
        let prepared = prepare_hls_with_keys(
            observed("fmp4_avc_regular", m.clone(), false, usize::MAX, None),
            suite::keys(Arc::new(suite::corpus::Provider)),
            suite::options()
                .with_cancel(cancel.clone())
                .with_on_event(Arc::new(move |e| copy.lock().unwrap().push(e))),
        )
        .await
        .unwrap();
        let mut writer = Writer::new();
        writer.block = mode.starts_with("block");
        writer.fail = mode == "write-fail";
        writer.flush_fail = mode == "flush-fail";
        writer.flush_block = mode == "flush-cancel";
        writer.final_flush_block = mode == "final-flush-cancel";
        let mut future = Box::pin(prepared.write_to(&mut writer));
        if mode.starts_with("block") || mode.ends_with("flush-cancel") {
            for _ in 0..10 {
                assert!(
                    future
                        .poll_unpin(&mut Context::from_waker(&noop_waker()))
                        .is_pending()
                );
                assert_eq!(reads(&m), if mode == "final-flush-cancel" { 4 } else { 2 });
            }
            if mode == "block-drop" {
                drop(future);
            } else {
                cancel.cancel();
                assert_eq!(
                    future.await.unwrap_err().kind(),
                    KeyedSessionErrorKind::Cancelled
                );
            }
        } else if mode == "success" {
            let report = future.await.unwrap();
            assert_eq!(report.inputs()[0].processed_segments(), 3);
            assert_eq!(reads(&m), 4);
        } else {
            let error = future.await.unwrap_err();
            assert_eq!(error.kind(), KeyedSessionErrorKind::Output);
            assert!(error.input_id().is_none());
            assert!(!format!("{error:?} {error}").contains("secret writer"));
        }
        assert_eq!(
            events
                .lock()
                .unwrap()
                .iter()
                .filter(|e| e.phase() == KeyedSessionPhase::Completed)
                .count(),
            usize::from(mode == "success")
        );
        assert_eq!(
            m.sessions.load(Ordering::SeqCst),
            m.stops.load(Ordering::SeqCst)
        );
    }
}
#[tokio::test]
async fn preflight_rejects_unsupported_before_io_and_resource_errors_keep_original_slot() {
    let m = Arc::new(Metrics::default());
    let p = provider(false, false);
    let text = include_str!("fixtures/crypto/ts_avc_regular/input.m3u8");
    for replacement in [
        text.replace("#EXT-X-ENDLIST", ""),
        text.replace("#EXTINF:2,", "#EXT-X-GAP\n#EXTINF:2,"),
        text.replace("METHOD=AES-128", "METHOD=AES-256-GCM"),
    ] {
        let result = prepare_hls_with_keys(
            observed(
                "ts_avc_regular",
                m.clone(),
                false,
                usize::MAX,
                Some(replacement),
            ),
            counted_keys(p.clone()),
            suite::options(),
        )
        .await;
        assert_eq!(
            result.err().unwrap().kind(),
            KeyedSessionErrorKind::UnsupportedPlaylist
        );
    }
    assert_eq!(reads(&m), 0);
    assert_eq!(p.calls.load(Ordering::SeqCst), 0);
    let result = prepare_hls_with_keys(
        observed("ts_avc_regular", m.clone(), false, 0, None),
        counted_keys(p),
        suite::options(),
    )
    .await;
    let error = result.err().unwrap();
    assert_eq!(error.kind(), KeyedSessionErrorKind::Resource);
    assert_eq!(error.slot().unwrap().sequence(), suite::corpus::SEQUENCE);
    assert!(!format!("{error:?} {error}").contains("HIDDEN"));
    assert!(error.resource_error().unwrap().raw_cause().is_some());
}
#[tokio::test]
async fn native_failure_preserves_target_and_streaming_partial() {
    for format in [
        OutputFormat::Mp4,
        OutputFormat::FragmentedMp4,
        OutputFormat::StreamingMp4,
    ] {
        let dir =
            std::env::temp_dir().join(format!("keyed-failure-{}-{format:?}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("target.mp4");
        std::fs::write(&path, b"existing").unwrap();
        let m = Arc::new(Metrics::default());
        let prepared = prepare_hls_with_keys(
            observed("fmp4_avc_regular", m, false, 2, None),
            suite::keys(Arc::new(suite::corpus::Provider)),
            suite::options(),
        )
        .await
        .unwrap();
        assert!(
            prepared
                .write_to_file(&path, FileOutputOptions::default().with_format(format))
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"existing");
        let partials = std::fs::read_dir(&dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains("partial")
            })
            .count();
        assert_eq!(partials, usize::from(format == OutputFormat::StreamingMp4));
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[tokio::test]
async fn independent_sequences_tails_and_minimum_resource_budget() {
    // Audio has a different original sequence and ends before the primary.
    for (v, a) in [
        ("ts_avc_regular", "fmp4_aac_audio_only"),
        ("fmp4_avc_regular", "ts_aac_audio_only"),
    ] {
        let (_, primary) = suite::corpus::input(v, "primary", true, false);
        let (legacy_audio, audio) = suite::corpus::input(a, "audio", false, false);
        let HlsInput::Custom(source, location) = legacy_audio else {
            unreachable!()
        };
        let text = source.read_text(&location).await.unwrap();
        let mut lines = text.content.lines().map(str::to_owned).collect::<Vec<_>>();
        let third = lines.iter().position(|l| l.starts_with("seg2.")).unwrap();
        lines.drain(third - 1..=third);
        let text = lines
            .join("\n")
            .replace(&suite::corpus::SEQUENCE.to_string(), "71");
        let snapshot = parse_playlist_snapshot(
            &TextResource {
                content: text,
                location,
            },
            audio.snapshot().context().clone(),
        )
        .unwrap();
        let inputs = KeyedInputs::new(primary).with_audio(KeyedInput::new(snapshot, source));
        let output = prepare_hls_with_keys(
            inputs,
            suite::keys(Arc::new(suite::corpus::Provider)),
            KeyedPrepareOptions::default()
                .with_resources(ResourceOptions::default().with_limits(200_000, 200_000, 1)),
        )
        .await
        .unwrap();
        assert_eq!(output.snapshots()[1].segments()[0].slot().sequence(), 71);
        let (_, report) = output.into_mp4_bytes().await.unwrap();
        assert_eq!(report.inputs()[0].processed_segments(), 3);
        assert_eq!(report.inputs()[1].processed_segments(), 2);
        assert_eq!(report.media().tracks[0].sample_count, 180);
        assert!(report.media().tracks[1].sample_count > 0);
    }
}
