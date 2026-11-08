//! Synthetic PES clocks, independent of the remote bad-case media.
use hls_engine::{crypto::key::*, playlist::*, *};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
struct Clock;
impl KeyClock for Clock {
    fn now(&self) -> u64 {
        0
    }
}
struct NoKeys;
impl KeyProvider for NoKeys {
    fn resolve(&self, _: KeyRequest) -> KeyFuture<KeyResolution> {
        Box::pin(async { KeyResolution::Unavailable })
    }
}
fn keys() -> KeySession {
    KeySession::new(
        "aac-test",
        "auth",
        Arc::new(NoKeys),
        Arc::new(Clock),
        KeySessionOptions::default(),
    )
    .unwrap()
}
#[path = "support/aac_fixture.rs"]
mod fixture;
use fixture::segment;
fn options(ms: u64) -> EngineOptions {
    EngineOptions::default().with_aac_timestamp_policy(AacTimestampPolicy::RepairOverlaps {
        max_shift: Duration::from_millis(ms),
    })
}
fn session(opts: EngineOptions, cp: Option<EngineCheckpoint>) -> EngineSession {
    let id = InputId::new("audio").unwrap();
    let mut times: Vec<_> = (0..15).map(|i| i * 1920).collect();
    times.extend((0..20).map(|i| 24840 + i * 1920));
    let source = MemorySource::new()
        .segment("https://test/a.ts", segment(&times))
        .segment(
            "https://test/b.ts",
            segment(&(0..10).map(|i| 70000 + i * 1920).collect::<Vec<_>>()),
        );
    let inputs = EngineInputs::new(
        EngineInput::new(id.clone(), Arc::new(source)),
        EmbeddedAudio::Keep,
    );
    let s = if let Some(cp) = cp {
        EngineSession::restore(inputs, keys(), opts, cp).unwrap()
    } else {
        EngineSession::new(inputs, keys(), opts).unwrap()
    };
    let snap=parse_playlist_snapshot(&TextResource{location:SourceLocation::Url(url::Url::parse("https://test/list").unwrap()),content:"#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\na.ts\n#EXTINF:1,\nb.ts\n#EXT-X-ENDLIST\n".into()},PlaylistContext::new(id.clone(),0)).unwrap();
    s.handle().accept_snapshot(&id, &snap).unwrap();
    s.handle().end_input(&id).unwrap();
    s
}
#[tokio::test]
async fn strict_limit_and_report() {
    let strict = session(EngineOptions::default(), None)
        .into_bytes(1024 * 1024, OutputFormat::FragmentedMp4)
        .await
        .unwrap_err();
    assert_eq!(strict.kind(), EngineErrorKind::AacTimestampOverlap);
    let diagnostic: &AacTimestampOverlapError = strict.aac_overlap_error().unwrap();
    assert_eq!(diagnostic.slot.input_id(), &InputId::new("audio").unwrap());
    assert_eq!(diagnostic.slot.sequence(), 0);
    assert_time(diagnostic.original, 24840, 90000);
    assert_time(diagnostic.expected, 28800, 90000);
    assert_time(diagnostic.overlap, 44, 1000);
    assert!(strict.aac_timestamp_error().is_none());
    assert!(!format!("{strict:?} {strict}").contains("https://"));
    let err = session(options(40), None)
        .into_bytes(1024 * 1024, OutputFormat::FragmentedMp4)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), EngineErrorKind::AacTimestampRepairLimitExceeded);
    assert!(err.aac_overlap_error().is_none());
    let d = err.aac_timestamp_error().unwrap();
    assert_eq!(d.position.original.ticks(), 24840);
    let events = Arc::new(Mutex::new(Vec::new()));
    let capture = events.clone();
    let (bytes, report) = session(
        options(50).with_on_event(Arc::new(move |e| {
            if let EngineEvent::AacTimestampRepaired {
                slot,
                corrected_frames,
                ..
            } = e
            {
                capture
                    .lock()
                    .unwrap()
                    .push((slot.sequence(), corrected_frames));
            }
        })),
        None,
    )
    .into_bytes(1024 * 1024, OutputFormat::FragmentedMp4)
    .await
    .unwrap();
    assert!(!bytes.is_empty());
    assert_eq!(report.tracks()[0].sample_count(), 45);
    let summary = &report.media().aac_repairs().unwrap()[0];
    assert_eq!(summary.corrected_frames, 20);
    assert_eq!(summary.max_shift, MediaTime::new(11, 250).unwrap());
    assert_eq!(*events.lock().unwrap(), [(0, 20)]);
    #[cfg(feature = "serde")]
    assert_eq!(
        serde_json::to_value(&report).unwrap()["media"]["aac_repairs"][0]["corrected_frames"],
        "20"
    );
}
#[test]
fn option_validation() {
    for d in [Duration::ZERO, Duration::MAX] {
        let inputs = EngineInputs::new(
            EngineInput::new(InputId::new("a").unwrap(), Arc::new(MemorySource::new())),
            EmbeddedAudio::Keep,
        );
        assert_eq!(
            EngineSession::new(
                inputs,
                keys(),
                EngineOptions::default()
                    .with_aac_timestamp_policy(AacTimestampPolicy::RepairOverlaps { max_shift: d })
            )
            .err()
            .unwrap()
            .kind(),
            EngineErrorKind::InvalidOptions
        );
    }
}
#[tokio::test]
async fn repair_checkpoint_matches_continuous_output() {
    for format in [OutputFormat::FragmentedMp4, OutputFormat::StreamingMp4] {
        let dir =
            std::env::temp_dir().join(format!("aac-recovery-{}-{format:?}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("resumed.mp4");
        let reference = dir.join("reference.mp4");
        let saved = Arc::new(Mutex::new(None));
        let capture = saved.clone();
        let recovery = RecoveryOptions::new(Arc::new(move |cp| {
            if cp.bytes_written() > 0 && !cp.is_completed() {
                *capture.lock().unwrap() = Some(cp);
                return Err(EngineError::output(Error::invalid(
                    "checkpoint interruption",
                )));
            }
            Ok(())
        }))
        .with_output_format(format);
        session(options(50), None)
            .write_recoverable_to_file(&out, recovery)
            .await
            .unwrap_err();
        let cp = saved.lock().unwrap().clone().unwrap();
        assert_eq!(cp.schema_version(), 4);
        let cp = EngineCheckpoint::from_bytes(&cp.to_bytes()).unwrap();
        #[cfg(feature = "serde")]
        assert_eq!(
            serde_json::from_value::<EngineCheckpoint>(serde_json::to_value(&cp).unwrap())
                .unwrap()
                .to_bytes(),
            cp.to_bytes()
        );
        let resumed = session(options(50), Some(cp))
            .write_recoverable_to_file(
                &out,
                RecoveryOptions::new(Arc::new(|_| Ok(()))).with_output_format(format),
            )
            .await
            .unwrap();
        let uninterrupted = session(options(50), None)
            .write_recoverable_to_file(
                &reference,
                RecoveryOptions::new(Arc::new(|_| Ok(()))).with_output_format(format),
            )
            .await
            .unwrap();
        assert_eq!(
            resumed.media().aac_repairs().unwrap()[0].corrected_frames,
            uninterrupted.media().aac_repairs().unwrap()[0].corrected_frames
        );
        let mut a = std::fs::read(&out).unwrap();
        let mut b = std::fs::read(&reference).unwrap();
        common::normalize_moov_timestamps(&mut a);
        common::normalize_moov_timestamps(&mut b);
        assert_eq!(a, b);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
mod common;

fn custom(times: &[&[u64]], boundary: &str, opts: EngineOptions) -> EngineSession {
    let mut source = MemorySource::new();
    let mut text = "#EXTM3U\n#EXT-X-TARGETDURATION:1\n".to_owned();
    for (i, times) in times.iter().enumerate() {
        if i > 0 {
            text.push_str(boundary);
        }
        text.push_str(&format!("#EXTINF:1,\n{i}.ts\n"));
        source = source.segment(format!("https://test/{i}.ts"), segment(times));
    }
    text.push_str("#EXT-X-ENDLIST\n");
    from_source(source, text, opts, None)
}

fn from_source(
    source: MemorySource,
    text: String,
    opts: EngineOptions,
    checkpoint: Option<EngineCheckpoint>,
) -> EngineSession {
    let id = InputId::new("audio").unwrap();
    let inputs = EngineInputs::new(
        EngineInput::new(id.clone(), Arc::new(source)),
        EmbeddedAudio::Keep,
    );
    let s = match checkpoint {
        Some(cp) => EngineSession::restore(inputs, keys(), opts, cp),
        None => EngineSession::new(inputs, keys(), opts),
    }
    .unwrap();
    let snapshot = parse_playlist_snapshot(
        &TextResource {
            location: SourceLocation::Url(url::Url::parse("https://test/list").unwrap()),
            content: text,
        },
        PlaylistContext::new(id.clone(), 0),
    )
    .unwrap();
    s.handle().accept_snapshot(&id, &snapshot).unwrap();
    s
}

fn assert_time(actual: MediaTime, ticks: i128, timescale: u32) {
    assert_eq!(
        actual.ticks() * i128::from(timescale),
        ticks * i128::from(actual.timescale())
    );
}

#[tokio::test]
async fn strict_overlap_diagnostics_cover_equal_increasing_and_cross_segment_clocks() {
    let wrap = 1u64 << 33;
    for (segments, sequence, original, expected) in [
        (vec![vec![0, 1920, 1920]], 0, 1920, 3840),
        (vec![vec![0, 1920, 3000, 4920]], 0, 3000, 3840),
        (vec![vec![0, 1920, 3840], vec![3800, 5720]], 1, 3800, 5760),
        // A positive source origin must not leak output-relative times into diagnostics.
        (vec![vec![90000, 91920, 93000]], 0, 93000, 93840),
        (
            vec![vec![wrap - 3840, wrap - 1920], vec![0, 0]],
            1,
            wrap,
            wrap + 1920,
        ),
    ] {
        let slices: Vec<_> = segments.iter().map(Vec::as_slice).collect();
        let error = custom(&slices, "", EngineOptions::default())
            .into_bytes(1 << 20, OutputFormat::FragmentedMp4)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), EngineErrorKind::AacTimestampOverlap);
        let d = error.aac_overlap_error().unwrap();
        assert_eq!(d.slot.sequence(), sequence);
        assert_time(d.original, i128::from(original), 90000);
        assert_time(d.expected, i128::from(expected), 90000);
        assert_time(d.overlap, i128::from(expected - original), 90000);
        #[cfg(feature = "serde")]
        assert!(serde_json::to_string(d).unwrap().contains("overlap"));
    }
}

#[tokio::test]
async fn strict_diagnostics_preserve_rounding_gaps_and_epoch_behavior() {
    for (segments, boundary) in [
        (vec![vec![0, 1919, 3839]], ""), // Existing one-output-tick tolerance.
        (vec![vec![0, 1920], vec![9000, 10920]], ""), // Forward segment gap.
        (vec![vec![0, 1920], vec![0, 1920]], "#EXT-X-DISCONTINUITY\n"),
    ] {
        let slices: Vec<_> = segments.iter().map(Vec::as_slice).collect();
        let (_, report) = custom(&slices, boundary, EngineOptions::default())
            .into_bytes(1 << 20, OutputFormat::FragmentedMp4)
            .await
            .unwrap();
        assert!(report.media().aac_repairs().is_none());
    }
    // An unsupported forward gap inside a fragment is not an AAC overlap.
    let error = custom(&[&[0, 1920, 9000]], "", EngineOptions::default())
        .into_bytes(1 << 20, OutputFormat::FragmentedMp4)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), EngineErrorKind::TimelineAmbiguous);
    assert!(error.aac_overlap_error().is_none());
}

