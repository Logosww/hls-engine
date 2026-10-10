#[path = "support/subtitle_contract.rs"]
mod subtitles;
use hls_engine::legacy::{playlist::*, *};
use std::{future::Future, pin::Pin, sync::Arc};
#[allow(dead_code)]
#[path = "support/sample_crypto.rs"]
mod sample;
struct Wait;
impl ContinuousWait for Wait {
    fn wait(&self, duration: std::time::Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(tokio::time::sleep(duration))
    }
}
fn id(s: &str) -> InputId {
    InputId::new(s).unwrap()
}
fn time(ms: i128) -> MediaTime {
    MediaTime::new(ms, 1000).unwrap()
}
fn snapshot(input: &str, text: &str) -> PlaylistSnapshot {
    parse_playlist_snapshot(
        &TextResource {
            location: SourceLocation::Url(
                url::Url::parse("https://multi.test/input.m3u8").unwrap(),
            ),
            content: text.into(),
        },
        PlaylistContext::new(id(input), 1),
    )
    .unwrap()
}
fn source(case: &sample::Case) -> Arc<dyn Source> {
    let mut source = MemorySource::new();
    for (name, bytes) in case.files {
        source = source.segment(format!("https://multi.test/{name}"), *bytes);
    }
    Arc::new(source)
}
fn options() -> ContinuousOptions {
    ContinuousOptions::default().with_waiter(Arc::new(Wait), std::time::Duration::from_secs(1))
}
fn write_fixture(name: &str, bytes: &[u8]) {
    if let Ok(dir) = std::env::var("HLS_MULTITRACK_OUTPUT") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(std::path::Path::new(&dir).join(name), bytes).unwrap();
    }
}
#[tokio::test]
async fn encrypted_video_two_audio_and_overlapping_subtitles_classic_and_fragmented() {
    let cases = sample::cases();
    let video = cases.iter().find(|c| c.name == "fmp4_avc_cenc").unwrap();
    let audio = cases.iter().find(|c| c.name == "fmp4_aac_cbcs").unwrap();
    for (name, format) in [
        ("multi-fragmented.mp4", OutputFormat::FragmentedMp4),
        ("multi-classic.mp4", OutputFormat::Mp4),
    ] {
        let inputs = MultiTrackInputs::new(
            ContinuousInput::new(id("main"), source(video)),
            EmbeddedAudio::Exclude,
        )
        .with_audio(
            ContinuousInput::new(id("en"), source(audio)),
            TrackMetadata::new("en", "English").with_default(true),
        )
        .with_audio(
            ContinuousInput::new(id("ja"), source(audio)),
            TrackMetadata::new("ja", "Japanese"),
        )
        .with_subtitle(SubtitleTrack::new(
            id("captions"),
            id("main"),
            TrackMetadata::new("en", "Captions"),
        ));
        let s = MultiTrackSession::new(inputs, sample::keys(Arc::new(sample::Provider)), options())
            .unwrap();
        let h = s.handle();
        for (input, case) in [("main", video), ("en", audio), ("ja", audio)] {
            h.accept_snapshot(&id(input), &snapshot(input, case.playlist))
                .unwrap();
        }
        let track = h.subtitle_track_id(&id("captions")).unwrap();
        h.accept_cues(
            track,
            &[
                SubtitleCue::new(1, 0, time(0), time(500), "hello")
                    .with_identifier("a")
                    .with_settings("align:start position:10%"),
                SubtitleCue::new(1, 0, time(250), time(750), "overlap").with_identifier("b"),
            ],
        )
        .unwrap();
        h.end_subtitles(track).unwrap();
        let (bytes, report) = s.into_bytes(8 * 1024 * 1024, format).await.unwrap();
        assert_eq!(report.tracks().len(), 4);
        let audios: Vec<_> = report
            .tracks()
            .iter()
            .filter(|t| t.kind() == OutputTrackKind::Audio)
            .collect();
        assert_eq!(audios.len(), 2);
        assert_eq!(audios[0].sample_count(), audios[1].sample_count());
        assert_ne!(audios[0].id(), audios[1].id());
        assert!(bytes.windows(4).any(|v| v == b"wvtt"));
        assert!(bytes.windows(7).any(|v| v == b"overlap"));
        assert!(bytes.windows(7).any(|v| v == b"English"));
        assert!(bytes.windows(8).any(|v| v == b"Japanese"));
        assert!(!report.subtitle_reports().is_empty());
        write_fixture(name, &bytes);
    }
}
fn packed(anchor: u64, frames: usize) -> Vec<u8> {
    let mut private = b"com.apple.streaming.transportStreamTimestamp\0".to_vec();
    private.extend_from_slice(&anchor.to_be_bytes());
    let size = private.len() + 10;
    let mut bytes = vec![b'I', b'D', b'3', 4, 0, 0, 0, 0, 0, size as u8];
    bytes.extend_from_slice(b"PRIV\0\0\0");
    bytes.push(private.len() as u8);
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&private);
    for _ in 0..frames {
        bytes.extend_from_slice(&[0xff, 0xf1, 0x50, 0x80, 0x01, 0x3f, 0xfc, 1, 2]);
    }
    bytes
}
#[tokio::test]
async fn packed_44100_wrap_is_exact_and_not_extension_based() {
    let first = (1u64 << 33) - 1000;
    let next = (first + 100 * 1024 * 90000 / 44100) % (1u64 << 33);
    let src = MemorySource::new()
        .segment("https://multi.test/a.bin", packed(first, 100))
        .segment("https://multi.test/b.bin", packed(next, 100));
    let s = MultiTrackSession::new(
        MultiTrackInputs::new(
            ContinuousInput::new(id("main"), Arc::new(src)),
            EmbeddedAudio::Keep,
        ),
        sample::keys(Arc::new(sample::Provider)),
        ContinuousOptions::default(),
    )
    .unwrap();
    s.handle().accept_snapshot(&id("main"),&snapshot("main","#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXTINF:2.322,\na.bin\n#EXTINF:2.322,\nb.bin\n#EXT-X-ENDLIST")).unwrap();
    let (_, report) = s
        .into_bytes(2 * 1024 * 1024, OutputFormat::Mp4)
        .await
        .unwrap();
    assert_eq!(report.tracks()[0].sample_count(), 200);
    assert_eq!(report.tracks()[0].timescale(), 44100);
    assert_eq!(report.tracks()[0].duration(), 200 * 1024);
}
#[tokio::test]
async fn packed_missing_anchor_and_subtitle_styles_fail_closed() {
    let src = MemorySource::new().segment(
        "https://multi.test/a.bin",
        vec![0xff, 0xf1, 0x50, 0x80, 0x01, 0x3f, 0xfc, 1, 2],
    );
    let inputs = MultiTrackInputs::new(
        ContinuousInput::new(id("main"), Arc::new(src)),
        EmbeddedAudio::Keep,
    )
    .with_subtitle(SubtitleTrack::new(
        id("cc"),
        id("main"),
        TrackMetadata::default(),
    ));
    let s = MultiTrackSession::new(
        inputs,
        sample::keys(Arc::new(sample::Provider)),
        ContinuousOptions::default(),
    )
    .unwrap();
    let h = s.handle();
    let cc = h.subtitle_track_id(&id("cc")).unwrap();
    for invalid in [
        "region:custom",
        "position:80%,auto",
        "line:auto",
        "size:1e1%",
        "position:+10%",
        "size:.5%",
        "size:5.%",
        "line:+1",
        "align:start align:end",
        "line:1\nsize:50%",
    ] {
        assert_eq!(
            h.accept_cues(
                cc,
                &[SubtitleCue::new(1, 0, time(0), time(100), "text").with_settings(invalid)]
            )
            .unwrap_err()
            .kind(),
            ContinuousErrorKind::UnsupportedSubtitleProfile,
            "{invalid}"
        );
    }
    for valid in [
        "align:right position:80%",
        "line:-1,end size:50.5%",
        "vertical:rl\tposition:10%,center",
    ] {
        assert!(
            h.accept_cues(
                cc,
                &[SubtitleCue::new(1, 0, time(0), time(100), "text").with_settings(valid)]
            )
            .is_ok(),
            "{valid}"
        );
    }
    h.accept_snapshot(
        &id("main"),
        &snapshot(
            "main",
            "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\na.bin\n#EXT-X-ENDLIST",
        ),
    )
    .unwrap();
    let mut bytes = vec![];
    assert_eq!(
        s.write_to(&mut bytes).await.unwrap_err().kind(),
        ContinuousErrorKind::Resource
    );
    assert!(bytes.is_empty());
}

