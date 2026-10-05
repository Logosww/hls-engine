use super::*;
use std::{
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll},
};

#[derive(Debug, Default)]
struct Counts {
    reads: AtomicUsize,
    active: AtomicUsize,
    stopped: AtomicUsize,
    writes: AtomicUsize,
    flushes: AtomicUsize,
}
struct Active(Arc<Counts>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}
#[derive(Debug)]
struct Reader {
    counts: Arc<Counts>,
    block_at: usize,
}
impl Source for Reader {
    fn stop_session(&self) {
        self.counts.stopped.fetch_add(1, Ordering::SeqCst);
    }
    fn read_text<'a>(
        &'a self,
        _: &'a SourceLocation,
    ) -> Pin<Box<dyn Future<Output = Result<TextResource>> + Send + 'a>> {
        unreachable!()
    }
    fn read_bytes<'a>(
        &'a self,
        _: &'a SourceLocation,
        _: Option<&'a ByteRange>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>>> + Send + 'a>> {
        Box::pin(async move {
            let number = self.counts.reads.fetch_add(1, Ordering::SeqCst) + 1;
            self.counts.active.fetch_add(1, Ordering::SeqCst);
            let _guard = Active(self.counts.clone());
            if number == self.block_at {
                std::future::pending::<()>().await;
            }
            Ok(include_bytes!("../fixtures/media/ts_avc_video_only/seg0.ts").to_vec())
        })
    }
}
fn input(counts: Arc<Counts>, block_at: usize) -> KeyedInputs {
    source_input(
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\na.ts\n#EXT-X-ENDLIST\n",
        Arc::new(Reader { counts, block_at }),
    )
}

#[tokio::test]
async fn cancel_and_drop_release_pending_scan_and_replay_reads() {
    for block_at in [1, 2, 3] {
        for cancel in [false, true] {
            let counts = Arc::new(Counts::default());
            let token = Arc::new(Cancel::new());
            let session = prepare_hls_timeline(
                input(counts.clone(), block_at),
                suite::keys(Arc::new(suite::corpus::Provider)),
                options().with_cancel(token.clone()),
            )
            .await
            .unwrap();
            let mut bytes = Vec::new();
            {
                let mut future = Box::pin(session.write_to(&mut bytes));
                assert!(futures_util::poll!(&mut future).is_pending());
                assert_eq!(counts.active.load(Ordering::SeqCst), 1);
                if cancel {
                    token.cancel();
                    let error = tokio::time::timeout(std::time::Duration::from_secs(1), future)
                        .await
                        .unwrap()
                        .unwrap_err();
                    assert_eq!(error.kind(), TimelineErrorKind::Cancelled);
                    assert!(error.completed_outputs().is_empty());
                }
            }
            assert_eq!(counts.active.load(Ordering::SeqCst), 0);
            assert_eq!(counts.stopped.load(Ordering::SeqCst), 1);
            assert_eq!(counts.reads.load(Ordering::SeqCst), block_at);
        }
    }
}

struct SlowWriter {
    counts: Arc<Counts>,
    block_flush: bool,
}
impl tokio::io::AsyncWrite for SlowWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.counts.writes.fetch_add(1, Ordering::SeqCst);
        if self.block_flush {
            Poll::Ready(Ok(bytes.len()))
        } else {
            Poll::Pending
        }
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.counts.flushes.fetch_add(1, Ordering::SeqCst);
        Poll::Pending
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        panic!("caller owns shutdown")
    }
}

#[tokio::test]
async fn blocked_writes_and_flushes_prevent_replay_reads_and_allow_cancel_or_drop() {
    for block_flush in [false, true] {
        for cancel in [false, true] {
            let counts = Arc::new(Counts::default());
            let token = Arc::new(Cancel::new());
            let session = prepare_hls_timeline(
                input(counts.clone(), usize::MAX),
                suite::keys(Arc::new(suite::corpus::Provider)),
                options().with_cancel(token.clone()),
            )
            .await
            .unwrap();
            let mut writer = SlowWriter {
                counts: counts.clone(),
                block_flush,
            };
            {
                let mut future = Box::pin(session.write_to(&mut writer));
                assert!(futures_util::poll!(&mut future).is_pending());
                assert_eq!(
                    counts.reads.load(Ordering::SeqCst),
                    2,
                    "blocked header must prevent output replay after catalog verification"
                );
                if cancel {
                    token.cancel();
                    let error = tokio::time::timeout(std::time::Duration::from_secs(1), future)
                        .await
                        .unwrap()
                        .unwrap_err();
                    assert_eq!(error.kind(), TimelineErrorKind::Cancelled);
                    assert!(error.completed_outputs().is_empty());
                }
            }
            assert_eq!(counts.reads.load(Ordering::SeqCst), 2);
            assert_eq!(counts.stopped.load(Ordering::SeqCst), 1);
        }
    }
}