#[tokio::test]
async fn strict_44100_clock_diagnostic_is_exact_without_rejecting_rounding() {
    let mut times: Vec<_> = (0..100).map(|n| n * 1024 * 90000 / 44100).collect();
    let playlist = "#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXTINF:3,\na.ts\n#EXT-X-ENDLIST\n";
    for duplicate in [false, true] {
        if duplicate {
            times.push(*times.last().unwrap());
        }
        let result = from_source(
            MemorySource::new().segment("https://test/a.ts", fixture::segment_with_rate(&times, 4)),
            playlist.into(),
            EngineOptions::default(),
            None,
        )
        .into_bytes(1 << 20, OutputFormat::FragmentedMp4)
        .await;
        if duplicate {
            let err = result.unwrap_err();
            assert_eq!(err.kind(), EngineErrorKind::AacTimestampOverlap);
            assert_time(err.aac_overlap_error().unwrap().overlap, 1024, 44100);
        } else {
            result.unwrap();
        }
    }
}

#[tokio::test]
async fn unrelated_containers_and_video_do_not_report_ts_aac_overlap() {
    fn packed(clock: u64) -> Vec<u8> {
        let mut private = b"com.apple.streaming.transportStreamTimestamp\0".to_vec();
        private.extend_from_slice(&clock.to_be_bytes());
        let mut bytes = b"ID3\x04\0\0\0\0\0\0".to_vec();
        bytes[9] = (private.len() + 10) as u8;
        bytes.extend_from_slice(b"PRIV\0\0\0");
        bytes.push(private.len() as u8);
        bytes.extend_from_slice(&[0, 0]);
        bytes.extend_from_slice(&private);
        bytes.extend_from_slice(&[0xff, 0xf1, 0x4c, 0x80, 0x01, 0x3f, 0xfc, 1, 2]);
        bytes
    }
    for (init, a, b) in [
        (None, packed(0), packed(0)),
        (None, packed(0), packed(1000)), // Increasing, but overlapping.
        (
            Some(include_bytes!("fixtures/media/fmp4_aac_audio_only/init.fmp4").to_vec()),
            include_bytes!("fixtures/media/fmp4_aac_audio_only/seg0.m4s").to_vec(),
            include_bytes!("fixtures/media/fmp4_aac_audio_only/seg0.m4s").to_vec(),
        ),
        (
            None,
            include_bytes!("fixtures/media/ts_avc_video_only/seg0.ts").to_vec(),
            include_bytes!("fixtures/media/ts_avc_video_only/seg0.ts").to_vec(),
        ),
    ] {
        let mut source = MemorySource::new()
            .segment("https://test/a", a)
            .segment("https://test/b", b);
        let map = if let Some(init) = init {
            source = source.segment("https://test/init", init);
            "#EXT-X-MAP:URI=\"init\"\n"
        } else {
            ""
        };
        let error = from_source(source,
            format!("#EXTM3U\n#EXT-X-TARGETDURATION:3\n{map}#EXTINF:3,\na\n#EXTINF:3,\nb\n#EXT-X-ENDLIST\n"),
            EngineOptions::default(), None,
        ).into_bytes(1 << 20, OutputFormat::FragmentedMp4).await.unwrap_err();
        assert_eq!(error.kind(), EngineErrorKind::TimelineAmbiguous);
        assert!(error.aac_overlap_error().is_none());
    }
}