#[tokio::test]
async fn late_cues_are_reported_and_partial_cue_tail_drains_after_audio_eof() {
    use std::sync::Mutex;
    let cases = sample::cases();
    let audio = cases.iter().find(|c| c.name == "fmp4_aac_clear").unwrap();
    let holder = Arc::new(Mutex::new(None::<MultiTrackHandle>));
    let callback = holder.clone();
    let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = fired.clone();
    let opts = ContinuousOptions::default().with_on_event(Arc::new(move |event| {
        if matches!(event, ContinuousEvent::Committed { .. })
            && !flag.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            let h = callback.lock().unwrap();
            let h = h.as_ref().unwrap();
            let cc = h.subtitle_track_id(&id("cc")).unwrap();
            let receipt = h
                .accept_cues(
                    cc,
                    &[
                        SubtitleCue::new(1, 0, time(0), time(1), "late").with_identifier("late"),
                        SubtitleCue::new(1, 0, time(0), time(8000), "tail").with_identifier("tail"),
                    ],
                )
                .unwrap();
            assert_eq!(receipt.rejected_late(), 1);
            assert_eq!(receipt.clipped(), 1);
            assert_eq!(receipt.accepted(), 1);
        }
    }));
    let inputs = MultiTrackInputs::new(
        ContinuousInput::new(id("main"), source(audio)),
        EmbeddedAudio::Keep,
    )
    .with_subtitle(SubtitleTrack::new(
        id("cc"),
        id("main"),
        TrackMetadata::default(),
    ));
    let s = MultiTrackSession::new(inputs, sample::keys(Arc::new(sample::Provider)), opts).unwrap();
    let h = s.handle();
    *holder.lock().unwrap() = Some(h.clone());
    h.accept_snapshot(&id("main"), &snapshot("main", audio.playlist))
        .unwrap();
    let (bytes, report) = s
        .into_bytes(4 * 1024 * 1024, OutputFormat::Mp4)
        .await
        .unwrap();
    assert!(fired.load(std::sync::atomic::Ordering::SeqCst));
    assert!(
        report
            .subtitle_reports()
            .iter()
            .any(|r| r.disposition() == SubtitleDisposition::RejectedLate)
    );
    assert_eq!(
        report
            .tracks()
            .iter()
            .find(|t| t.kind() == OutputTrackKind::Subtitle)
            .unwrap()
            .duration(),
        720000
    );
    assert!(!bytes.windows(8).any(|v| v == b"payllate"));
    let cc = h.subtitle_track_id(&id("cc")).unwrap();
    assert_eq!(
        h.accept_cues(cc, &[]).unwrap_err().kind(),
        ContinuousErrorKind::Closed
    );
    write_fixture("late-tail-classic.mp4", &bytes);
}

