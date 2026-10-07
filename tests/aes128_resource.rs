#[path = "support/crypto_corpus.rs"]
mod corpus;
use futures_util::{FutureExt, task::noop_waker};
use hls_engine::legacy::{
    MemorySource, Source, SourceLocation, TextResource,
    crypto::{key::*, resource::*},
    parse_playlist_snapshot,
    playlist::*,
};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
fn snapshot(body: &str) -> PlaylistSnapshot {
    parse_playlist_snapshot(&TextResource {content:format!("#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:{}\n{body}\n#EXT-X-ENDLIST\n",corpus::SEQUENCE),location:SourceLocation::Url(url::Url::parse("https://user:password@media.test/list?token=SECRET").unwrap())},PlaylistContext::new(InputId::new("primary").unwrap(),0)).unwrap()
}
fn encrypted() -> ResourceRequest {
    ResourceRequest::media(
        &snapshot("#EXT-X-KEY:METHOD=AES-128,URI=\"key?secret=SECRET\"\n#EXTINF:2,\nseg"),
        0,
    )
    .unwrap()
}
fn explicit() -> ResourceRequest {
    ResourceRequest::media(&snapshot("#EXT-X-KEY:METHOD=AES-128,URI=\"key\",IV=0x000102030405060708090a0b0c0d0e0f\n#EXTINF:2,\nseg"),0).unwrap()
}
fn source(data: &[u8]) -> Arc<MemorySource> {
    Arc::new(MemorySource::new().segment("https://user:password@media.test/seg", data))
}
fn session(provider: Arc<dyn KeyProvider>, options: ResourceOptions) -> ResourceSession {
    ResourceSession::new(
        KeySession::new(
            "op",
            "auth",
            provider,
            Arc::new(corpus::Clock),
            KeySessionOptions::default(),
        )
        .unwrap(),
        options,
    )
    .unwrap()
}
#[tokio::test]
async fn retained_external_crypto_corpus() {
    let report = corpus::run(Arc::new(corpus::Provider)).await;
    assert_eq!(report["cases"].as_array().unwrap().len(), 12);
}

