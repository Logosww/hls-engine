use hls_engine::{crypto::key::*, playlist::*, *};
use std::sync::Arc;
#[cfg(feature = "experimental-gcm")]
#[allow(dead_code)]
#[path = "support/sample_crypto.rs"]
mod sample;

struct Clock;
impl KeyClock for Clock {
    fn now(&self) -> u64 {
        0
    }
}
struct Provider;
impl KeyProvider for Provider {
    fn resolve(&self, request: KeyRequest) -> KeyFuture<KeyResolution> {
        Box::pin(async move {
            #[cfg(feature = "experimental-gcm")]
            {
                let rotated = matches!(request.reference().location().location(), SourceLocation::Url(url) if url.path().ends_with("rotated"));
                let start = if rotated { 32 } else { 0 };
                KeyResolution::Available(
                    AvailableKey::aes256_gcm(
                        SecretKey::aes256((start..start + 32).collect()).unwrap(),
                    )
                    .with_version("fixture-v1"),
                )
            }
            #[cfg(not(feature = "experimental-gcm"))]
            {
                let _ = request;
                KeyResolution::Unavailable
            }
        })
    }
}
fn id() -> InputId {
    InputId::new("primary").unwrap()
}
fn snapshot(extra: &str, map: bool) -> PlaylistSnapshot {
    parse_playlist_snapshot(&TextResource {
        location: SourceLocation::Url(url::Url::parse("https://fixture.test/list").unwrap()),
        content: format!("#EXTM3U\n#EXT-X-TARGETDURATION:10\n#EXT-X-KEY:METHOD=AES-256-GCM,URI=\"key\"{extra}\n{}#EXTINF:2,\nmedia\n#EXT-X-ENDLIST\n",
            if map { "#EXT-X-MAP:URI=\"init\"\n" } else { "" }),
    }, PlaylistContext::new(id(), 0)).unwrap()
}
fn session(data: &[u8], init: Option<&[u8]>, enabled: bool) -> EngineSession {
    let mut source = MemorySource::new().segment("https://fixture.test/media", data);
    if let Some(init) = init {
        source = source.segment("https://fixture.test/init", init);
    }
    EngineSession::new(
        EngineInputs::new(
            EngineInput::new(id(), Arc::new(source)),
            EmbeddedAudio::Keep,
        ),
        KeySession::new(
            "op",
            "auth",
            Arc::new(Provider),
            Arc::new(Clock),
            KeySessionOptions::default(),
        )
        .unwrap(),
        EngineOptions::default().with_experimental_gcm(enabled),
    )
    .unwrap()
}
#[test]
fn gcm_requires_both_opt_ins() {
    let snap = snapshot("", false);
    let s = session(&[], None, false);
    assert_eq!(
        s.handle().accept_snapshot(&id(), &snap).unwrap_err().kind(),
        EngineErrorKind::UnsupportedPlaylist
    );
    #[cfg(not(feature = "experimental-gcm"))]
    assert_eq!(
        session(&[], None, true)
            .handle()
            .accept_snapshot(&id(), &snap)
            .unwrap_err()
            .kind(),
        EngineErrorKind::UnsupportedPlaylist
    );
}
#[test]
fn explicit_iv_is_rejected_during_admission() {
    let snap = snapshot(",IV=0x01", false);
    let error = session(&[], None, true)
        .handle()
        .accept_snapshot(&id(), &snap)
        .unwrap_err();
    assert_eq!(error.kind(), EngineErrorKind::UnsupportedPlaylist);
}