#[tokio::test]
async fn subtitle_admission_is_atomic_and_unknown_epoch_fails_before_finalize() {
    let cases = sample::cases();
    let audio = cases.iter().find(|c| c.name == "fmp4_aac_clear").unwrap();
    let inputs = MultiTrackInputs::new(
        ContinuousInput::new(id("main"), source(audio)),
        EmbeddedAudio::Keep,
    )
    .with_subtitle(SubtitleTrack::new(
        id("cc"),
        id("main"),
        TrackMetadata::default(),
    ));
    let s = MultiTrackSession::new(
        inputs,
        sample::keys(Arc::new(sample::Provider)),
        ContinuousOptions::default(),
    )
    .unwrap();
    let h = s.handle();
    let cc = h.subtitle_track_id(&id("cc")).unwrap();
    assert_eq!(
        h.accept_cues(
            cc,
            &[
                SubtitleCue::new(1, 0, time(0), time(100), "valid"),
                SubtitleCue::new(1, 0, time(100), time(100), "invalid"),
            ]
        )
        .unwrap_err()
        .kind(),
        ContinuousErrorKind::InvalidSubtitle
    );
    h.accept_cues(
        cc,
        &[SubtitleCue::new(1, 99, time(0), time(100), "unknown")],
    )
    .unwrap();
    h.accept_snapshot(&id("main"), &snapshot("main", audio.playlist))
        .unwrap();
    assert_eq!(
        s.into_bytes(4 * 1024 * 1024, OutputFormat::Mp4)
            .await
            .unwrap_err()
            .kind(),
        ContinuousErrorKind::MissingSubtitleMapping
    );
}

#[tokio::test]
async fn native_classic_preserves_all_tracks_and_metadata() {
    let cases = sample::cases();
    let audio = cases.iter().find(|c| c.name == "fmp4_aac_clear").unwrap();
    let inputs = MultiTrackInputs::new(
        ContinuousInput::new(id("main"), source(audio)),
        EmbeddedAudio::Keep,
    )
    .with_primary_audio(TrackMetadata::new("en", "Primary").with_default(true))
    .with_audio(
        ContinuousInput::new(id("second"), source(audio)),
        TrackMetadata::new("ja", "Second"),
    )
    .with_subtitle(SubtitleTrack::new(
        id("cc"),
        id("main"),
        TrackMetadata::new("zh", "字幕"),
    ));
    let s = MultiTrackSession::new(inputs, sample::keys(Arc::new(sample::Provider)), options())
        .unwrap();
    let h = s.handle();
    for name in ["main", "second"] {
        h.accept_snapshot(&id(name), &snapshot(name, audio.playlist))
            .unwrap();
    }
    let cc = h.subtitle_track_id(&id("cc")).unwrap();
    h.accept_cues(cc, &[SubtitleCue::new(1, 0, time(0), time(1000), "你好")])
        .unwrap();
    let path =
        std::env::temp_dir().join(format!("hls-multitrack-native-{}.mp4", std::process::id()));
    let report = s
        .write_to_file(
            &path,
            FileOutputOptions::default().with_format(OutputFormat::StreamingMp4),
        )
        .await
        .unwrap();
    let bytes = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert_eq!(report.tracks().len(), 3);
    assert!(bytes.windows(6).any(|v| v == "你好".as_bytes()));
    assert!(bytes.windows(6).any(|v| v == b"Second"));
    assert!(
        report.media().outputs()[0].classic_index_samples()
            > report.media().outputs()[0]
                .media()
                .tracks
                .iter()
                .map(|t| t.sample_count as u64)
                .sum()
    );
    write_fixture("native-multitrack-classic.mp4", &bytes);
}

