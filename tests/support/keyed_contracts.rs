use hls_engine::legacy::{capabilities::*, crypto::key::*, *};
use std::sync::{Arc, Mutex};
#[path = "keyed_corpus.rs"]
#[allow(dead_code)]
pub mod fixtures;

pub fn queries() -> usize {
    let layouts = vec![
        vec![KeyedCodec::Avc],
        vec![KeyedCodec::Hevc],
        vec![KeyedCodec::AacLc],
        vec![KeyedCodec::Avc, KeyedCodec::AacLc],
        vec![KeyedCodec::Hevc, KeyedCodec::AacLc],
    ];
    let mut accepted = 0;
    for container in [
        KeyedContainer::TransportStream,
        KeyedContainer::FragmentedMp4,
    ] {
        for encryption in [
            KeyedEncryption::Clear,
            KeyedEncryption::Aes128,
            KeyedEncryption::ClearAndAes128,
        ] {
            for codecs in &layouts {
                for output in [KeyedOutput::Mp4Bytes, KeyedOutput::FragmentedWriter] {
                    let query = KeyedCapabilityQuery::new(
                        KeyedInputCapability::new(container, encryption, codecs.clone()),
                        output,
                    );
                    let decision = query_keyed_capability(&query);
                    assert!(decision.supported());
                    assert!(
                        decision
                            .requirements()
                            .contains(&CapabilityRequirement::ContainerAndCodecValidation)
                    );
                    accepted += 1;
                }
            }
        }
    }
    let input = KeyedInputCapability::new(
        KeyedContainer::TransportStream,
        KeyedEncryption::Aes128,
        vec![KeyedCodec::Avc],
    );
    let base = KeyedCapabilityQuery::new(input.clone(), KeyedOutput::Mp4Bytes);
    use CapabilityDimension as D;
    let cases = vec![
        (
            KeyedCapabilityQuery::new(
                KeyedInputCapability::new(
                    KeyedContainer::PackedAac,
                    KeyedEncryption::Clear,
                    vec![KeyedCodec::AacLc],
                ),
                KeyedOutput::Mp4Bytes,
            ),
            D::Container,
        ),
        (
            KeyedCapabilityQuery::new(
                KeyedInputCapability::new(
                    KeyedContainer::TransportStream,
                    KeyedEncryption::SampleAesCtr,
                    vec![KeyedCodec::Avc],
                ),
                KeyedOutput::Mp4Bytes,
            ),
            D::Encryption,
        ),
        (
            KeyedCapabilityQuery::new(
                KeyedInputCapability::new(
                    KeyedContainer::FragmentedMp4,
                    KeyedEncryption::Aes256Gcm,
                    vec![KeyedCodec::Avc],
                ),
                KeyedOutput::Mp4Bytes,
            ),
            D::Encryption,
        ),
        (
            KeyedCapabilityQuery::new(
                KeyedInputCapability::new(
                    KeyedContainer::TransportStream,
                    KeyedEncryption::Clear,
                    vec![KeyedCodec::Other],
                ),
                KeyedOutput::Mp4Bytes,
            ),
            D::Codec,
        ),
        (
            KeyedCapabilityQuery::new(
                input.clone().with_scheme(KeyedProtectionScheme::Cbcs),
                KeyedOutput::Mp4Bytes,
            ),
            D::ProtectionScheme,
        ),
        (
            KeyedCapabilityQuery::new(
                input.clone().with_key_source(KeyedKeySource::None),
                KeyedOutput::Mp4Bytes,
            ),
            D::KeySource,
        ),
        (
            KeyedCapabilityQuery::new(
                input.clone().with_key_source(KeyedKeySource::Cdm),
                KeyedOutput::Mp4Bytes,
            ),
            D::KeySource,
        ),
        (
            KeyedCapabilityQuery::new(
                input.clone().with_key_source(KeyedKeySource::BuiltInHttp),
                KeyedOutput::Mp4Bytes,
            ),
            D::KeySource,
        ),
        (
            base.clone().with_source_mode(KeyedSourceMode::Live),
            D::SourceMode,
        ),
        (
            base.clone().with_source_mode(KeyedSourceMode::Event),
            D::SourceMode,
        ),
        (
            base.clone()
                .with_source_mode(KeyedSourceMode::RewrittenSnapshot),
            D::SourceMode,
        ),
        (
            base.clone()
                .with_range(KeyedRange::ArbitraryEncryptedRanges),
            D::Range,
        ),
        (
            base.clone().with_range(KeyedRange::PresentationRange),
            D::Range,
        ),
        (base.clone().with_range(KeyedRange::IFrameRanges), D::Range),
        (base.clone().with_resume(true), D::Resume),
        (base.clone().with_multitrack(true), D::TrackSelection),
        (base.clone().with_audio(input.clone()), D::TrackSelection),
        (base.clone().with_subtitles(true), D::Subtitles),
        (base.clone().with_experimental(true), D::Experimental),
        (base.clone().with_timeline_changes(true), D::Timeline),
    ];
    for (query, dimension) in &cases {
        let result = query_keyed_capability(query);
        assert!(!result.supported());
        assert!(
            result
                .rejections()
                .iter()
                .any(|r| r.dimension() == *dimension)
        );
    }
    for output in [
        KeyedOutput::Mp4File,
        KeyedOutput::FragmentedFile,
        KeyedOutput::NativeStreamingFile,
        KeyedOutput::FfmpegStreamingFile,
    ] {
        let result = query_keyed_capability(&KeyedCapabilityQuery::new(input.clone(), output));
        let expected = !cfg!(target_arch = "wasm32")
            && (output != KeyedOutput::FfmpegStreamingFile || cfg!(feature = "ffmpeg-finalize"));
        assert_eq!(result.supported(), expected);
        if !expected {
            assert!(
                result
                    .rejections()
                    .iter()
                    .any(|r| r.dimension() == D::Output)
            );
        }
    }
    assert_eq!(accepted, 60);
    assert_eq!(cases.len(), 20);
    accepted + cases.len()
}
/// Runs unchanged in native, Node/WASM and Chrome/WASM.
pub async fn run(provider: Arc<dyn KeyProvider>) -> serde_json::Value {
    let queries = queries();
    let mut reports = Vec::new();
    for name in ["ts_avc_regular", "fmp4_avc_regular"] {
        for encrypted in [false, true] {
            let events = Arc::new(Mutex::new(Vec::new()));
            let sink = events.clone();
            let (_, inputs) = fixtures::pair(name, None, [encrypted, false], true);
            let session = prepare_hls_with_keys(
                inputs,
                fixtures::keys(provider.clone()),
                fixtures::options().with_on_event(Arc::new(move |e| sink.lock().unwrap().push(e))),
            )
            .await
            .unwrap();
            let progress = session.progress();
            assert_eq!(progress[0].discovered_segments(), 3);
            assert_eq!(progress[0].downloaded_segments(), 1);
            assert_eq!(progress[0].committed_segments(), 0);
            let mut writer = Vec::new();
            let report = session.write_to(&mut writer).await.unwrap();
            assert!(query_keyed_capability(report.capability_query()).supported());
            assert_eq!(
                report.capability_query().output(),
                KeyedOutput::FragmentedWriter
            );
            let p = &report.inputs()[0];
            assert_eq!(p.downloaded_segments(), 3);
            assert_eq!(p.ready_segments(), 3);
            assert_eq!(p.committed_segments(), 3);
            assert_eq!(p.decrypted_segments(), if encrypted { 2 } else { 0 });
            if encrypted {
                assert!(p.downloaded_bytes() > p.media().clear_bytes());
                assert!(p.decrypted_bytes() < p.media().clear_bytes());
            } else {
                assert_eq!(p.downloaded_bytes(), p.media().clear_bytes());
                assert_eq!(p.decrypted_bytes(), 0);
            }
            let fmp4 = name.starts_with("fmp4");
            assert_eq!(p.maps().downloaded_resources(), usize::from(fmp4));
            assert_eq!(
                p.maps().decrypted_resources(),
                usize::from(fmp4 && encrypted)
            );
            assert_eq!(p.maps().cache_reuses(), if fmp4 { 2 } else { 0 });
            let events = events.lock().unwrap();
            assert_eq!(events.last().unwrap().phase(), KeyedSessionPhase::Completed);
            let mut previous = (0, 0, 0, 0);
            for event in events.iter() {
                let p = &event.inputs()[0];
                let current = (
                    p.downloaded_segments(),
                    p.decrypted_segments(),
                    p.ready_segments(),
                    p.committed_segments(),
                );
                assert!(
                    current.0 >= previous.0
                        && current.1 >= previous.1
                        && current.2 >= previous.2
                        && current.3 >= previous.3
                );
                assert!(current.3 <= current.2 && current.2 <= current.0);
                previous = current;
                if let Some(resource) = event.resource_context() {
                    assert_eq!(resource.slot().sequence(), event.slot().unwrap().sequence());
                }
            }
            reports.push(serde_json::json!({"input":name,"encrypted":encrypted,"downloadedBytes":p.downloaded_bytes().to_string(),"decryptedBytes":p.decrypted_bytes().to_string(),"clearBytes":p.media().clear_bytes().to_string(),"mapBytes":p.maps().downloaded_bytes().to_string(),"mapReuses":p.maps().cache_reuses(),"committed":p.committed_segments(),"events":events.len()}));
        }
    }
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let (_, inputs) = fixtures::pair(
        "fmp4_hevc_regular",
        Some("ts_aac_audio_only"),
        [true, true],
        true,
    );
    let prepared = prepare_hls_with_keys(
        inputs,
        fixtures::keys(provider),
        fixtures::options().with_on_event(Arc::new(move |e| sink.lock().unwrap().push(e))),
    )
    .await
    .unwrap();
    let (_, report) = prepared.into_mp4_bytes().await.unwrap();
    assert!(query_keyed_capability(report.capability_query()).supported());
    assert!(report.capability_query().audio().is_some());
    for (i, progress) in report.inputs().iter().enumerate() {
        assert_eq!(
            progress.input_id().as_str(),
            if i == 0 { "primary" } else { "audio" }
        );
        assert_eq!(progress.downloaded_segments(), 3);
        assert_eq!(progress.decrypted_segments(), 2);
        assert_eq!(progress.committed_segments(), 3);
        assert_eq!(progress.maps().downloaded_resources(), usize::from(i == 0));
    }
    for event in events.lock().unwrap().iter() {
        if let Some(resource) = event.resource_context() {
            let p = event
                .inputs()
                .iter()
                .find(|p| p.input_id() == resource.slot().input_id())
                .unwrap();
            let counters = if resource.kind() == KeyResourceKind::Map {
                p.maps()
            } else {
                p.media()
            };
            assert!(counters.downloaded_resources() > 0);
        }
    }
    reports.push(serde_json::json!({"dualInput":true,"inputs":report.inputs().iter().map(|p|serde_json::json!({"downloadedBytes":p.downloaded_bytes().to_string(),"decryptedBytes":p.decrypted_bytes().to_string(),"committed":p.committed_segments(),"maps":p.maps().downloaded_resources()})).collect::<Vec<_>>() }));
    serde_json::json!({"portableQueries":queries,"reports":reports})
}