#[tokio::test]
async fn padding_length_wrong_key_wrong_iv_and_structure_are_distinct() {
    let s = session(Arc::new(corpus::Provider), ResourceOptions::default());
    for data in [&[][..], &[0u8; 15][..]] {
        assert_eq!(
            s.read(source(data), encrypted()).await.unwrap_err().kind(),
            ResourceErrorKind::InvalidCiphertextLength
        );
    }
    assert_eq!(
        s.read(
            source(include_bytes!("fixtures/crypto/invalid-padding.cbc")),
            explicit()
        )
        .await
        .unwrap_err()
        .kind(),
        ResourceErrorKind::Decrypt
    );
    assert_eq!(
        s.read(
            source(include_bytes!("fixtures/crypto/invalid-container.cbc")),
            explicit()
        )
        .await
        .unwrap_err()
        .kind(),
        ResourceErrorKind::MediaValidation
    );
    // CBC wrong IV changes only the first plaintext block: padding still succeeds.
    let mut iv = sequence_iv(corpus::SEQUENCE);
    iv[0] = 255;
    let iv = iv.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let request = ResourceRequest::media(
        &snapshot(&format!(
            "#EXT-X-KEY:METHOD=AES-128,URI=\"key\",IV=0x{iv}\n#EXTINF:2,\nseg"
        )),
        0,
    )
    .unwrap();
    assert_eq!(
        s.read(
            source(include_bytes!("fixtures/crypto/ts_avc_regular/seg0.cbc")),
            request
        )
        .await
        .unwrap_err()
        .kind(),
        ResourceErrorKind::MediaValidation
    );
    struct Wrong(AtomicUsize);
    impl KeyProvider for Wrong {
        fn resolve(&self, _: KeyRequest) -> KeyFuture<KeyResolution> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                KeyResolution::Available(AvailableKey::aes128(SecretKey::new(vec![0; 16]).unwrap()))
            })
        }
    }
    let wrong = Arc::new(Wrong(AtomicUsize::new(0)));
    let s = session(wrong.clone(), ResourceOptions::default());
    assert_eq!(
        s.read(
            source(include_bytes!("fixtures/crypto/ts_avc_regular/seg0.cbc")),
            encrypted()
        )
        .await
        .unwrap_err()
        .kind(),
        ResourceErrorKind::Decrypt
    );
    assert_eq!(wrong.0.load(Ordering::SeqCst), 1); // No key guessing or retry.
}
#[tokio::test]
async fn encrypted_ranges_require_attestation_and_exact_complete_reads() {
    let snapshot =
        snapshot("#EXT-X-KEY:METHOD=AES-128,URI=\"key\"\n#EXTINF:2,\n#EXT-X-BYTERANGE:16@0\nseg");
    let request = ResourceRequest::media(&snapshot, 0).unwrap();
    let s = session(Arc::new(corpus::Provider), ResourceOptions::default());
    assert_eq!(
        s.read(source(&[0; 16]), request.clone())
            .await
            .unwrap_err()
            .kind(),
        ResourceErrorKind::UnconfirmedRange
    );
    let s = session(
        Arc::new(corpus::Provider),
        ResourceOptions::default().with_encrypted_ranges(EncryptedRangePolicy::CompleteResources),
    );
    assert_eq!(
        s.read(source(&[0; 16]), request).await.unwrap_err().kind(),
        ResourceErrorKind::Decrypt
    );
    let iframe=parse_playlist_snapshot(&TextResource{content:"#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-I-FRAMES-ONLY\n#EXTINF:2,\nseg\n#EXT-X-ENDLIST\n".into(),location:SourceLocation::Url(url::Url::parse("https://media.test/list").unwrap())},PlaylistContext::new(InputId::new("p").unwrap(),0)).unwrap();
    assert_eq!(
        ResourceRequest::media(&iframe, 0)
            .unwrap_err()
            .playlist_rejection(),
        Some(PlaylistRejection::IFrameOnly)
    );
}
struct SlowProvider {
    calls: AtomicUsize,
    aborts: AtomicUsize,
    replies: Mutex<Vec<tokio::sync::oneshot::Sender<KeyResolution>>>,
}
impl KeyProvider for SlowProvider {
    fn resolve(&self, _: KeyRequest) -> KeyFuture<KeyResolution> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.replies.lock().unwrap().push(tx);
        Box::pin(async { rx.await.unwrap() })
    }
    fn abort(&self, _: &KeyRequest) {
        self.aborts.fetch_add(1, Ordering::SeqCst);
    }
}
fn slow() -> Arc<SlowProvider> {
    Arc::new(SlowProvider {
        calls: AtomicUsize::new(0),
        aborts: AtomicUsize::new(0),
        replies: Mutex::new(vec![]),
    })
}
#[tokio::test]
async fn slow_key_budget_drop_and_operation_cancel_release_all_resources() {
    let provider = slow();
    let s = session(
        provider.clone(),
        ResourceOptions::default().with_limits(100_000, 100_000, 2),
    );
    let mut a = Box::pin(s.read(
        source(include_bytes!("fixtures/crypto/ts_avc_regular/seg0.cbc")),
        encrypted(),
    ));
    assert!(
        a.poll_unpin(&mut std::task::Context::from_waker(&noop_waker()))
            .is_pending()
    );
    assert_eq!(s.stats().reserved_bytes(), 100_000);
    assert_eq!(
        s.read(source(&[0; 16]), encrypted())
            .await
            .unwrap_err()
            .kind(),
        ResourceErrorKind::BudgetExceeded
    );
    drop(a);
    assert_eq!(s.stats().resources(), 0);
    assert_eq!(provider.aborts.load(Ordering::SeqCst), 1);
    let mut a = Box::pin(s.read(
        source(include_bytes!("fixtures/crypto/ts_avc_regular/seg0.cbc")),
        encrypted(),
    ));
    assert!(
        a.poll_unpin(&mut std::task::Context::from_waker(&noop_waker()))
            .is_pending()
    );
    s.cancel();
    s.cancel();
    assert_eq!(a.await.unwrap_err().kind(), ResourceErrorKind::Cancelled);
    assert_eq!(s.stats().reserved_bytes(), 0);
    assert_eq!(provider.aborts.load(Ordering::SeqCst), 2);
    for tx in provider.replies.lock().unwrap().drain(..) {
        assert!(tx.send(KeyResolution::Unavailable).is_err());
    }
}
#[derive(Debug)]
struct ControlledSource {
    calls: Arc<AtomicUsize>,
    stops: Arc<AtomicUsize>,
    mode: u8,
}
impl Source for ControlledSource {
    fn create_session_with_options(
        &self,
        options: &hls_engine::legacy::SourceSessionOptions,
    ) -> Option<Arc<dyn Source>> {
        assert!(options.demand_driven());
        assert!(options.max_resource_bytes().is_some());
        Some(Arc::new(Self {
            calls: self.calls.clone(),
            stops: self.stops.clone(),
            mode: self.mode,
        }))
    }
    fn stop_session(&self) {
        self.stops.fetch_add(1, Ordering::SeqCst);
    }
    fn read_text<'a>(
        &'a self,
        _: &'a SourceLocation,
    ) -> Pin<Box<dyn Future<Output = hls_engine::legacy::Result<TextResource>> + Send + 'a>> {
        unreachable!()
    }
    fn read_bytes<'a>(
        &'a self,
        _: &'a SourceLocation,
        _: Option<&'a hls_engine::legacy::ByteRange>,
    ) -> Pin<Box<dyn Future<Output = hls_engine::legacy::Result<Vec<u8>>> + Send + 'a>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            match self.mode {
                0 => futures_util::future::pending().await,
                1 => Err(hls_engine::legacy::Error::invalid("RAW-CREDENTIALS")),
                _ => Ok(vec![0; 32]),
            }
        })
    }
}
#[tokio::test]
async fn read_cancel_drop_caps_and_redaction() {
    let calls = Arc::new(AtomicUsize::new(0));
    let stops = Arc::new(AtomicUsize::new(0));
    let source = Arc::new(ControlledSource {
        calls: calls.clone(),
        stops: stops.clone(),
        mode: 0,
    });
    let provider = slow();
    let s = session(provider.clone(), ResourceOptions::default());
    let mut read = Box::pin(s.read(source.clone(), encrypted()));
    assert!(
        read.poll_unpin(&mut std::task::Context::from_waker(&noop_waker()))
            .is_pending()
    );
    drop(read);
    assert_eq!(stops.load(Ordering::SeqCst), 1);
    assert_eq!(s.stats().resources(), 0);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    let mut read = Box::pin(s.read(source, encrypted()));
    assert!(
        read.poll_unpin(&mut std::task::Context::from_waker(&noop_waker()))
            .is_pending()
    );
    s.cancel();
    assert_eq!(read.await.unwrap_err().kind(), ResourceErrorKind::Cancelled);
    assert_eq!(stops.load(Ordering::SeqCst), 2);
    let s = session(
        Arc::new(corpus::Provider),
        ResourceOptions::default().with_limits(16, 32, 2),
    );
    for mode in [1, 2] {
        let err = s
            .read(
                Arc::new(ControlledSource {
                    calls: calls.clone(),
                    stops: stops.clone(),
                    mode,
                }),
                encrypted(),
            )
            .await
            .unwrap_err();
        assert_eq!(
            err.kind(),
            if mode == 1 {
                ResourceErrorKind::Read
            } else {
                ResourceErrorKind::ResourceTooLarge
            }
        );
        let text = format!("{err:?} {err}");
        for secret in ["RAW-CREDENTIALS", "password", "SECRET"] {
            assert!(!text.contains(secret));
        }
        if mode == 1 {
            assert!(
                err.raw_cause()
                    .unwrap()
                    .to_string()
                    .contains("RAW-CREDENTIALS")
            );
        }
    }
    assert_eq!(
        s.read(crate::source(&[0; 32]), encrypted())
            .await
            .unwrap_err()
            .kind(),
        ResourceErrorKind::ResourceTooLarge
    );
}
#[tokio::test]
async fn malformed_fmp4_envelopes_and_map_iv_fail() {
    let s = session(Arc::new(corpus::Provider), ResourceOptions::default());
    let snapshot = snapshot("#EXT-X-MAP:URI=\"init\"\n#EXTINF:2,\nseg");
    let mut data = include_bytes!("fixtures/media/fmp4_avc_regular/seg0.m4s").to_vec();
    data.push(0);
    assert_eq!(
        s.read(source(&data), ResourceRequest::media(&snapshot, 0).unwrap())
            .await
            .unwrap_err()
            .kind(),
        ResourceErrorKind::MediaValidation
    );
    let snapshot = crate::snapshot(
        "#EXT-X-KEY:METHOD=AES-128,URI=\"key\"\n#EXT-X-MAP:URI=\"init\"\n#EXTINF:2,\nseg",
    );
    assert_eq!(
        ResourceRequest::map(&snapshot, 0)
            .unwrap_err()
            .playlist_rejection(),
        Some(PlaylistRejection::MissingMapIv)
    );
}
#[cfg(feature = "default-source")]
#[tokio::test]
async fn native_file_limit_applies_before_loading_large_file_and_ranges_seek() {
    let folder = std::env::temp_dir().join(format!("hls-p3-{}", std::process::id()));
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join("seg");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(100_000_000).unwrap();
    drop(file);
    let snapshot = parse_playlist_snapshot(
        &TextResource {
            content: "#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:2,\nseg\n#EXT-X-ENDLIST\n".into(),
            location: SourceLocation::File(folder.join("list")),
        },
        PlaylistContext::new(InputId::new("p").unwrap(), 0),
    )
    .unwrap();
    let s = session(
        Arc::new(corpus::Provider),
        ResourceOptions::default().with_limits(1024, 2048, 2),
    );
    let error = s
        .read(
            Arc::new(hls_engine::legacy::ReqwestSource::new()),
            ResourceRequest::media(&snapshot, 0).unwrap(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ResourceErrorKind::ResourceTooLarge);
    use std::io::{Seek, Write};
    let encrypted = include_bytes!("fixtures/crypto/ts_avc_regular/seg0.cbc");
    let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.seek(std::io::SeekFrom::Start(90_000_000)).unwrap();
    file.write_all(encrypted).unwrap();
    drop(file);
    let snapshot = parse_playlist_snapshot(&TextResource {
        content: format!("#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:{}\n#EXT-X-KEY:METHOD=AES-128,URI=\"key\"\n#EXTINF:2,\n#EXT-X-BYTERANGE:{}@90000000\nseg\n#EXT-X-ENDLIST\n", corpus::SEQUENCE, encrypted.len()),
        location: SourceLocation::File(folder.join("list")),
    }, PlaylistContext::new(InputId::new("p").unwrap(), 0)).unwrap();
    let s = session(
        Arc::new(corpus::Provider),
        ResourceOptions::default()
            .with_limits(100_000, 200_000, 2)
            .with_encrypted_ranges(EncryptedRangePolicy::CompleteResources),
    );
    let output = s
        .read(
            Arc::new(hls_engine::legacy::ReqwestSource::new()),
            ResourceRequest::media(&snapshot, 0).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        output.bytes(),
        include_bytes!("fixtures/media/ts_avc_regular/seg0.ts")
    );
    std::fs::remove_dir_all(folder).unwrap();
}

#[tokio::test]
async fn resource_count_limit_shared_waiter_and_short_custom_range() {
    let provider = slow();
    let s = session(
        provider.clone(),
        ResourceOptions::default().with_limits(100_000, 300_000, 2),
    );
    let source = source(include_bytes!("fixtures/crypto/ts_avc_regular/seg0.cbc"));
    let mut a = Box::pin(s.read(source.clone(), encrypted()));
    let mut b = Box::pin(s.read(source.clone(), encrypted()));
    let mut cx = std::task::Context::from_waker(noop_waker_ref());
    assert!(a.poll_unpin(&mut cx).is_pending());
    assert!(b.poll_unpin(&mut cx).is_pending());
    assert_eq!(
        s.read(source, encrypted()).await.unwrap_err().kind(),
        ResourceErrorKind::BudgetExceeded
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    drop(a);
    assert_eq!(s.stats().resources(), 1);
    assert_eq!(provider.aborts.load(Ordering::SeqCst), 0);
    provider
        .replies
        .lock()
        .unwrap()
        .remove(0)
        .send(KeyResolution::Available(AvailableKey::aes128(
            SecretKey::new(corpus::KEY_A.to_vec()).unwrap(),
        )))
        .unwrap();
    assert_eq!(
        b.await.unwrap().bytes(),
        include_bytes!("fixtures/media/ts_avc_regular/seg0.ts")
    );
    let snapshot = snapshot("#EXTINF:2,\n#EXT-X-BYTERANGE:64@0\nseg");
    let source = Arc::new(ControlledSource {
        calls: Arc::new(AtomicUsize::new(0)),
        stops: Arc::new(AtomicUsize::new(0)),
        mode: 2,
    });
    assert_eq!(
        s.read(source, ResourceRequest::media(&snapshot, 0).unwrap())
            .await
            .unwrap_err()
            .kind(),
        ResourceErrorKind::InvalidRange
    );
}
fn noop_waker_ref() -> &'static std::task::Waker {
    futures_util::task::noop_waker_ref()
}

#[cfg(feature = "default-source")]
#[tokio::test]
async fn http_announced_size_limit_keeps_typed_resource_failure() {
    use std::io::{BufRead, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut reader = std::io::BufReader::new(socket.try_clone().unwrap());
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                break;
            }
        }
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 65536\r\nConnection: close\r\n\r\n")
            .unwrap();
    });
    let snapshot = parse_playlist_snapshot(
        &TextResource {
            content: "#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:2,\nseg\n#EXT-X-ENDLIST\n".into(),
            location: SourceLocation::Url(
                url::Url::parse(&format!("http://{address}/list")).unwrap(),
            ),
        },
        PlaylistContext::new(InputId::new("p").unwrap(), 0),
    )
    .unwrap();
    let s = session(
        Arc::new(corpus::Provider),
        ResourceOptions::default().with_limits(1024, 2048, 2),
    );
    let result = s
        .read(
            Arc::new(hls_engine::legacy::ReqwestSource::new()),
            ResourceRequest::media(&snapshot, 0).unwrap(),
        )
        .await;
    server.join().unwrap();
    assert_eq!(
        result.unwrap_err().kind(),
        ResourceErrorKind::ResourceTooLarge
    );
    assert_eq!(s.stats().reserved_bytes(), 0);
}