#[test]
fn capability_queries_keep_legacy_restrictions_and_reject_unverified_players() {
    use hls_engine::legacy::capabilities::*;
    let packed = KeyedInputCapability::new(
        KeyedContainer::PackedAac,
        KeyedEncryption::SampleAes,
        vec![KeyedCodec::AacLc],
    );
    assert!(
        !query_keyed_capability(&KeyedCapabilityQuery::new(
            packed.clone(),
            KeyedOutput::FragmentedWriter
        ))
        .supported()
    );
    let query = MultiTrackCapabilityQuery::new(
        id("main"),
        packed,
        KeyedOutput::FragmentedWriter,
        EmbeddedAudio::Keep,
    )
    .with_subtitles(true);
    assert!(query_multitrack_capability(&query).supported());
    assert!(
        !query_multitrack_capability(
            &query
                .clone()
                .with_playback(MultiTrackPlayback::DirectBrowser)
        )
        .supported()
    );
    for (target, reason) in [
        (
            MultiTrackPlayback::DirectBrowser,
            MultiTrackPlaybackRejection::BrowserTrackSelection,
        ),
        (
            MultiTrackPlayback::Vlc,
            MultiTrackPlaybackRejection::SubtitleSettings,
        ),
        (
            MultiTrackPlayback::Iina,
            MultiTrackPlaybackRejection::WvttDecoder,
        ),
        (
            MultiTrackPlayback::Ffmpeg,
            MultiTrackPlaybackRejection::WvttDecoder,
        ),
    ] {
        let decision = query_multitrack_capability(&query.clone().with_playback(target));
        assert!(!decision.supported());
        assert!(decision.container_supported());
        assert!(decision.container_rejections().is_empty());
        assert!(!decision.playback_supported());
        assert_eq!(decision.playback_rejection(), Some(reason));
    }
    assert!(
        query_multitrack_capability(
            &query
                .clone()
                .with_playback(MultiTrackPlayback::AvFoundation)
        )
        .supported()
    );
    let classic = MultiTrackCapabilityQuery::new(
        id("main"),
        KeyedInputCapability::new(
            KeyedContainer::PackedAac,
            KeyedEncryption::Clear,
            vec![KeyedCodec::AacLc],
        ),
        KeyedOutput::Mp4Bytes,
        EmbeddedAudio::Keep,
    )
    .with_memory_capacity(1024 * 1024);
    for target in [MultiTrackPlayback::Ffmpeg, MultiTrackPlayback::Iina] {
        let decision = query_multitrack_capability(&classic.clone().with_playback(target));
        assert!(!decision.supported());
        assert_eq!(
            decision.playback_rejection(),
            Some(MultiTrackPlaybackRejection::ClassicEditTimeline)
        );
    }
    assert!(query_multitrack_capability(&classic.with_decode_gaps(true)).supported());
    assert!(!query_multitrack_capability(&query.with_resume(true)).supported());
}

#[cfg(feature = "serde")]
#[test]
fn subtitle_wire_preserves_large_integers_and_rejects_numbers() {
    let cue = SubtitleCue::new(9_007_199_254_740_993, u64::MAX, time(0), time(100), "hello");
    let mut wire = serde_json::to_value(cue).unwrap();
    assert_eq!(wire["generation"], "9007199254740993");
    let _: SubtitleCue = serde_json::from_value(wire.clone()).unwrap();
    wire["generation"] = serde_json::json!(9_007_199_254_740_993u64);
    assert!(serde_json::from_value::<SubtitleCue>(wire).is_err());
}

