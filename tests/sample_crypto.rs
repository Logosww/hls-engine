#[path = "support/sample_crypto.rs"]
mod suite;
#[tokio::test]
async fn external_packager_clear_samples_and_timelines_match() {
    assert_eq!(
        suite::run(std::sync::Arc::new(suite::Provider)).await["cases"]
            .as_array()
            .unwrap()
            .len(),
        21
    );
}

use hls_transmux::{
    crypto::{key::*, resource::*, sample::*},
    *,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
struct Counting(AtomicUsize);
impl KeyProvider for Counting {
    fn resolve(&self, r: KeyRequest) -> KeyFuture<KeyResolution> {
        self.0.fetch_add(1, Ordering::SeqCst);
        suite::Provider.resolve(r)
    }
}
fn replace_box(bytes: &mut [u8], name: &[u8; 4], replacement: &[u8; 4]) {
    let moof = bytes.windows(4).position(|b| b == b"moof").unwrap();
    let size = u32::from_be_bytes(bytes[moof - 4..moof].try_into().unwrap()) as usize;
    let pos = bytes[moof..moof - 4 + size]
        .windows(4)
        .position(|b| b == name)
        .unwrap()
        + moof;
    bytes[pos..pos + 4].copy_from_slice(replacement);
}
#[tokio::test]
async fn external_auxiliary_only_and_inline_only_match() {
    let case = suite::cases()
        .into_iter()
        .find(|c| c.name == "fmp4_avc_cenc")
        .unwrap();
    let source = case.files.iter().find(|(n, _)| *n == "seg1.m4s").unwrap().1;
    let clear = prepare_hls_with_keys(
        KeyedInputs::new(suite::input("fmp4_avc_clear", "primary")),
        suite::keys(Arc::new(suite::Provider)),
        KeyedPrepareOptions::default(),
    )
    .await
    .unwrap()
    .into_mp4_bytes()
    .await
    .unwrap()
    .0;
    for auxiliary in [true, false] {
        let mut bytes = source.to_vec();
        if auxiliary {
            replace_box(&mut bytes, b"senc", b"free")
        } else {
            replace_box(&mut bytes, b"saiz", b"free");
            replace_box(&mut bytes, b"saio", b"free")
        }
        let input = suite::input_modified(
            &case,
            "primary",
            case.playlist.into(),
            Some(("seg1.m4s", bytes)),
        );
        let output = prepare_hls_with_keys(
            KeyedInputs::new(input),
            suite::keys(Arc::new(suite::Provider)),
            KeyedPrepareOptions::default(),
        )
        .await
        .unwrap()
        .into_mp4_bytes()
        .await
        .unwrap()
        .0;
        assert_eq!(suite::canonical(output), suite::canonical(clear.clone()));
    }
}
#[tokio::test]
async fn sample_budget_and_scheme_mismatch_do_not_request_keys() {
    for name in ["fmp4_avc_cenc", "ts_avc_sample"] {
        let provider = Arc::new(Counting(AtomicUsize::new(0)));
        let error = prepare_hls_with_keys(
            KeyedInputs::new(suite::input(name, "primary")),
            suite::keys(provider.clone()),
            KeyedPrepareOptions::default()
                .with_resources(ResourceOptions::default().with_sample_limit(1)),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(
            error.sample_error().unwrap().kind(),
            SampleErrorKind::BudgetExceeded
        );
        assert_eq!(provider.0.load(Ordering::SeqCst), 0);
    }
    let case = suite::cases()
        .into_iter()
        .find(|c| c.name == "fmp4_avc_cenc")
        .unwrap();
    let provider = Arc::new(Counting(AtomicUsize::new(0)));
    let input = suite::input_modified(
        &case,
        "primary",
        case.playlist.replace("SAMPLE-AES-CTR", "SAMPLE-AES"),
        None,
    );
    let error = prepare_hls_with_keys(
        KeyedInputs::new(input),
        suite::keys(provider.clone()),
        KeyedPrepareOptions::default(),
    )
    .await
    .err()
    .unwrap();
    let sample = error.sample_error().unwrap();
    assert_eq!(sample.kind(), SampleErrorKind::InvalidMetadata);
    assert_eq!(sample.track_id(), Some(1));
    assert_eq!(sample.sample_index(), Some(0));
    assert_eq!(provider.0.load(Ordering::SeqCst), 0);
}

struct PendingProvider {
    calls: AtomicUsize,
    aborts: AtomicUsize,
}
impl KeyProvider for PendingProvider {
    fn resolve(&self, _: KeyRequest) -> KeyFuture<KeyResolution> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::pending())
    }
    fn abort(&self, _: &KeyRequest) {
        self.aborts.fetch_add(1, Ordering::SeqCst);
    }
}
#[tokio::test]
async fn dropping_sample_key_wait_aborts_provider_and_cancels_native_send_future() {
    use futures_util::{FutureExt, task::noop_waker};
    use std::task::{Context, Poll};
    for name in ["fmp4_avc_cenc", "ts_avc_sample"] {
        let provider = Arc::new(PendingProvider {
            calls: AtomicUsize::new(0),
            aborts: AtomicUsize::new(0),
        });
        let mut future = Box::pin(prepare_hls_with_keys(
            KeyedInputs::new(suite::input(name, "primary")),
            suite::keys(provider.clone()),
            KeyedPrepareOptions::default(),
        ));
        fn assert_send<T: Send>(_: &T) {}
        assert_send(&future);
        assert!(matches!(
            future.poll_unpin(&mut Context::from_waker(&noop_waker())),
            Poll::Pending
        ));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        drop(future);
        assert_eq!(provider.aborts.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn external_audio_mixed_schemes_and_fragmented_writer_match() {
    for (video, audio) in [
        ("fmp4_avc_cenc", "fmp4_aac_cbcs"),
        ("ts_avc_sample", "fmp4_aac_cenc"),
        ("fmp4_hevc_cbcs", "ts_aac_sample"),
    ] {
        let mut outputs = Vec::new();
        for clear in [true, false] {
            let v = if clear {
                format!("{}_clear", video.rsplit_once('_').unwrap().0)
            } else {
                video.into()
            };
            let a = if clear {
                format!("{}_clear", audio.rsplit_once('_').unwrap().0)
            } else {
                audio.into()
            };
            let inputs =
                KeyedInputs::new(suite::input(&v, "primary")).with_audio(suite::input(&a, "audio"));
            let session = prepare_hls_with_keys(
                inputs,
                suite::keys(Arc::new(suite::Provider)),
                KeyedPrepareOptions::default(),
            )
            .await
            .unwrap();
            let mut bytes = Vec::new();
            session.write_to(&mut bytes).await.unwrap();
            outputs.push(suite::canonical(bytes));
        }
        assert!(outputs[0] == outputs[1], "{video}/{audio}");
    }
}
#[tokio::test]
async fn native_output_modes_and_finalizers_preserve_clear_samples() {
    for name in [
        "fmp4_avc_cenc",
        "fmp4_hevc_cbcs",
        "fmp4_aac_cenc",
        "ts_avc_sample",
        "ts_aac_sample",
    ] {
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
                if backend != FinalizeBackend::Native && format != OutputFormat::StreamingMp4 {
                    continue;
                }
                let mut outputs = Vec::new();
                for clear in [true, false] {
                    let case = if clear {
                        format!("{}_clear", name.rsplit_once('_').unwrap().0)
                    } else {
                        name.into()
                    };
                    let path = std::env::temp_dir().join(format!(
                        "sample-{}-{name}-{format:?}-{backend:?}-{clear}.mp4",
                        std::process::id()
                    ));
                    prepare_hls_with_keys(
                        KeyedInputs::new(suite::input(&case, "primary")),
                        suite::keys(Arc::new(suite::Provider)),
                        KeyedPrepareOptions::default(),
                    )
                    .await
                    .unwrap()
                    .write_to_file(
                        &path,
                        FileOutputOptions::default()
                            .with_format(format)
                            .with_finalize_backend(backend),
                    )
                    .await
                    .unwrap();
                    outputs.push(suite::canonical(std::fs::read(&path).unwrap()));
                    if !clear && let Ok(directory) = std::env::var("HLS_SAMPLE_OUTPUT") {
                        std::fs::create_dir_all(&directory).unwrap();
                        std::fs::copy(
                            &path,
                            format!("{directory}/{name}-{format:?}-{backend:?}.mp4"),
                        )
                        .unwrap();
                    }
                    std::fs::remove_file(path).unwrap();
                }
                assert!(outputs[0] == outputs[1], "{name}/{format:?}/{backend:?}");
            }
        }
    }
}

fn epoch_input(encrypted: bool, change_codec: bool, gap: bool) -> KeyedInputs {
    let first = if encrypted {
        "fmp4_avc_cenc"
    } else {
        "fmp4_avc_clear"
    };
    let second = if change_codec {
        if encrypted {
            "fmp4_hevc_cbcs"
        } else {
            "fmp4_hevc_clear"
        }
    } else {
        first
    };
    let mut source = MemorySource::new();
    for name in [first, second] {
        let case = suite::cases().into_iter().find(|c| c.name == name).unwrap();
        for (file, bytes) in case.files {
            source = source.segment(format!("https://epochs.test/{name}/{file}"), *bytes);
        }
    }
    let key = |name: &str| {
        if encrypted {
            format!(
                "#EXT-X-KEY:METHOD={},URI=\"https://epochs.test/key\"\n",
                if name.ends_with("cbcs") {
                    "SAMPLE-AES"
                } else {
                    "SAMPLE-AES-CTR"
                }
            )
        } else {
            String::new()
        }
    };
    let text = format!(
        "#EXTM3U\n#EXT-X-TARGETDURATION:4\n{}#EXT-X-MAP:URI=\"https://epochs.test/{first}/init.mp4\"\n#EXTINF:2,\nhttps://epochs.test/{first}/seg1.m4s\n{}#EXT-X-DISCONTINUITY\n{}#EXT-X-MAP:URI=\"https://epochs.test/{second}/init.mp4\"\n#EXTINF:{},\nhttps://epochs.test/{second}/seg1.m4s\n#EXTINF:2,\nhttps://epochs.test/{second}/seg2.m4s\n#EXT-X-ENDLIST\n",
        key(first),
        if gap {
            "#EXT-X-GAP\n#EXTINF:2,\nhttps://epochs.test/missing.m4s\n"
        } else {
            ""
        },
        key(second),
        if change_codec { "3.967" } else { "2" }
    );
    let snapshot = parse_playlist_snapshot(
        &TextResource {
            location: SourceLocation::Url(
                url::Url::parse("https://epochs.test/list.m3u8").unwrap(),
            ),
            content: text,
        },
        playlist::PlaylistContext::new(playlist::InputId::new("primary").unwrap(), 0),
    )
    .unwrap();
    KeyedInputs::new(KeyedInput::new(snapshot, Arc::new(source)))
}
#[tokio::test]
async fn encrypted_epochs_gaps_configuration_split_and_redeclaration_match_clear() {
    for (change, gap) in [(false, false), (false, true), (true, false)] {
        let mut results = Vec::new();
        for encrypted in [false, true] {
            let mut options = TimelinePrepareOptions::default()
                .with_change_policy(TimelineChangePolicy::Split)
                .with_gap_policy(if change {
                    GapPolicy::Preserve
                } else {
                    GapPolicy::Collapse
                });
            if change {
                // Preserve the independent HEVC corpus presentation holes: collapsing
                // these across reordered pictures would make DTS non-monotonic.
                // The two independent packager sources have different composition origins.
                options = options.with_epoch_anchor(EpochAnchor::new(
                    playlist::InputId::new("primary").unwrap(),
                    1,
                    MediaTime::new(0, 1).unwrap(),
                    MediaTime::new(3, 1).unwrap(),
                ));
            }
            let (outputs, report) = prepare_hls_timeline(
                epoch_input(encrypted, change, gap),
                suite::keys(Arc::new(suite::Provider)),
                options,
            )
            .await
            .unwrap()
            .into_mp4_outputs()
            .await
            .unwrap_or_else(|e| panic!("encrypted={encrypted},change={change},gap={gap}: {e:?}"));
            assert_eq!(outputs.len(), if change { 2 } else { 1 });
            results.push((
                outputs
                    .into_iter()
                    .map(suite::canonical)
                    .collect::<Vec<_>>(),
                report,
            ));
        }
        assert!(results[0].0 == results[1].0, "change={change}, gap={gap}");
    }
}

#[tokio::test]
async fn reordered_picture_holes_cannot_be_silently_collapsed() {
    for encrypted in [false, true] {
        let prepared = prepare_hls_timeline(
            epoch_input(encrypted, true, false),
            suite::keys(Arc::new(suite::Provider)),
            TimelinePrepareOptions::default()
                .with_change_policy(TimelineChangePolicy::Split)
                .with_gap_policy(GapPolicy::Collapse),
        )
        .await
        .unwrap();
        assert_eq!(
            prepared.into_mp4_outputs().await.unwrap_err().kind(),
            TimelineErrorKind::TimelineAmbiguous
        );
    }
}

#[tokio::test]
async fn unchanged_ciphertext_with_expired_replaced_key_is_resource_changed() {
    use std::{future::Future, pin::Pin};
    #[derive(Debug)]
    struct Time(AtomicUsize);
    impl KeyClock for Time {
        fn now(&self) -> u64 {
            self.0.load(Ordering::SeqCst) as u64
        }
    }
    struct Rotating(Arc<Time>);
    impl KeyProvider for Rotating {
        fn resolve(&self, request: KeyRequest) -> KeyFuture<KeyResolution> {
            let now = self.0.now();
            Box::pin(async move {
                let key = if now == 0 { suite::KEY } else { [7; 16] };
                KeyResolution::Available(
                    AvailableKey::sample_aes_ctr(SecretKey::new(key.to_vec()).unwrap())
                        .with_kid(request.resource().kid().unwrap())
                        .with_version(format!("revision-{now}"))
                        .with_valid_until(now + 1),
                )
            })
        }
    }
    #[derive(Debug)]
    struct Replaying {
        source: MemorySource,
        time: Arc<Time>,
        reads: AtomicUsize,
    }
    impl Source for Replaying {
        fn read_text<'a>(
            &'a self,
            _: &'a SourceLocation,
        ) -> Pin<Box<dyn Future<Output = Result<TextResource>> + Send + 'a>> {
            panic!("snapshot supplied")
        }
        fn read_bytes<'a>(
            &'a self,
            location: &'a SourceLocation,
            range: Option<&'a ByteRange>,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>>> + Send + 'a>> {
            Box::pin(async move {
                if matches!(location, SourceLocation::Url(u) if u.path().ends_with("seg1.m4s"))
                    && self.reads.fetch_add(1, Ordering::SeqCst) > 0
                {
                    self.time.0.store(2, Ordering::SeqCst);
                }
                self.source.read_bytes(location, range).await
            })
        }
    }
    let case = suite::cases()
        .into_iter()
        .find(|c| c.name == "fmp4_aac_cenc")
        .unwrap();
    let mut source = MemorySource::new();
    for (file, bytes) in case.files {
        source = source.segment(format!("https://rotation.test/{file}"), *bytes);
    }
    let snapshot = parse_playlist_snapshot(
        &TextResource {
            location: SourceLocation::Url(
                url::Url::parse("https://rotation.test/input.m3u8").unwrap(),
            ),
            content: case.playlist.into(),
        },
        playlist::PlaylistContext::new(playlist::InputId::new("primary").unwrap(), 0),
    )
    .unwrap();
    let time = Arc::new(Time(AtomicUsize::new(0)));
    let keys = KeySession::new(
        "test",
        "rotation",
        Arc::new(Rotating(time.clone())),
        time.clone(),
        KeySessionOptions::default(),
    )
    .unwrap();
    let source = Arc::new(Replaying {
        source,
        time,
        reads: AtomicUsize::new(0),
    });
    let session = prepare_hls_timeline(
        KeyedInputs::new(KeyedInput::new(snapshot, source)),
        keys,
        TimelinePrepareOptions::default(),
    )
    .await
    .unwrap();
    let error = session.into_mp4_bytes().await.unwrap_err();
    assert_eq!(error.kind(), TimelineErrorKind::ResourceChanged);
    assert!(error.completed_outputs().is_empty());
}