struct PendingProvider(Arc<Counts>);
impl TimelineWriterProvider for PendingProvider {
    type Writer = Vec<u8>;
    fn acquire<'a>(
        &'a mut self,
        _: TimelineOutputRequest,
    ) -> Pin<Box<dyn Future<Output = TimelineResult<Vec<u8>>> + 'a>> {
        Box::pin(async move {
            self.0.active.fetch_add(1, Ordering::SeqCst);
            let _guard = Active(self.0.clone());
            std::future::pending().await
        })
    }
}
#[tokio::test]
async fn cancel_and_drop_release_pending_output_leases() {
    for cancel in [false, true] {
        let counts = Arc::new(Counts::default());
        let token = Arc::new(Cancel::new());
        let session = prepare_hls_timeline(
            input(counts.clone(), usize::MAX),
            suite::keys(Arc::new(suite::corpus::Provider)),
            options().with_cancel(token.clone()),
        )
        .await
        .unwrap();
        let mut provider = PendingProvider(counts.clone());
        {
            let mut future = Box::pin(session.write_to_outputs(&mut provider));
            assert!(futures_util::poll!(&mut future).is_pending());
            assert_eq!(counts.active.load(Ordering::SeqCst), 1);
            if cancel {
                token.cancel();
                let error = tokio::time::timeout(std::time::Duration::from_secs(1), future)
                    .await
                    .unwrap()
                    .unwrap_err();
                assert_eq!(error.kind(), TimelineErrorKind::Cancelled);
            }
        }
        assert_eq!(counts.active.load(Ordering::SeqCst), 0);
        assert_eq!(counts.stopped.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn cancel_and_drop_abort_pending_key_resolution_once() {
    use hls_transmux::crypto::key::*;
    struct PendingKey(Arc<Counts>);
    impl KeyProvider for PendingKey {
        fn resolve(&self, _: KeyRequest) -> KeyFuture<KeyResolution> {
            let counts = self.0.clone();
            Box::pin(async move {
                counts.active.fetch_add(1, Ordering::SeqCst);
                let _guard = Active(counts);
                std::future::pending().await
            })
        }
        fn abort(&self, request: &KeyRequest) {
            assert!(request.cancellation().is_cancelled());
            self.0.stopped.fetch_add(1, Ordering::SeqCst);
        }
    }
    for cancel in [false, true] {
        let counts = Arc::new(Counts::default());
        let token = Arc::new(Cancel::new());
        let input = snapshot_input(
            "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-KEY:METHOD=AES-128,URI=\"key\"\n#EXTINF:2,\na.ts\n#EXT-X-ENDLIST\n",
            &[("a.ts", &[0; 16])],
        );
        let session = prepare_hls_timeline(
            input,
            suite::keys(Arc::new(PendingKey(counts.clone()))),
            options().with_cancel(token.clone()),
        )
        .await
        .unwrap();
        {
            let mut future = Box::pin(session.into_mp4_bytes());
            assert!(futures_util::poll!(&mut future).is_pending());
            assert_eq!(counts.active.load(Ordering::SeqCst), 1);
            if cancel {
                token.cancel();
                let error = tokio::time::timeout(std::time::Duration::from_secs(1), future)
                    .await
                    .unwrap()
                    .unwrap_err();
                assert_eq!(error.kind(), TimelineErrorKind::Cancelled);
            }
        }
        assert_eq!(counts.active.load(Ordering::SeqCst), 0);
        assert_eq!(counts.stopped.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn split_media_write_and_flush_faults_keep_only_completed_outputs() {
    use std::{cell::RefCell, rc::Rc, sync::Mutex};
    struct Lease {
        index: usize,
        bytes: Rc<RefCell<Vec<Vec<u8>>>>,
        fail_flush: bool,
        media_started: bool,
    }
    impl tokio::io::AsyncWrite for Lease {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            if self.index == 1 && self.media_started && !self.fail_flush {
                return Poll::Ready(Err(std::io::Error::other("media write fault")));
            }
            let moof = bytes.windows(4).position(|bytes| bytes == b"moof");
            self.media_started |= moof.is_some();
            let count = if self.index == 1 && self.media_started && !self.fail_flush {
                bytes.len().min(moof.unwrap_or(0) + 13)
            } else {
                bytes.len()
            };
            self.bytes.borrow_mut()[self.index].extend_from_slice(&bytes[..count]);
            Poll::Ready(Ok(count))
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(
                if self.index == 1 && self.media_started && self.fail_flush {
                    Err(std::io::Error::other("media flush fault"))
                } else {
                    Ok(())
                },
            )
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            panic!("caller owns shutdown")
        }
    }
    struct Provider {
        bytes: Rc<RefCell<Vec<Vec<u8>>>>,
        fail_flush: bool,
    }
    impl TimelineWriterProvider for Provider {
        type Writer = Lease;
        fn acquire<'a>(
            &'a mut self,
            request: TimelineOutputRequest,
        ) -> Pin<Box<dyn Future<Output = TimelineResult<Lease>> + 'a>> {
            Box::pin(async move {
                self.bytes.borrow_mut().push(Vec::new());
                Ok(Lease {
                    index: request.index(),
                    bytes: self.bytes.clone(),
                    fail_flush: self.fail_flush,
                    media_started: false,
                })
            })
        }
    }
    for fail_flush in [false, true] {
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed = events.clone();
        let bytes = Rc::new(RefCell::new(Vec::new()));
        let error = prepare_hls_timeline(
            three_configurations(),
            suite::keys(Arc::new(suite::corpus::Provider)),
            options()
                .with_change_policy(TimelineChangePolicy::Split)
                .with_on_event(Arc::new(move |event| {
                    observed
                        .lock()
                        .unwrap()
                        .push((event.kind(), event.output_index()));
                })),
        )
        .await
        .unwrap()
        .write_to_outputs(&mut Provider {
            bytes: bytes.clone(),
            fail_flush,
        })
        .await
        .unwrap_err();
        assert_eq!(error.kind(), TimelineErrorKind::Output);
        assert_eq!(error.completed_outputs().len(), 1);
        assert_eq!(error.completed_outputs()[0].index(), 0);
        assert_eq!(
            events.lock().unwrap().as_slice(),
            &[
                (TimelineEventKind::MappingCommitted, 0),
                (TimelineEventKind::OutputCompleted, 0)
            ]
        );
        let bytes = bytes.borrow();
        assert_eq!(bytes.len(), 2);
        assert_eq!(
            bytes[0].len() as u64,
            error.completed_outputs()[0].media().bytes_written
        );
        assert!(bytes[1].windows(4).any(|b| b == b"moof"));
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[tokio::test]
async fn split_native_finalization_cancellation_keeps_published_output_and_cleans_temps() {
    use std::{path::PathBuf, sync::atomic::AtomicBool};
    #[derive(Debug)]
    struct StopWorker {
        caller: std::thread::ThreadId,
        armed: Arc<AtomicBool>,
        checks: AtomicUsize,
        limit: usize,
        signal: Cancel,
    }
    impl CancelToken for StopWorker {
        fn is_cancelled(&self) -> bool {
            if self.armed.load(Ordering::SeqCst)
                && std::thread::current().id() != self.caller
                && self.checks.fetch_add(1, Ordering::SeqCst) + 1 >= self.limit
            {
                self.signal.cancel();
            }
            self.signal.is_cancelled()
        }
        fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            self.signal.cancelled()
        }
    }
    struct Directory(PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    struct Provider(PathBuf);
    impl TimelineFileProvider for Provider {
        fn acquire<'a>(
            &'a mut self,
            request: TimelineOutputRequest,
        ) -> Pin<Box<dyn Future<Output = TimelineResult<PathBuf>> + 'a>> {
            Box::pin(async move { Ok(self.0.join(format!("output-{}.mp4", request.index()))) })
        }
    }
    for limit in [1, 5, 20] {
        let directory = Directory(std::env::temp_dir().join(format!(
                "timeline-finalize-fault-{}-{}-{limit}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )));
        std::fs::create_dir(&directory.0).unwrap();
        let previous = b"existing destination must survive cancelled finalization";
        std::fs::write(directory.0.join("output-1.mp4"), previous).unwrap();
        let armed = Arc::new(AtomicBool::new(false));
        let signal = armed.clone();
        let token = Arc::new(StopWorker {
            caller: std::thread::current().id(),
            armed,
            checks: AtomicUsize::new(0),
            limit,
            signal: Cancel::new(),
        });
        let error = prepare_hls_timeline(
            three_configurations(),
            suite::keys(Arc::new(suite::corpus::Provider)),
            options()
                .with_change_policy(TimelineChangePolicy::Split)
                .with_cancel(token.clone())
                .with_on_event(Arc::new(move |event| {
                    if event.kind() == TimelineEventKind::OutputCompleted
                        && event.output_index() == 0
                    {
                        signal.store(true, Ordering::SeqCst);
                    }
                })),
        )
        .await
        .unwrap()
        .write_to_files(
            &mut Provider(directory.0.clone()),
            FileOutputOptions::default(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), TimelineErrorKind::Cancelled);
        assert_eq!(error.completed_outputs().len(), 1);
        assert!(token.checks.load(Ordering::SeqCst) >= limit);
        assert_eq!(
            std::fs::read(directory.0.join("output-1.mp4")).unwrap(),
            previous
        );
        assert_eq!(
            std::fs::metadata(directory.0.join("output-0.mp4"))
                .unwrap()
                .len(),
            error.completed_outputs()[0].media().bytes_written
        );
        assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 2);
    }
}