#[tokio::test]
async fn fixed_tracks_keep_independent_tails_sequences_and_offsets() {
    let cases = sample::cases();
    let av = &sample::Case {
        name: "av",
        playlist: "#EXTM3U\n#EXT-X-TARGETDURATION:10\n#EXTINF:5,\nav.ts\n#EXT-X-ENDLIST\n",
        files: &[("av.ts", include_bytes!("fixtures/h264_aac_fhd.ts"))],
    };
    let audio = cases.iter().find(|c| c.name == "fmp4_aac_clear").unwrap();
    for embedded in [EmbeddedAudio::Keep, EmbeddedAudio::Exclude] {
        let mut inputs =
            MultiTrackInputs::new(ContinuousInput::new(id("main"), source(av)), embedded);
        for name in ["early", "late"] {
            inputs = inputs.with_audio(
                ContinuousInput::new(id(name), source(audio)),
                TrackMetadata::new("en", name),
            );
        }
        let s = MultiTrackSession::new(inputs, sample::keys(Arc::new(sample::Provider)), options())
            .unwrap();
        let h = s.handle();
        h.accept_snapshot(&id("main"), &snapshot("main", av.playlist))
            .unwrap();
        h.accept_snapshot(
            &id("late"),
            &snapshot(
                "late",
                &audio
                    .playlist
                    .replace("#EXT-X-MEDIA-SEQUENCE:0", "#EXT-X-MEDIA-SEQUENCE:900"),
            ),
        )
        .unwrap();
        // End early after one resource; the other two lanes retain all their tails.
        let mut short = String::new();
        for line in audio.playlist.lines() {
            short.push_str(line);
            short.push('\n');
            if !line.is_empty() && !line.starts_with('#') {
                break;
            }
        }
        short.push_str("#EXT-X-ENDLIST\n");
        h.accept_snapshot(&id("early"), &snapshot("early", &short))
            .unwrap();
        let (_, r) = s
            .into_bytes(8 * 1024 * 1024, OutputFormat::FragmentedMp4)
            .await
            .unwrap();
        let early = r
            .tracks()
            .iter()
            .find(|t| t.input_id() == &id("early"))
            .unwrap();
        let late = r
            .tracks()
            .iter()
            .find(|t| t.input_id() == &id("late"))
            .unwrap();
        assert!(late.sample_count() > early.sample_count());
        let main_audio = r
            .tracks()
            .iter()
            .filter(|t| t.input_id() == &id("main") && t.kind() == OutputTrackKind::Audio)
            .count();
        assert_eq!(main_audio, usize::from(embedded == EmbeddedAudio::Keep));
    }
}

#[tokio::test]
async fn slow_sink_blocks_every_input_and_cue_admission_cancel_beats_stop() {
    struct Block;
    impl tokio::io::AsyncWrite for Block {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            _: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::task::Poll::Pending
        }
        fn poll_flush(
            self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Pending
        }
        fn poll_shutdown(
            self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            panic!("caller owns shutdown")
        }
    }
    let cases = sample::cases();
    let audio = cases.iter().find(|c| c.name == "fmp4_aac_clear").unwrap();
    let inputs = MultiTrackInputs::new(
        ContinuousInput::new(id("main"), source(audio)),
        EmbeddedAudio::Keep,
    )
    .with_audio(
        ContinuousInput::new(id("other"), source(audio)),
        TrackMetadata::default(),
    )
    .with_subtitle(SubtitleTrack::new(
        id("cc"),
        id("main"),
        TrackMetadata::default(),
    ));
    let s = MultiTrackSession::new(inputs, sample::keys(Arc::new(sample::Provider)), options())
        .unwrap();
    let h = s.handle();
    let text = audio.playlist.replace("#EXT-X-ENDLIST", "");
    for name in ["main", "other"] {
        h.accept_snapshot(&id(name), &snapshot(name, &text))
            .unwrap();
    }
    let mut writer = Block;
    let mut run = Box::pin(s.write_to(&mut writer));
    assert!(futures_util::poll!(&mut run).is_pending());
    for name in ["main", "other"] {
        assert_eq!(
            h.accept_snapshot(&id(name), &snapshot(name, &text))
                .unwrap_err()
                .kind(),
            ContinuousErrorKind::WouldBlock
        );
    }
    assert_eq!(
        h.accept_cues(
            h.subtitle_track_id(&id("cc")).unwrap(),
            &[SubtitleCue::new(1, 0, time(0), time(100), "blocked")]
        )
        .unwrap_err()
        .kind(),
        ContinuousErrorKind::WouldBlock
    );
    h.stop();
    h.cancel();
    assert_eq!(
        run.await.unwrap_err().kind(),
        ContinuousErrorKind::Cancelled
    );
    assert!(h.control().progress().iter().all(|p| p.committed() == 0));
}

