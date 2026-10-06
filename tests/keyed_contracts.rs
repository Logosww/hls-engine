use futures_util::{FutureExt, task::noop_waker};
use hls_transmux::{
    crypto::{key::*, resource::*},
    playlist::*,
    *,
};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::Context,
};
#[path = "support/keyed_contracts.rs"]
mod suite;
#[tokio::test]
async fn supported_rejected_queries_and_exact_resource_progress() {
    suite::run(Arc::new(suite::fixtures::corpus::Provider)).await;
}
#[derive(Debug)]
struct NoIo(Arc<AtomicUsize>);
impl Source for NoIo {
    fn read_text<'a>(
        &'a self,
        _: &'a SourceLocation,
    ) -> Pin<Box<dyn Future<Output = Result<TextResource>> + Send + 'a>> {
        panic!("unexpected playlist read")
    }
    fn read_bytes<'a>(
        &'a self,
        _: &'a SourceLocation,
        _: Option<&'a ByteRange>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>>> + Send + 'a>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(Error::invalid("no I/O allowed")) })
    }
}
fn snapshot(text: &str) -> PlaylistSnapshot {
    parse_playlist_snapshot(&TextResource {content:text.into(),location:SourceLocation::Url(url::Url::parse("https://USER_SECRET:PASS_SECRET@example.test/list?TOKEN_SECRET#FRAGMENT_SECRET").unwrap())},PlaylistContext::new(InputId::new("INPUT_SECRET").unwrap(),7)).unwrap()
}
fn finite(body: &str) -> String {
    format!(
        "#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:9007199254740993\n{body}\n#EXT-X-ENDLIST\n"
    )
}
#[tokio::test]
async fn manifest_rejections_include_late_keys_and_ranges_before_any_io() {
    let calls = Arc::new(AtomicUsize::new(0));
    let unsupported = [
        finite("#EXT-X-KEY:METHOD=SAMPLE-AES-CTR,URI=\"key\"\n#EXTINF:2,\na"),
        finite("#EXT-X-KEY:METHOD=AES-256-GCM,URI=\"key\"\n#EXTINF:2,\na"),
        finite("#EXT-X-GAP\n#EXTINF:2,\na"),
        finite("#EXT-X-DISCONTINUITY\n#EXTINF:2,\na"),
        finite("#EXT-X-I-FRAMES-ONLY\n#EXTINF:2,\na"),
        finite("#EXT-X-PLAYLIST-TYPE:EVENT\n#EXTINF:2,\na"),
        finite("#EXTINF:2,\na").replace("#EXT-X-ENDLIST", ""),
        finite(
            "#EXTINF:2,\na\n#EXT-X-KEY:METHOD=AES-128,URI=\"key\",KEYFORMAT=\"unsupported\"\n#EXTINF:2,\nb",
        ),
        finite(
            "#EXTINF:2,\na\n#EXT-X-KEY:METHOD=AES-128,URI=\"key\"\n#EXTINF:2,\n#EXT-X-BYTERANGE:16@0\nb",
        ),
    ];
    for text in unsupported {
        let error = prepare_hls_with_keys(
            KeyedInputs::new(KeyedInput::new(
                snapshot(&text),
                Arc::new(NoIo(calls.clone())),
            )),
            suite::fixtures::keys(Arc::new(suite::fixtures::corpus::Provider)),
            KeyedPrepareOptions::default(),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.failure(), KeyedFailure::UnsupportedCombination);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    let text =
        finite("#EXT-X-KEY:METHOD=AES-128,URI=\"key\"\n#EXT-X-MAP:URI=\"init\"\n#EXTINF:2,\na");
    let error = prepare_hls_with_keys(
        KeyedInputs::new(KeyedInput::new(
            snapshot(&text),
            Arc::new(NoIo(calls.clone())),
        )),
        suite::fixtures::keys(Arc::new(suite::fixtures::corpus::Provider)),
        KeyedPrepareOptions::default(),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.failure(), KeyedFailure::InvalidIv);
    assert_eq!(
        error.resource_context().unwrap().kind(),
        KeyResourceKind::Map
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
#[derive(Debug)]
struct PrivateCause;
impl std::fmt::Display for PrivateCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PROVIDER_SECRET ?token=KEY_SECRET")
    }
}
impl std::error::Error for PrivateCause {}
struct Fault(u8);
impl KeyProvider for Fault {
    fn resolve(&self, _: KeyRequest) -> KeyFuture<KeyResolution> {
        let mode = self.0;
        Box::pin(async move {
            match mode {
                0 => KeyResolution::Unavailable,
                1 => KeyResolution::Failure(ProviderFailure::new(
                    ProviderFailureKind::Authorization,
                    Arc::new(PrivateCause),
                )),
                2 => KeyResolution::Available(AvailableKey::aes128(
                    SecretKey::new(vec![0; 16]).unwrap(),
                )),
                _ => KeyResolution::Available(
                    AvailableKey::aes128(
                        SecretKey::new(suite::fixtures::corpus::KEY_A.to_vec()).unwrap(),
                    )
                    .with_valid_until(0),
                ),
            }
        })
    }
}
fn safe_chain(error: &KeyedSessionError) -> String {
    let mut out = format!("{error:?} {error}");
    let mut cause = std::error::Error::source(error);
    while let Some(e) = cause {
        out.push_str(&format!(" {e:?} {e}"));
        cause = e.source();
    }
    for secret in [
        "USER_SECRET",
        "PASS_SECRET",
        "TOKEN_SECRET",
        "FRAGMENT_SECRET",
        "INPUT_SECRET",
        "PROVIDER_SECRET",
        "KEY_SECRET",
        "2b7e151628aed2a6abf7158809cf4f3c",
    ] {
        assert!(!out.contains(secret), "{secret}: {out}");
    }
    out
}
#[tokio::test]
async fn typed_safe_source_chain_preserves_raw_provider_cause() {
    for (mode, expected) in [
        (0, KeyedFailure::ProviderUnavailable),
        (1, KeyedFailure::ProviderFailure),
        (2, KeyedFailure::Decrypt),
        (3, KeyedFailure::KeyExpired),
    ] {
        let text = finite(
            "#EXT-X-KEY:METHOD=AES-128,URI=\"https://USER_SECRET:PASS_SECRET@keys.test/key?KEY_SECRET#FRAGMENT_SECRET\"\n#EXTINF:2,\nseg",
        );
        let snap = snapshot(&text);
        let source = MemorySource::new().segment(
            match snap.segments()[0].location().location() {
                SourceLocation::Url(url) => url.as_str(),
                _ => unreachable!(),
            },
            include_bytes!("fixtures/crypto/ts_avc_regular/seg0.cbc"),
        );
        let error = prepare_hls_with_keys(
            KeyedInputs::new(KeyedInput::new(snap, Arc::new(source))),
            suite::fixtures::keys(Arc::new(Fault(mode))),
            KeyedPrepareOptions::default(),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.failure(), expected);
        safe_chain(&error);
        assert_eq!(
            error.slot().unwrap().sequence(),
            suite::fixtures::corpus::SEQUENCE
        );
        assert!(error.track_id().is_none() && error.sample_index().is_none());
        if mode == 1 {
            let resource = std::error::Error::source(&error)
                .unwrap()
                .downcast_ref::<ResourceError>()
                .unwrap();
            let key = std::error::Error::source(resource)
                .unwrap()
                .downcast_ref::<KeyError>()
                .unwrap();
            let failure = std::error::Error::source(key)
                .unwrap()
                .downcast_ref::<ProviderFailure>()
                .unwrap();
            assert_eq!(failure.kind(), ProviderFailureKind::Authorization);
            assert!(failure.raw_cause().downcast_ref::<PrivateCause>().is_some());
        }
    }
}
struct Pending(Arc<AtomicUsize>);
impl KeyProvider for Pending {
    fn resolve(&self, _: KeyRequest) -> KeyFuture<KeyResolution> {
        Box::pin(std::future::pending())
    }
    fn abort(&self, _: &KeyRequest) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
#[tokio::test]
async fn downloaded_ciphertext_precedes_provider_wait_and_drop_stops_events() {
    let (_, inputs) = suite::fixtures::pair("ts_avc_regular", None, [true, false], false);
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let aborted = Arc::new(AtomicUsize::new(0));
    let mut future = Box::pin(prepare_hls_with_keys(
        inputs,
        suite::fixtures::keys(Arc::new(Pending(aborted.clone()))),
        KeyedPrepareOptions::default()
            .with_on_event(Arc::new(move |e| sink.lock().unwrap().push(e))),
    ));
    assert!(
        future
            .poll_unpin(&mut Context::from_waker(&noop_waker()))
            .is_pending()
    );
    let seen = events.lock().unwrap().clone();
    let p = &seen.last().unwrap().inputs()[0];
    assert_eq!(
        p.downloaded_bytes(),
        include_bytes!("fixtures/crypto/ts_avc_regular/seg0.cbc").len() as u64
    );
    assert_eq!(p.downloaded_segments(), 1);
    assert_eq!(p.decrypted_segments(), 0);
    assert_eq!(p.ready_segments(), 0);
    assert_eq!(p.committed_segments(), 0);
    drop(future);
    assert_eq!(aborted.load(Ordering::SeqCst), 1);
    assert_eq!(seen.len(), events.lock().unwrap().len());
}

#[tokio::test]
async fn decrypted_invalid_media_and_bad_padding_have_distinct_progress() {
    for (cipher, expected, decrypted) in [
        (
            &include_bytes!("fixtures/crypto/invalid-container.cbc")[..],
            KeyedFailure::MediaValidation,
            1,
        ),
        (
            &include_bytes!("fixtures/crypto/invalid-padding.cbc")[..],
            KeyedFailure::Decrypt,
            0,
        ),
    ] {
        let snap = snapshot(&finite(
            "#EXT-X-KEY:METHOD=AES-128,URI=\"key\",IV=0x000102030405060708090a0b0c0d0e0f\n#EXTINF:2,\nseg",
        ));
        let source =
            MemorySource::new().segment("https://USER_SECRET:PASS_SECRET@example.test/seg", cipher);
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let error = prepare_hls_with_keys(
            KeyedInputs::new(KeyedInput::new(snap, Arc::new(source))),
            suite::fixtures::keys(Arc::new(suite::fixtures::corpus::Provider)),
            KeyedPrepareOptions::default()
                .with_on_event(Arc::new(move |e| sink.lock().unwrap().push(e))),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.failure(), expected);
        safe_chain(&error);
        let events = events.lock().unwrap();
        let p = &events.last().unwrap().inputs()[0];
        assert_eq!(p.downloaded_segments(), 1);
        assert_eq!(p.downloaded_bytes(), cipher.len() as u64);
        assert_eq!(p.decrypted_segments(), decrypted);
        assert_eq!(p.ready_segments(), 0);
        assert_eq!(p.committed_segments(), 0);
        assert!(!format!("{events:?}").contains("SECRET"));
    }
}
#[tokio::test]
async fn successful_events_and_report_do_not_serialize_transport_secrets() {
    let text = include_str!("fixtures/crypto/ts_avc_regular/input.m3u8").replace(
        "key.bin",
        "https://USER_SECRET:PASS_SECRET@keys.test/key?KEY_SECRET#FRAGMENT_SECRET",
    );
    let source = MemorySource::new()
        .segment(
            "https://USER_SECRET:PASS_SECRET@example.test/seg0.cbc",
            include_bytes!("fixtures/crypto/ts_avc_regular/seg0.cbc"),
        )
        .segment(
            "https://USER_SECRET:PASS_SECRET@example.test/seg1.cbc",
            include_bytes!("fixtures/crypto/ts_avc_regular/seg1.cbc"),
        )
        .segment(
            "https://USER_SECRET:PASS_SECRET@example.test/clear.bin",
            include_bytes!("fixtures/media/ts_avc_regular/seg2.ts"),
        );
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let (_, report) = prepare_hls_with_keys(
        KeyedInputs::new(KeyedInput::new(snapshot(&text), Arc::new(source))),
        suite::fixtures::keys(Arc::new(suite::fixtures::corpus::Provider)),
        KeyedPrepareOptions::default()
            .with_on_event(Arc::new(move |e| sink.lock().unwrap().push(e))),
    )
    .await
    .unwrap()
    .into_mp4_bytes()
    .await
    .unwrap();
    let debug = format!("{report:?} {:?}", events.lock().unwrap());
    assert!(!debug.contains("SECRET"));
    let p = &report.inputs()[0];
    assert_eq!(
        p.downloaded_bytes(),
        (include_bytes!("fixtures/crypto/ts_avc_regular/seg0.cbc").len()
            + include_bytes!("fixtures/crypto/ts_avc_regular/seg1.cbc").len()
            + include_bytes!("fixtures/media/ts_avc_regular/seg2.ts").len()) as u64
    );
    assert_eq!(
        p.decrypted_bytes(),
        (include_bytes!("fixtures/media/ts_avc_regular/seg0.ts").len()
            + include_bytes!("fixtures/media/ts_avc_regular/seg1.ts").len()) as u64
    );
}
