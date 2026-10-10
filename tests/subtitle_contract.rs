#[path = "support/subtitle_contract.rs"]
mod suite;
#[tokio::test]
async fn shared_native_subtitle_contract() {
    suite::run().await;
}

#[cfg(feature = "serde")]
#[tokio::test]
async fn subtitle_resource_versions_and_serialized_cues_are_revalidated_before_truncation() {
    use hls_engine::{crypto::resource::*, *};
    use std::{
        io::Write,
        sync::{Arc, Mutex, atomic::Ordering},
    };
    async fn prepare(
        checkpoint: Option<EngineCheckpoint>,
        version: usize,
        serialized: bool,
    ) -> EngineSession {
        let provider = Arc::new(suite::Provider::default());
        provider
            .version
            .store(if version == 3 { 1 } else { version }, Ordering::SeqCst);
        let keys = suite::keys(provider);
        let s = match checkpoint {
            Some(cp) => {
                EngineSession::restore(suite::inputs(true), keys, EngineOptions::default(), cp)
                    .unwrap()
            }
            None => {
                EngineSession::new(suite::inputs(true), keys, EngineOptions::default()).unwrap()
            }
        };
        let h = s.handle();
        let p = suite::snapshot(
            "cc",
            "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:42\n#EXT-X-KEY:METHOD=AES-128,URI=\"key\"\n#EXTINF:1,\nseq\n",
        );
        let source =
            Arc::new(MemorySource::new().segment("https://subtitle.test/seq", suite::SEQUENCE));
        let request = ResourceRequest::webvtt_media(&p, 0).unwrap();
        let clear = if version == 3 {
            // Identical bytes/version from another operation are not fresh replay.
            ResourceSession::new(
                suite::keys(Arc::new(suite::Provider::default())),
                ResourceOptions::default(),
            )
            .unwrap()
            .read(source, request)
            .await
            .unwrap()
        } else {
            h.read_subtitle_resource(source, request).await.unwrap()
        };
        let cue = SubtitleCue::new(7, 0, suite::time(0), suite::time(8000), "bound")
            .with_resource(&clear)
            .unwrap();
        let cue = if serialized {
            serde_json::from_slice(&serde_json::to_vec(&cue).unwrap()).unwrap()
        } else {
            cue
        };
        let track = h.subtitle_track_id(&suite::id("cc")).unwrap();
        h.accept_cues(track, &[cue]).unwrap();
        suite::feed(&s);
        s
    }
    let directory =
        std::env::temp_dir().join(format!("hls-subtitle-recovery-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("media.mp4");
    let partial = directory.join("media.mp4.hls-partial");
    let latest = Arc::new(Mutex::new(None));
    let capture = latest.clone();
    let result = prepare(None, 1, false)
        .await
        .write_recoverable_to_file(
            &path,
            RecoveryOptions::new(Arc::new(move |cp| {
                if cp.bytes_written() > 0 {
                    *capture.lock().unwrap() = Some(cp);
                    return Err(EngineError::output(
                        std::io::Error::other("interrupted").into(),
                    ));
                }
                Ok(())
            })),
        )
        .await;
    assert!(result.is_err());
    let cp = latest.lock().unwrap().clone().unwrap();
    let wire = cp.to_bytes();
    assert!(!wire.windows(16).any(|v| v == (0u8..16).collect::<Vec<_>>()));
    std::fs::OpenOptions::new()
        .append(true)
        .open(&partial)
        .unwrap()
        .write_all(b"uncommitted tail")
        .unwrap();
    let unchanged = std::fs::read(&partial).unwrap();
    for (version, serialized) in [(2, false), (1, true), (3, false)] {
        let error = prepare(Some(cp.clone()), version, serialized)
            .await
            .write_recoverable_to_file(&path, RecoveryOptions::new(Arc::new(|_| Ok(()))))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), EngineErrorKind::ReplayRequired);
        assert_eq!(std::fs::read(&partial).unwrap(), unchanged);
        assert!(!path.exists());
    }
    prepare(Some(cp), 1, false)
        .await
        .write_recoverable_to_file(&path, RecoveryOptions::new(Arc::new(|_| Ok(()))))
        .await
        .unwrap();
    assert!(path.exists());
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn ordinary_subtitle_sink_cannot_claim_file_recovery() {
    use hls_engine::*;
    use std::sync::Arc;
    let sink = Arc::new(suite::Collector::default());
    let s = suite::session(true, EngineOptions::default()).with_subtitle_sink(sink);
    suite::feed(&s);
    let path = std::env::temp_dir().join(format!(
        "hls-sidecar-unsupported-{}.mp4",
        std::process::id()
    ));
    let error = s
        .write_recoverable_to_file(
            &path,
            RecoveryOptions::new(Arc::new(|_| panic!("must fail before checkpoint"))),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind(), EngineErrorKind::InvalidOptions);
    assert!(!path.exists());
}