#[tokio::test]
async fn subtitle_split_keeps_identity_metadata_and_remaining_cue() {
    use std::sync::Mutex;
    struct Parts(Arc<Mutex<Vec<Vec<u8>>>>);
    struct Writer(Arc<Mutex<Vec<Vec<u8>>>>, usize);
    impl tokio::io::AsyncWrite for Writer {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            b: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            self.0.lock().unwrap()[self.1].extend_from_slice(b);
            std::task::Poll::Ready(Ok(b.len()))
        }
        fn poll_flush(
            self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            panic!("caller owns shutdown")
        }
    }
    impl ContinuousWriterProvider for Parts {
        type Writer = Writer;
        fn acquire<'a>(
            &'a mut self,
            r: ContinuousOutputRequest,
        ) -> Pin<Box<dyn Future<Output = ContinuousResult<Writer>> + 'a>> {
            Box::pin(async move {
                assert_eq!(r.all_tracks().len(), 2);
                let mut p = self.0.lock().unwrap();
                let n = p.len();
                p.push(vec![]);
                Ok(Writer(self.0.clone(), n))
            })
        }
    }
    let src = MemorySource::new()
        .segment(
            "https://multi.test/avc",
            include_bytes!("fixtures/sample_crypto/fmp4_avc_clear/seg1.m4s"),
        )
        .segment(
            "https://multi.test/hevc",
            include_bytes!("fixtures/sample_crypto/fmp4_hevc_clear/seg1.m4s"),
        )
        .segment(
            "https://multi.test/a.init",
            include_bytes!("fixtures/sample_crypto/fmp4_avc_clear/init.mp4"),
        )
        .segment(
            "https://multi.test/h.init",
            include_bytes!("fixtures/sample_crypto/fmp4_hevc_clear/init.mp4"),
        );
    let s = MultiTrackSession::new(
        MultiTrackInputs::new(
            ContinuousInput::new(id("main"), Arc::new(src)),
            EmbeddedAudio::Exclude,
        )
        .with_subtitle(SubtitleTrack::new(
            id("cc"),
            id("main"),
            TrackMetadata::new("zh-Hans", "字幕"),
        )),
        sample::keys(Arc::new(sample::Provider)),
        options().with_change_policy(TimelineChangePolicy::Split),
    )
    .unwrap();
    let sink = Arc::new(subtitles::Collector::default());
    let s = s.with_subtitle_sink(sink.clone());
    let h = s.handle();
    h.accept_snapshot(&id("main"),&snapshot("main","#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXT-X-MAP:URI=\"a.init\"\n#EXTINF:2,\navc\n#EXT-X-DISCONTINUITY\n#EXT-X-MAP:URI=\"h.init\"\n#EXTINF:2,\nhevc\n#EXT-X-ENDLIST\n")).unwrap();
    let cc = h.subtitle_track_id(&id("cc")).unwrap();
    h.accept_cues(
        cc,
        &[SubtitleCue::new(1, 0, time(0), time(5000), "spanning").with_identifier("original")],
    )
    .unwrap();
    let parts = Arc::new(Mutex::new(vec![]));
    let r = s.write_to_outputs(&mut Parts(parts.clone())).await.unwrap();
    assert_eq!(parts.lock().unwrap().len(), 2);
    for (index, p) in parts.lock().unwrap().iter().enumerate() {
        assert!(p.windows(8).any(|b| b == b"spanning"));
        let cues = sink.cues.lock().unwrap();
        let intervals = cues
            .iter()
            .filter(|c| c.output_index() == index as u64)
            .map(|c| {
                (
                    c.cue().payload().to_owned(),
                    c.start().ticks(),
                    c.end().ticks(),
                )
            })
            .collect();
        assert_eq!(
            subtitles::interval_union(subtitles::embedded_intervals(p)),
            subtitles::interval_union(intervals)
        );
        assert!(
            cues.iter()
                .all(|c| c.receipt() == 0 && c.cue().identifier() == "original")
        );
    }
    let text: Vec<_> = r
        .tracks()
        .iter()
        .filter(|t| t.kind() == OutputTrackKind::Subtitle)
        .collect();
    assert_eq!(text.len(), 2);
    assert_eq!(text[0].id(), text[1].id());
    assert_eq!(text[1].metadata().language(), "zh-Hans");
    assert!(
        r.subtitle_reports()
            .iter()
            .any(|r| r.output_index() == 1 && r.identifier() == "original")
    );
}

#[tokio::test]
async fn packed_range_and_subtitle_intervals_are_clipped_together() {
    let src = MemorySource::new().segment("https://multi.test/a.bin", packed(0, 200));
    let inputs = MultiTrackInputs::new(
        ContinuousInput::new(id("main"), Arc::new(src)),
        EmbeddedAudio::Keep,
    )
    .with_subtitle(SubtitleTrack::new(
        id("cc"),
        id("main"),
        TrackMetadata::default(),
    ));
    let s = MultiTrackSession::new(
        inputs,
        sample::keys(Arc::new(sample::Provider)),
        ContinuousOptions::default()
            .with_range(PresentationRange::new(time(1000), time(3000)).unwrap()),
    )
    .unwrap();
    let h = s.handle();
    h.accept_snapshot(
        &id("main"),
        &snapshot(
            "main",
            "#EXTM3U\n#EXT-X-TARGETDURATION:5\n#EXTINF:4.65,\na.bin\n#EXT-X-ENDLIST",
        ),
    )
    .unwrap();
    let cc = h.subtitle_track_id(&id("cc")).unwrap();
    h.accept_cues(cc, &[SubtitleCue::new(1, 0, time(0), time(5000), "range")])
        .unwrap();
    let (_, r) = s
        .into_bytes(1024 * 1024, OutputFormat::FragmentedMp4)
        .await
        .unwrap();
    assert!(r.tracks()[0].sample_count() < 200);
    for cue in r.subtitle_reports() {
        assert!(cue.start().ticks() * 1000 >= 1000 * i128::from(cue.start().timescale()));
        assert!(cue.end().ticks() * 1000 <= 3000 * i128::from(cue.end().timescale()));
    }
    assert!(!r.subtitle_reports().is_empty());
}