#[tokio::test]
async fn strict_checkpoint_restore_preserves_overlap_diagnostic() {
    let dir = std::env::temp_dir().join(format!("aac-strict-diagnostic-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for format in [OutputFormat::FragmentedMp4, OutputFormat::Mp4] {
        for last in [1920, 3000] {
            let make = |cp| {
                from_source(
                MemorySource::new().segment("https://test/a", segment(&[0, 1920]))
                    .segment("https://test/b", segment(&[last, last + 1920])),
                "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\na\n#EXTINF:1,\nb\n#EXT-X-ENDLIST\n".into(),
                EngineOptions::default(), cp,
            )
            };
            let direct = make(None).into_bytes(1 << 20, format).await.unwrap_err();
            let saved = Arc::new(Mutex::new(None));
            let capture = saved.clone();
            let path = dir.join(format!("{format:?}-{last}.mp4"));
            let stopped = make(None)
                .write_recoverable_to_file(
                    &path,
                    RecoveryOptions::new(Arc::new(move |cp| {
                        if cp.bytes_written() > 0 {
                            *capture.lock().unwrap() = Some(cp);
                            return Err(EngineError::output(Error::invalid(
                                "stop before second segment",
                            )));
                        }
                        Ok(())
                    }))
                    .with_output_format(format),
                )
                .await
                .unwrap_err();
            assert_eq!(stopped.kind(), EngineErrorKind::Output);
            let cp = saved.lock().unwrap().clone().unwrap();
            assert_eq!(cp.schema_version(), 2);
            let cp = EngineCheckpoint::from_bytes(&cp.to_bytes()).unwrap();
            let resumed = make(Some(cp))
                .write_recoverable_to_file(
                    &path,
                    RecoveryOptions::new(Arc::new(|_| Ok(()))).with_output_format(format),
                )
                .await
                .unwrap_err();
            assert_eq!(resumed.kind(), EngineErrorKind::AacTimestampOverlap);
            let d = resumed.aac_overlap_error().unwrap();
            assert_eq!(d.slot.sequence(), 1);
            assert_time(d.expected, 3840, 90000);
            assert_time(d.original, i128::from(last), 90000);
            assert_eq!(d.overlap, direct.aac_overlap_error().unwrap().overlap);
            assert!(!path.exists());
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}

fn mdat_payload(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let size = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        assert!(size >= 8);
        if &bytes[at + 4..at + 8] == b"mdat" {
            out.extend_from_slice(&bytes[at + 8..at + size]);
        }
        at += size;
    }
    out
}

#[tokio::test]
async fn generation_restart_reanchors_without_erasing_statistics() {
    let id = InputId::new("audio").unwrap();
    let source = MemorySource::new().segment("https://test/a.ts", segment(&[0, 1920, 1000]));
    let s = EngineSession::new(
        EngineInputs::new(
            EngineInput::new(id.clone(), Arc::new(source)),
            EmbeddedAudio::Keep,
        ),
        keys(),
        options(50).with_mode(EngineMode::Open),
    )
    .unwrap();
    let h = s.handle();
    let snapshot = |generation, end: bool| {
        parse_playlist_snapshot(
            &TextResource {
                location: SourceLocation::Url(url::Url::parse("https://test/list").unwrap()),
                content: format!(
                    "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\na.ts\n{}",
                    if end { "#EXT-X-ENDLIST\n" } else { "" }
                ),
            },
            PlaylistContext::new(id.clone(), generation),
        )
        .unwrap()
    };
    h.accept_snapshot(&id, &snapshot(0, false)).unwrap();
    let mut bytes = Vec::new();
    let mut run = Box::pin(s.write_to(&mut bytes));
    assert!(futures_util::poll!(&mut run).is_pending());
    assert_eq!(h.progress()[0].committed(), 1);
    h.restart(&id, 1).unwrap();
    h.accept_snapshot(&id, &snapshot(1, true)).unwrap();
    let report = run.await.unwrap();
    let summary = &report.media().aac_repairs().unwrap()[0];
    assert_eq!(summary.corrected_frames, 2);
    assert_eq!(summary.first.as_ref().unwrap().slot.generation(), 0);
    assert_eq!(summary.last.as_ref().unwrap().slot.generation(), 1);
    assert_eq!(mdat_payload(&bytes), [0, 1, 2, 0, 1, 2]);
}

#[tokio::test]
async fn segment_overlap_wrap_and_boundary_resets_preserve_payload() {
    let wrap = 1u64 << 33;
    for (a, b, boundary, count) in [
        (vec![0, 1920, 3840], vec![3800, 5720], "", 2),
        (vec![wrap - 3840, wrap - 1920], vec![0, 1920], "", 0),
        (
            vec![0, 1920, 3840],
            vec![0, 1920],
            "#EXT-X-DISCONTINUITY\n",
            0,
        ),
        (
            vec![0, 1920],
            vec![93840, 95760],
            "#EXT-X-GAP\n#EXTINF:1,\nmissing.ts\n",
            0,
        ),
    ] {
        let s = custom(
            &[&a, &b],
            boundary,
            options(50).with_missing_segments(MissingSegmentPolicy::Skip),
        );
        let (bytes, report) = s
            .into_bytes(1 << 20, OutputFormat::FragmentedMp4)
            .await
            .unwrap();
        assert_eq!(
            report.media().aac_repairs().unwrap()[0].corrected_frames,
            count
        );
        let expected: Vec<_> = (0..a.len()).chain(0..b.len()).map(|i| i as u8).collect();
        assert_eq!(mdat_payload(&bytes), expected, "payload and ordering");
    }
}

#[tokio::test]
async fn policies_cannot_change_on_restore_and_failure_never_publishes() {
    let dir = std::env::temp_dir().join(format!("aac-fail-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("fail.mp4");
    assert_eq!(
        session(options(40), None)
            .write_to_file(&out, FileOutputOptions::default())
            .await
            .unwrap_err()
            .kind(),
        EngineErrorKind::AacTimestampRepairLimitExceeded
    );
    assert!(!out.exists());
    let saved = Arc::new(Mutex::new(None));
    let capture = saved.clone();
    session(options(50), None)
        .write_recoverable_to_file(
            dir.join("repair.mp4"),
            RecoveryOptions::new(Arc::new(move |cp| {
                *capture.lock().unwrap() = Some(cp);
                Err(EngineError::output(Error::invalid("stop")))
            })),
        )
        .await
        .unwrap_err();
    let cp = saved.lock().unwrap().clone().unwrap();
    for policy in [EngineOptions::default(), options(49), options(51)] {
        let inputs = EngineInputs::new(
            EngineInput::new(
                InputId::new("audio").unwrap(),
                Arc::new(MemorySource::new()),
            ),
            EmbeddedAudio::Keep,
        );
        assert_eq!(
            EngineSession::restore(inputs, keys(), policy, cp.clone())
                .err()
                .unwrap()
                .kind(),
            EngineErrorKind::ResumeConflict
        );
    }
    for data in [
        include_bytes!("fixtures/checkpoint/engine-schema2.bin").as_slice(),
        include_bytes!("fixtures/checkpoint/engine-schema3.bin").as_slice(),
    ] {
        let inputs = EngineInputs::new(
            EngineInput::new(
                InputId::new("audio").unwrap(),
                Arc::new(MemorySource::new()),
            ),
            EmbeddedAudio::Keep,
        );
        let cp = EngineCheckpoint::from_bytes(data).unwrap();
        assert_eq!(
            EngineSession::restore(inputs, keys(), options(50), cp)
                .err()
                .unwrap()
                .kind(),
            EngineErrorKind::ResumeConflict
        );
    }
    assert!(EngineCheckpoint::from_bytes(b"HLSECP01").is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn encrypted_multitrack_and_range_share_the_same_repair() {
    use aes::cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
    struct Provider;
    impl KeyProvider for Provider {
        fn resolve(&self, _: KeyRequest) -> KeyFuture<KeyResolution> {
            Box::pin(async {
                KeyResolution::Available(AvailableKey::aes128(SecretKey::new(vec![7; 16]).unwrap()))
            })
        }
    }
    struct Wait;
    impl EngineWait for Wait {
        fn wait(
            &self,
            d: Duration,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
            Box::pin(tokio::time::sleep(d))
        }
    }
    let mut outputs = Vec::new();
    for encrypted in [false, true] {
        let mut source = MemorySource::new();
        for (name, times) in [
            ("a", vec![0, 1920, 1000, 2920, 4840]),
            ("b", vec![0, 1920, 3840, 5760, 7680]),
        ] {
            let mut bytes = segment(&times);
            if encrypted {
                let n = bytes.len();
                bytes.resize(n + 16, 0);
                bytes = cbc::Encryptor::<aes::Aes128>::new(&[7; 16].into(), &[0; 16].into())
                    .encrypt_padded_mut::<Pkcs7>(&mut bytes, n)
                    .unwrap()
                    .to_vec();
            }
            source = source.segment(format!("https://test/{name}.ts"), bytes);
        }
        let source = Arc::new(source);
        let inputs = EngineInputs::new(
            EngineInput::new(InputId::new("a").unwrap(), source.clone()),
            EmbeddedAudio::Keep,
        )
        .with_audio(
            EngineInput::new(InputId::new("b").unwrap(), source),
            TrackMetadata::default(),
        );
        let keys = KeySession::new(
            "aac-multi",
            "scope",
            Arc::new(Provider),
            Arc::new(Clock),
            KeySessionOptions::default(),
        )
        .unwrap();
        let s = EngineSession::new(
            inputs,
            keys,
            options(50)
                .with_waiter(Arc::new(Wait), Duration::from_secs(1))
                .with_range(
                    PresentationRange::new(
                        MediaTime::new(1, 50).unwrap(),
                        MediaTime::new(9, 100).unwrap(),
                    )
                    .unwrap(),
                ),
        )
        .unwrap();
        for name in ["a", "b"] {
            let id = InputId::new(name).unwrap();
            let key = if encrypted {
                "#EXT-X-KEY:METHOD=AES-128,URI=\"key\",IV=0x00000000000000000000000000000000\n"
            } else {
                ""
            };
            let snap=parse_playlist_snapshot(&TextResource {location:SourceLocation::Url(url::Url::parse("https://test/list").unwrap()),content:format!("#EXTM3U\n#EXT-X-TARGETDURATION:1\n{key}#EXTINF:1,\n{name}.ts\n#EXT-X-ENDLIST\n")},PlaylistContext::new(id.clone(),0)).unwrap();
            s.handle().accept_snapshot(&id, &snap).unwrap();
        }
        let (bytes, report) = s
            .into_bytes(1 << 20, OutputFormat::FragmentedMp4)
            .await
            .unwrap();
        let stats = report.media().aac_repairs().unwrap();
        assert_eq!(
            stats
                .iter()
                .map(|s| (s.input_id.as_str(), s.corrected_frames))
                .collect::<Vec<_>>(),
            [("a", 3), ("b", 0)]
        );
        outputs.push(bytes);
    }
    common::normalize_moov_timestamps(&mut outputs[0]);
    common::normalize_moov_timestamps(&mut outputs[1]);
    assert_eq!(outputs[0], outputs[1]);
}

#[tokio::test]
async fn excluded_embedded_audio_does_not_repair_or_shift_video() {
    let mut data = include_bytes!("fixtures/media/ts_avc_video_only/seg0.ts").to_vec();
    data.extend(segment(&[126000, 127920, 125000, 126920]));
    let source = Arc::new(MemorySource::new().segment("https://test/a.ts", data));
    let id = InputId::new("video").unwrap();
    let snap = parse_playlist_snapshot(
        &TextResource {
            location: SourceLocation::Url(url::Url::parse("https://test/list").unwrap()),
            content: "#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXTINF:2,\na.ts\n#EXT-X-ENDLIST\n".into(),
        },
        PlaylistContext::new(id.clone(), 0),
    )
    .unwrap();
    let mut outputs = Vec::new();
    for opts in [EngineOptions::default(), options(1)] {
        let s = EngineSession::new(
            EngineInputs::new(
                EngineInput::new(id.clone(), source.clone()),
                EmbeddedAudio::Exclude,
            ),
            keys(),
            opts,
        )
        .unwrap();
        s.handle().accept_snapshot(&id, &snap).unwrap();
        let (bytes, report) = s
            .into_bytes(1 << 20, OutputFormat::FragmentedMp4)
            .await
            .unwrap();
        assert!(report.media().aac_repairs().is_none_or(|s| s.is_empty()));
        assert_eq!(report.tracks().len(), 1);
        outputs.push(bytes);
    }
    common::normalize_moov_timestamps(&mut outputs[0]);
    common::normalize_moov_timestamps(&mut outputs[1]);
    assert_eq!(outputs[0], outputs[1]);
}

#[tokio::test]
async fn accepted_configuration_change_establishes_a_new_clock() {
    let source = MemorySource::new()
        .segment("https://test/a.ts", segment(&[0, 1920]))
        .segment(
            "https://test/b.ts",
            fixture::segment_with_rate(&[3000, 5089], 4),
        );
    let id = InputId::new("audio").unwrap();
    let s = EngineSession::new(
        EngineInputs::new(
            EngineInput::new(id.clone(), Arc::new(source)),
            EmbeddedAudio::Keep,
        ),
        keys(),
        options(1).with_change_policy(TimelineChangePolicy::Split),
    )
    .unwrap();
    let snapshot=parse_playlist_snapshot(&TextResource{location:SourceLocation::Url(url::Url::parse("https://test/list").unwrap()),content:"#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\na.ts\n#EXTINF:1,\nb.ts\n#EXT-X-ENDLIST\n".into()},PlaylistContext::new(id.clone(),0)).unwrap();
    s.handle().accept_snapshot(&id, &snapshot).unwrap();
    let dir = std::env::temp_dir().join(format!("aac-config-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let report = s
        .write_recoverable_to_file(
            dir.join("media.mp4"),
            RecoveryOptions::new(Arc::new(|_| Ok(())))
                .with_output_format(OutputFormat::FragmentedMp4),
        )
        .await
        .unwrap();
    assert_eq!(report.media().outputs().len(), 2);
    // Only the sub-90kHz rounding of the second 44.1kHz frame is corrected.
    // Carrying the previous 48kHz end (3840) into the new config would exceed 1ms.
    assert_eq!(report.media().aac_repairs().unwrap()[0].corrected_frames, 1);
    std::fs::remove_dir_all(dir).unwrap();
}