#[tokio::test]
async fn clear_samples_skip_provider_and_resource_methods_rotate() {
    let clear = suite::cases()
        .into_iter()
        .find(|c| c.name == "fmp4_avc_clear")
        .unwrap();
    for method in ["SAMPLE-AES", "SAMPLE-AES-CTR"] {
        let provider = Arc::new(Counting(AtomicUsize::new(0)));
        let text = clear.playlist.replace(
            "#EXT-X-MAP",
            &format!("#EXT-X-KEY:METHOD={method},URI=\"key\"\n#EXT-X-MAP"),
        );
        let session = prepare_hls_with_keys(
            KeyedInputs::new(suite::input_modified(&clear, "primary", text, None)),
            suite::keys(provider.clone()),
            KeyedPrepareOptions::default(),
        )
        .await
        .unwrap();
        session.into_mp4_bytes().await.unwrap();
        assert_eq!(provider.0.load(Ordering::SeqCst), 0);
    }
    let mut source = MemorySource::new();
    for name in ["fmp4_avc_cenc", "fmp4_avc_clear"] {
        let case = suite::cases().into_iter().find(|c| c.name == name).unwrap();
        for (file, bytes) in case.files {
            source = source.segment(format!("https://mixed.test/{name}/{file}"), *bytes);
        }
    }
    source = source
        .segment(
            "https://mixed.test/aes-init.mp4",
            include_bytes!("fixtures/sample_crypto/mixed/aes-init.mp4").as_slice(),
        )
        .segment(
            "https://mixed.test/aes-seg3.m4s",
            include_bytes!("fixtures/sample_crypto/mixed/aes-seg3.m4s").as_slice(),
        );
    let text = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-KEY:METHOD=SAMPLE-AES-CTR,URI=\"key\"\n#EXT-X-MAP:URI=\"fmp4_avc_cenc/init.mp4\"\n#EXTINF:2,\nfmp4_avc_cenc/seg1.m4s\n#EXT-X-KEY:METHOD=NONE\n#EXT-X-MAP:URI=\"fmp4_avc_clear/init.mp4\"\n#EXTINF:2,\nfmp4_avc_clear/seg2.m4s\n#EXT-X-KEY:METHOD=AES-128,URI=\"key\",IV=0x000102030405060708090a0b0c0d0e0f\n#EXT-X-MAP:URI=\"aes-init.mp4\"\n#EXTINF:2,\naes-seg3.m4s\n#EXT-X-ENDLIST\n";
    let snapshot = parse_playlist_snapshot(
        &TextResource {
            location: SourceLocation::Url(
                url::Url::parse("https://mixed.test/input.m3u8").unwrap(),
            ),
            content: text.into(),
        },
        playlist::PlaylistContext::new(playlist::InputId::new("primary").unwrap(), 0),
    )
    .unwrap();
    let mixed = prepare_hls_timeline(
        KeyedInputs::new(KeyedInput::new(snapshot, Arc::new(source))),
        suite::keys(Arc::new(suite::Provider)),
        TimelinePrepareOptions::default(),
    )
    .await
    .unwrap()
    .into_mp4_bytes()
    .await
    .unwrap()
    .0;
    let original = prepare_hls_timeline(
        KeyedInputs::new(suite::input("fmp4_avc_clear", "primary")),
        suite::keys(Arc::new(suite::Provider)),
        TimelinePrepareOptions::default(),
    )
    .await
    .unwrap()
    .into_mp4_bytes()
    .await
    .unwrap()
    .0;
    assert_eq!(suite::canonical(mixed), suite::canonical(original));
}