#[tokio::test]
async fn packed_configuration_changes_fail_between_and_within_segments() {
    for within in [false, true] {
        let mut changed = packed(90000, 2);
        // ADTS sampling_frequency_index 3 (48 kHz), following 44.1 kHz.
        let offset = changed.len() - 18;
        changed[offset + 2] = 0x4c;
        changed[offset + 11] = 0x4c;
        let first = packed(0, 2);
        let (src, text) = if within {
            let mut merged = first.clone();
            merged.extend_from_slice(&changed[offset..]);
            (
                MemorySource::new().segment("https://multi.test/a", merged),
                "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\na\n#EXT-X-ENDLIST",
            )
        } else {
            (
                MemorySource::new()
                    .segment("https://multi.test/a", first)
                    .segment("https://multi.test/b", changed),
                "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\na\n#EXTINF:1,\nb\n#EXT-X-ENDLIST",
            )
        };
        let s = MultiTrackSession::new(
            MultiTrackInputs::new(
                ContinuousInput::new(id("main"), Arc::new(src)),
                EmbeddedAudio::Keep,
            ),
            sample::keys(Arc::new(sample::Provider)),
            ContinuousOptions::default(),
        )
        .unwrap();
        s.handle()
            .accept_snapshot(&id("main"), &snapshot("main", text))
            .unwrap();
        assert!(
            s.into_bytes(1024 * 1024, OutputFormat::FragmentedMp4)
                .await
                .is_err()
        );
    }
}

#[test]
fn cue_budget_is_shared_across_subtitle_tracks() {
    let inputs = MultiTrackInputs::new(
        ContinuousInput::new(id("main"), Arc::new(MemorySource::new())),
        EmbeddedAudio::Keep,
    )
    .with_subtitle(SubtitleTrack::new(
        id("one"),
        id("main"),
        TrackMetadata::default(),
    ))
    .with_subtitle(SubtitleTrack::new(
        id("two"),
        id("main"),
        TrackMetadata::default(),
    ));
    let limits = ContinuousLimits::default().with_samples(1, 4096);
    let s = MultiTrackSession::new(
        inputs,
        sample::keys(Arc::new(sample::Provider)),
        ContinuousOptions::default().with_limits(limits),
    )
    .unwrap();
    let h = s.handle();
    let cue = SubtitleCue::new(1, 0, time(0), time(1000), "one");
    h.accept_cues(
        h.subtitle_track_id(&id("one")).unwrap(),
        std::slice::from_ref(&cue),
    )
    .unwrap();
    assert_eq!(
        h.accept_cues(h.subtitle_track_id(&id("two")).unwrap(), &[cue])
            .unwrap_err()
            .kind(),
        ContinuousErrorKind::WouldBlock
    );
}

#[tokio::test]
async fn pending_cue_admission_wakes_on_independent_end_and_cancel() {
    for cancel in [false, true] {
        let inputs = MultiTrackInputs::new(
            ContinuousInput::new(id("main"), Arc::new(MemorySource::new())),
            EmbeddedAudio::Keep,
        )
        .with_subtitle(SubtitleTrack::new(
            id("cc"),
            id("main"),
            TrackMetadata::default(),
        ));
        let s = MultiTrackSession::new(
            inputs,
            sample::keys(Arc::new(sample::Provider)),
            ContinuousOptions::default()
                .with_limits(ContinuousLimits::default().with_samples(1, 4096)),
        )
        .unwrap();
        let h = s.handle();
        let cc = h.subtitle_track_id(&id("cc")).unwrap();
        let cues = [SubtitleCue::new(1, 0, time(0), time(1000), "pending")];
        h.accept_cues(cc, &cues).unwrap();
        let mut waiting = Box::pin(h.accept_cues_when_ready(cc, &cues));
        assert!(futures_util::poll!(&mut waiting).is_pending());
        if cancel {
            h.cancel();
        } else {
            h.end_subtitles(cc).unwrap();
        }
        assert_eq!(
            waiting.await.unwrap_err().kind(),
            if cancel {
                ContinuousErrorKind::Cancelled
            } else {
                ContinuousErrorKind::Closed
            }
        );
    }
}

#[test]
fn classic_known_decode_gaps_use_native_edits() {
    use hls_engine::legacy::capabilities::*;
    let input = KeyedInputCapability::new(
        KeyedContainer::FragmentedMp4,
        KeyedEncryption::Clear,
        vec![KeyedCodec::AacLc],
    );
    for (output, supported) in [
        (KeyedOutput::NativeStreamingFile, true),
        (KeyedOutput::FragmentedWriter, true),
    ] {
        let q =
            MultiTrackCapabilityQuery::new(id("main"), input.clone(), output, EmbeddedAudio::Keep)
                .with_decode_gaps(true);
        assert_eq!(query_multitrack_capability(&q).supported(), supported);
    }
}