#[cfg(feature = "experimental-gcm")]
#[tokio::test]
async fn independent_ts_fmp4_and_packed_resources_execute() {
    for (data, init, clear, clear_init) in [
        (
            include_bytes!("fixtures/gcm/resource-0.gcm").as_slice(),
            None,
            include_bytes!("fixtures/media/ts_avc_regular/seg0.ts").as_slice(),
            None,
        ),
        (
            include_bytes!("fixtures/gcm/resource-2.gcm").as_slice(),
            Some(include_bytes!("fixtures/gcm/resource-1.gcm").as_slice()),
            include_bytes!("fixtures/media/fmp4_avc_regular/seg0.m4s").as_slice(),
            Some(include_bytes!("fixtures/media/fmp4_avc_regular/init.fmp4").as_slice()),
        ),
        (
            include_bytes!("fixtures/gcm/resource-3.gcm").as_slice(),
            None,
            include_bytes!("fixtures/packed_aac/clear/seg0.bin").as_slice(),
            None,
        ),
    ] {
        for format in [OutputFormat::FragmentedMp4, OutputFormat::Mp4] {
            let s = session(data, init, true);
            s.handle()
                .accept_snapshot(&id(), &snapshot("", init.is_some()))
                .unwrap();
            let (bytes, report) = s.into_bytes(8 * 1024 * 1024, format).await.unwrap();
            assert!(!bytes.is_empty());
            assert!(report.tracks().iter().all(|t| t.sample_count() > 0));
            let s = session(clear, clear_init, false);
            let snap = parse_playlist_snapshot(
                &TextResource {
                    location: SourceLocation::Url(
                        url::Url::parse("https://fixture.test/list").unwrap(),
                    ),
                    content: format!(
                        "#EXTM3U\n#EXT-X-TARGETDURATION:10\n{}#EXTINF:2,\nmedia\n#EXT-X-ENDLIST\n",
                        if clear_init.is_some() {
                            "#EXT-X-MAP:URI=\"init\"\n"
                        } else {
                            ""
                        }
                    ),
                },
                PlaylistContext::new(id(), 0),
            )
            .unwrap();
            s.handle().accept_snapshot(&id(), &snap).unwrap();
            let (expected, _) = s.into_bytes(8 * 1024 * 1024, format).await.unwrap();
            assert_eq!(sample::canonical(bytes), sample::canonical(expected));
        }
    }
}
#[cfg(feature = "experimental-gcm")]
#[tokio::test]
async fn bad_authentication_never_writes_plaintext() {
    use hls_engine::crypto::resource::ResourceErrorKind;
    let fixture = include_bytes!("fixtures/gcm/resource-0.gcm");
    for offset in [0, 16, fixture.len() - 1] {
        let mut damaged = fixture.to_vec();
        damaged[offset] ^= 1;
        let s = session(&damaged, None, true);
        s.handle()
            .accept_snapshot(&id(), &snapshot("", false))
            .unwrap();
        let mut writer = Vec::new();
        let error = s.write_to(&mut writer).await.unwrap_err();
        assert_eq!(error.kind(), EngineErrorKind::AuthenticationFailed);
        assert_eq!(error.input_id(), Some(&id()));
        assert_eq!(error.epoch(), Some(0));
        assert!(matches!(error.cause(), Some(EngineCause::Resource(_))));
        assert_eq!(
            error.resource_error().unwrap().kind(),
            ResourceErrorKind::AuthenticationFailed
        );
        assert!(writer.is_empty());
    }
}

#[cfg(feature = "experimental-gcm")]
#[path = "support/engine_gcm.rs"]
mod runtime;
#[cfg(feature = "experimental-gcm")]
#[tokio::test]
async fn rotation_range_and_open_input_match_clear_output() {
    assert_eq!(
        runtime::suite().await["cases"].as_array().unwrap().len(),
        16
    );
}

#[cfg(feature = "experimental-gcm")]
#[tokio::test]
async fn encrypted_ranges_require_complete_authenticated_resources() {
    use hls_engine::crypto::resource::{EncryptedRangePolicy, ResourceErrorKind, ResourceOptions};
    let encrypted = include_bytes!("fixtures/gcm/resource-0.gcm");
    let mut resource = vec![0; 7];
    resource.extend_from_slice(encrypted);
    resource.extend_from_slice(&[0; 9]);
    for (attest, trim) in [(false, 0), (true, 0), (true, 1)] {
        let source = MemorySource::new().segment("https://fixture.test/media", resource.as_slice());
        let mut resources = ResourceOptions::default().with_experimental_gcm(true);
        if attest {
            resources = resources.with_encrypted_ranges(EncryptedRangePolicy::CompleteResources);
        }
        let session = EngineSession::new(
            EngineInputs::new(
                EngineInput::new(id(), Arc::new(source)),
                EmbeddedAudio::Keep,
            ),
            KeySession::new(
                "ranges",
                "auth",
                Arc::new(Provider),
                Arc::new(Clock),
                KeySessionOptions::default(),
            )
            .unwrap(),
            EngineOptions::default().with_resources(resources),
        )
        .unwrap();
        let snapshot = parse_playlist_snapshot(&TextResource {
            location: SourceLocation::Url(url::Url::parse("https://fixture.test/list").unwrap()),
            content: format!("#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-KEY:METHOD=AES-256-GCM,URI=\"key\"\n#EXTINF:2,\n#EXT-X-BYTERANGE:{}@7\nmedia\n#EXT-X-ENDLIST\n", encrypted.len() - trim),
        }, PlaylistContext::new(id(), 0)).unwrap();
        session.handle().accept_snapshot(&id(), &snapshot).unwrap();
        let mut writer = Vec::new();
        let result = session.write_to(&mut writer).await;
        if attest && trim == 0 {
            result.unwrap();
            assert!(!writer.is_empty());
        } else {
            let expected = if attest {
                ResourceErrorKind::AuthenticationFailed
            } else {
                ResourceErrorKind::UnconfirmedRange
            };
            assert_eq!(
                result.unwrap_err().resource_error().unwrap().kind(),
                expected
            );
            assert!(writer.is_empty());
        }
    }
}