#[tokio::test]
async fn retained_maps_share_one_operation_budget() {
    let mut init = include_bytes!("fixtures/sample_crypto/fmp4_aac_clear/init.mp4").to_vec();
    init.extend_from_slice(&100_000u32.to_be_bytes());
    init.extend_from_slice(b"free");
    init.resize(init.len() + 99_992, 0);
    let src = Arc::new(
        MemorySource::new()
            .segment("https://multi.test/init", init)
            .segment(
                "https://multi.test/media",
                include_bytes!("fixtures/sample_crypto/fmp4_aac_clear/seg1.m4s"),
            ),
    );
    let s = MultiTrackSession::new(
        MultiTrackInputs::new(
            ContinuousInput::new(id("main"), src.clone()),
            EmbeddedAudio::Keep,
        )
        .with_audio(
            ContinuousInput::new(id("other"), src),
            TrackMetadata::default(),
        ),
        sample::keys(Arc::new(sample::Provider)),
        options().with_limits(ContinuousLimits::default().with_samples(10000, 180_000)),
    )
    .unwrap();
    for name in ["main", "other"] {
        s.handle().accept_snapshot(&id(name),&snapshot(name,"#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXT-X-MAP:URI=\"init\"\n#EXTINF:2.005,\nmedia\n#EXT-X-ENDLIST")).unwrap();
    }
    let mut bytes = vec![];
    assert_eq!(
        s.write_to(&mut bytes).await.unwrap_err().kind(),
        ContinuousErrorKind::BudgetExceeded
    );
    assert!(bytes.is_empty());
}

#[tokio::test]
async fn native_classic_audio_gaps_match_memory_finalization() {
    let cases = sample::cases();
    let audio = cases.iter().find(|c| c.name == "fmp4_aac_clear").unwrap();
    let make = || {
        let mut inputs = MultiTrackInputs::new(
            ContinuousInput::new(id("main"), source(audio)),
            EmbeddedAudio::Keep,
        );
        for name in ["en", "ja"] {
            inputs = inputs.with_audio(
                ContinuousInput::new(id(name), source(audio)),
                TrackMetadata::new(name, name),
            );
        }
        inputs = inputs.with_subtitle(SubtitleTrack::new(
            id("cc"),
            id("main"),
            TrackMetadata::default(),
        ));
        let mut opts = options();
        for name in ["main", "en", "ja"] {
            opts = opts.with_anchor(ContinuousAnchor::new(id(name), 1, 1, time(0), time(8000)));
        }
        let session =
            MultiTrackSession::new(inputs, sample::keys(Arc::new(sample::Provider)), opts).unwrap();
        let h = session.handle();
        let body: String = audio
            .playlist
            .lines()
            .filter(|l| {
                !l.starts_with("#EXTM3U")
                    && !l.starts_with("#EXT-X-TARGETDURATION")
                    && !l.starts_with("#EXT-X-PLAYLIST-TYPE")
            })
            .map(|l| format!("{l}\n"))
            .collect();
        let text = audio
            .playlist
            .replace("#EXT-X-ENDLIST", &format!("#EXT-X-DISCONTINUITY\n{body}"));
        for name in ["main", "en", "ja"] {
            h.accept_snapshot(&id(name), &snapshot(name, &text))
                .unwrap();
        }
        let cc = h.subtitle_track_id(&id("cc")).unwrap();
        h.accept_cues(
            cc,
            &[SubtitleCue::new(
                1,
                0,
                time(0),
                time(14000),
                "across the gap",
            )],
        )
        .unwrap();
        h.end_subtitles(cc).unwrap();
        session
    };
    let (memory, expected) = make()
        .into_bytes(8 * 1024 * 1024, OutputFormat::Mp4)
        .await
        .unwrap();
    let path = std::env::temp_dir().join(format!("hls-multitrack-gap-{}.mp4", std::process::id()));
    let actual = make()
        .write_to_file(
            &path,
            FileOutputOptions::default().with_format(OutputFormat::StreamingMp4),
        )
        .await
        .unwrap();
    let file = std::fs::read(&path).unwrap();
    std::fs::remove_file(path).unwrap();
    assert_eq!(sample::canonical(file.clone()), sample::canonical(memory));
    assert_eq!(actual.tracks().len(), expected.tracks().len());
    // These audio tracks start at zero. Interior gaps use composition offsets,
    // so finalization must not introduce repeated audio edits.
    assert!(!file.windows(4).any(|b| b == b"elst"));
    let ctts = file.windows(4).position(|b| b == b"ctts").unwrap();
    let count = u32::from_be_bytes(file[ctts + 8..ctts + 12].try_into().unwrap()) as usize;
    assert!(count >= 2);
    let last_offset = ctts + 16 + (count - 1) * 8;
    assert!(i32::from_be_bytes(file[last_offset..last_offset + 4].try_into().unwrap()) > 0);
    write_fixture("native-gap-classic.mp4", &file);
}
