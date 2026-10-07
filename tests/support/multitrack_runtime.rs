//! Shared native / actual WASM vectors. No host timers, filesystem or threads required.
use crate::sample_corpus as sample;
use hls_transmux::{crypto::key::*, playlist::*, *};
use sha2::{Digest, Sha256};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};
pub struct Wait;
impl ContinuousWait for Wait {
    #[cfg(not(target_arch = "wasm32"))]
    fn wait(&self, _: std::time::Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
    #[cfg(target_arch = "wasm32")]
    fn wait(&self, _: std::time::Duration) -> Pin<Box<dyn Future<Output = ()> + '_>> {
        Box::pin(std::future::pending())
    }
}
pub fn id(value: &str) -> InputId {
    InputId::new(value).unwrap()
}
fn snapshot(input: &str, text: &str) -> PlaylistSnapshot {
    parse_playlist_snapshot(
        &TextResource {
            location: SourceLocation::Url(
                url::Url::parse("https://multi.test/input.m3u8").unwrap(),
            ),
            content: text.into(),
        },
        PlaylistContext::new(id(input), 9_007_199_254_740_993),
    )
    .unwrap()
}
fn source(case: &sample::Case) -> Arc<dyn Source> {
    let mut source = MemorySource::new();
    for (file, bytes) in case.files {
        source = source.segment(format!("https://multi.test/{file}"), *bytes);
    }
    Arc::new(source)
}
pub fn packed_cases() -> Vec<sample::Case> {
    macro_rules! case {
        ($name:literal) => {
            sample::Case {
                name: concat!("packed-", $name),
                playlist: include_str!(concat!("../fixtures/packed_aac/", $name, "/input.m3u8")),
                files: &[
                    (
                        "seg0.bin",
                        include_bytes!(concat!("../fixtures/packed_aac/", $name, "/seg0.bin")),
                    ),
                    (
                        "seg1.bin",
                        include_bytes!(concat!("../fixtures/packed_aac/", $name, "/seg1.bin")),
                    ),
                    (
                        "seg2.bin",
                        include_bytes!(concat!("../fixtures/packed_aac/", $name, "/seg2.bin")),
                    ),
                    (
                        "seg3.bin",
                        include_bytes!(concat!("../fixtures/packed_aac/", $name, "/seg3.bin")),
                    ),
                ],
            }
        };
    }
    vec![
        case!("clear"),
        case!("aes128"),
        case!("sample_aes"),
        case!("aes128_rotation"),
        case!("sample_aes_rotation"),
    ]
}
fn record(name: String, bytes: Vec<u8>, report: MultiTrackReport) -> serde_json::Value {
    #[cfg(not(target_arch = "wasm32"))]
    if let Ok(dir) = std::env::var("HLS_MULTITRACK_OUTPUT") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            std::path::Path::new(&dir).join(format!("{name}.mp4")),
            &bytes,
        )
        .unwrap();
    }
    let mut report = serde_json::to_value(report).unwrap();
    report["media"].as_object_mut().unwrap().remove("peaks");
    serde_json::json!({"name":name,"hash":format!("{:x}",Sha256::digest(sample::canonical(bytes))),"report":report})
}
pub async fn run(provider: Arc<dyn KeyProvider>) -> serde_json::Value {
    verify_failures(provider.clone()).await;
    let mut results = vec![];
    for case in packed_cases() {
        for (suffix, format) in [
            ("fragmented", OutputFormat::FragmentedMp4),
            ("classic", OutputFormat::Mp4),
        ] {
            let s = MultiTrackSession::new(
                MultiTrackInputs::new(
                    ContinuousInput::new(id("main"), source(&case)),
                    EmbeddedAudio::Keep,
                ),
                sample::keys(provider.clone()),
                ContinuousOptions::default().with_mode(ContinuousMode::Vod),
            )
            .unwrap();
            s.handle()
                .accept_snapshot(&id("main"), &snapshot("main", case.playlist))
                .unwrap();
            let (bytes, report) = s.into_bytes(8 * 1024 * 1024, format).await.unwrap();
            assert_eq!(report.tracks()[0].timescale(), 44100);
            results.push(record(format!("{}-{suffix}", case.name), bytes, report));
        }
    }
    let cases = sample::cases();
    let video = cases.iter().find(|c| c.name == "fmp4_avc_cenc").unwrap();
    let audio = cases.iter().find(|c| c.name == "fmp4_aac_cbcs").unwrap();
    let packed_audio = packed_cases().pop().unwrap();
    for external_packed in [false, true] {
        let audio = if external_packed {
            &packed_audio
        } else {
            audio
        };
        for open in [false, true] {
            for (suffix, format) in [
                ("fragmented", OutputFormat::FragmentedMp4),
                ("classic", OutputFormat::Mp4),
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
                    id("cc-en"),
                    id("main"),
                    TrackMetadata::new("en", "English captions"),
                ))
                .with_subtitle(SubtitleTrack::new(
                    id("cc-ja"),
                    id("main"),
                    TrackMetadata::new("ja", "日本語"),
                ));
                let holder = Arc::new(Mutex::new(None::<MultiTrackHandle>));
                let callback = holder.clone();
                let options = ContinuousOptions::default()
                    .with_mode(if open {
                        ContinuousMode::Open
                    } else {
                        ContinuousMode::Vod
                    })
                    .with_waiter(Arc::new(Wait), std::time::Duration::from_secs(1))
                    .with_on_event(Arc::new(move |event| {
                        if open && matches!(event, ContinuousEvent::Committed { .. }) {
                            callback.lock().unwrap().as_ref().unwrap().stop();
                        }
                    }));
                let s = MultiTrackSession::new(inputs, sample::keys(provider.clone()), options)
                    .unwrap();
                let h = s.handle();
                *holder.lock().unwrap() = Some(h.clone());
                for (name, case) in [("main", video), ("en", audio), ("ja", audio)] {
                    let text = if open {
                        case.playlist
                            .replace("#EXT-X-ENDLIST", "")
                            .replace("#EXT-X-PLAYLIST-TYPE:VOD", "#EXT-X-PLAYLIST-TYPE:EVENT")
                    } else {
                        case.playlist.to_owned()
                    };
                    h.accept_snapshot(&id(name), &snapshot(name, &text))
                        .unwrap();
                }
                for name in ["cc-en", "cc-ja"] {
                    let track = h.subtitle_track_id(&id(name)).unwrap();
                    h.accept_cues(
                        track,
                        &[
                            SubtitleCue::new(
                                9_007_199_254_740_993,
                                0,
                                MediaTime::new(0, 1000).unwrap(),
                                MediaTime::new(1000, 1000).unwrap(),
                                if name == "cc-ja" {
                                    "こんにちは"
                                } else {
                                    "Hello"
                                },
                            )
                            .with_identifier("first")
                            .with_settings("align:start position:10%"),
                            SubtitleCue::new(
                                9_007_199_254_740_993,
                                0,
                                MediaTime::new(500, 1000).unwrap(),
                                MediaTime::new(1500, 1000).unwrap(),
                                "Overlap",
                            )
                            .with_identifier("second"),
                        ],
                    )
                    .unwrap();
                    for (index, settings) in [
                        "align:end position:90%,line-right line:20%,center size:50%",
                        "vertical:rl align:start position:20% line:2",
                        "vertical:lr align:left position:50%,center line:-1,end",
                        "align:right position:80%",
                        "align:center position:10%,line-left line:1,start",
                    ]
                    .iter()
                    .enumerate()
                    {
                        let start = 2000 + index as i128 * 600;
                        h.accept_cues(
                            track,
                            &[SubtitleCue::new(
                                9_007_199_254_740_993,
                                0,
                                MediaTime::new(start, 1000).unwrap(),
                                MediaTime::new(start + 500, 1000).unwrap(),
                                format!("Settings {index}"),
                            )
                            .with_settings(*settings)],
                        )
                        .unwrap();
                    }
                    h.end_subtitles(track).unwrap();
                }
                let (bytes, report) = s.into_bytes(8 * 1024 * 1024, format).await.unwrap();
                assert_eq!(report.tracks().len(), 5);
                results.push(record(
                    format!(
                        "multitrack-{}{}{suffix}",
                        if external_packed { "packed-" } else { "" },
                        if open { "open-" } else { "" }
                    ),
                    bytes,
                    report,
                ));
            }
        }
    }
    results.extend(cross_product(provider.clone()).await);
    results.extend(split_rotations(provider).await);
    serde_json::json!(results)
}

async fn verify_failures(provider: Arc<dyn KeyProvider>) {
    let source = Arc::new(MemorySource::new().segment(
        "https://multi.test/bad",
        vec![0xff, 0xf1, 0x50, 0x80, 0x01, 0x3f, 0xfc, 1, 2],
    ));
    let s = MultiTrackSession::new(
        MultiTrackInputs::new(
            ContinuousInput::new(id("main"), source),
            EmbeddedAudio::Keep,
        )
        .with_subtitle(SubtitleTrack::new(
            id("cc"),
            id("main"),
            TrackMetadata::default(),
        )),
        sample::keys(provider),
        ContinuousOptions::default(),
    )
    .unwrap();
    let h = s.handle();
    let cc = h.subtitle_track_id(&id("cc")).unwrap();
    let t = |n| MediaTime::new(n, 1000).unwrap();
    assert_eq!(
        h.accept_cues(cc, &[SubtitleCue::new(0, 0, t(0), t(1), "<b>style</b>")])
            .unwrap_err()
            .kind(),
        ContinuousErrorKind::UnsupportedSubtitleProfile
    );
    assert_eq!(
        h.accept_cues(cc, &[SubtitleCue::new(0, 0, t(2), t(1), "bad interval")])
            .unwrap_err()
            .kind(),
        ContinuousErrorKind::InvalidSubtitle
    );
    h.accept_snapshot(
        &id("main"),
        &snapshot(
            "main",
            "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\nbad\n#EXT-X-ENDLIST",
        ),
    )
    .unwrap();
    let mut bytes = vec![];
    assert_eq!(
        s.write_to(&mut bytes).await.unwrap_err().kind(),
        ContinuousErrorKind::Resource
    );
    assert!(bytes.is_empty());
    assert_eq!(
        h.accept_cues(cc, &[]).unwrap_err().kind(),
        ContinuousErrorKind::Closed
    );
}

// Cross-product vectors are shared by native, Node WASM and Chrome. Each
// encrypted vector has an otherwise identical clear reference.
async fn cross_product(provider: Arc<dyn KeyProvider>) -> Vec<serde_json::Value> {
    let cases = sample::cases();
    let packed = packed_cases();
    let lookup = |name: &str| {
        cases
            .iter()
            .chain(&packed)
            .find(|c| c.name == name)
            .unwrap()
    };
    let mut rows = vec![];
    for video_kind in ["fmp4", "ts", "packed"] {
        for (family, clear_audio, encrypted_audio) in [
            ("packed-aes", "packed-clear", "packed-aes128_rotation"),
            (
                "packed-sample",
                "packed-clear",
                "packed-sample_aes_rotation",
            ),
            ("fmp4", "fmp4_aac_clear", "fmp4_aac_cbcs"),
            ("ts", "ts_aac_clear", "ts_aac_sample"),
        ] {
            if video_kind == "packed" && !family.starts_with("packed-") {
                continue;
            }
            for scenario in ["range", "epochs"] {
                for open in [false, true] {
                    for (suffix, format) in [
                        ("fragmented", OutputFormat::FragmentedMp4),
                        ("classic", OutputFormat::Mp4),
                    ] {
                        let mut clear_hash = None;
                        for encrypted in [false, true] {
                            let video = lookup(match (video_kind, encrypted) {
                                ("packed", true) => encrypted_audio,
                                ("packed", false) => clear_audio,
                                ("ts", true) => "ts_avc_sample",
                                ("ts", false) => "ts_avc_clear",
                                (_, true) => "fmp4_avc_cenc",
                                _ => "fmp4_avc_clear",
                            });
                            let audio = lookup(if encrypted {
                                encrypted_audio
                            } else {
                                clear_audio
                            });
                            let name = format!(
                                "matrix-{video_kind}-{family}-{scenario}-{}-{}-{suffix}",
                                if open { "open" } else { "vod" },
                                if encrypted { "encrypted" } else { "clear" }
                            );
                            let mut inputs = MultiTrackInputs::new(
                                ContinuousInput::new(id("main"), source(video)),
                                if video_kind == "packed" {
                                    EmbeddedAudio::Keep
                                } else {
                                    EmbeddedAudio::Exclude
                                },
                            );
                            for (input, language) in [("en", "en"), ("ja", "ja")] {
                                inputs = inputs.with_audio(
                                    ContinuousInput::new(id(input), source(audio)),
                                    TrackMetadata::new(language, input).with_default(input == "en"),
                                );
                            }
                            inputs = inputs.with_subtitle(SubtitleTrack::new(
                                id("cc"),
                                id("main"),
                                TrackMetadata::new("en", "Captions"),
                            ));
                            let mut opts = ContinuousOptions::default()
                                .with_mode(if open {
                                    ContinuousMode::Open
                                } else {
                                    ContinuousMode::Vod
                                })
                                .with_waiter(Arc::new(Wait), std::time::Duration::from_secs(1));
                            if scenario == "range" {
                                opts = opts.with_range(
                                    PresentationRange::new(
                                        MediaTime::new(250, 1000).unwrap(),
                                        MediaTime::new(1100, 1000).unwrap(),
                                    )
                                    .unwrap(),
                                );
                            }
                            // Epoch 1 deliberately leaves a gap after epoch 0 on
                            // each input, including short Packed audio renditions.
                            if scenario == "epochs" {
                                for input in ["main", "en", "ja"] {
                                    opts = opts.with_anchor(ContinuousAnchor::new(
                                        id(input),
                                        9_007_199_254_740_993,
                                        1,
                                        MediaTime::new(0, 1000).unwrap(),
                                        MediaTime::new(8000, 1000).unwrap(),
                                    ));
                                }
                            }
                            let session = MultiTrackSession::new(
                                inputs,
                                sample::keys(provider.clone()),
                                opts,
                            )
                            .unwrap();
                            let h = session.handle();
                            for (input, case) in [("main", video), ("en", audio), ("ja", audio)] {
                                let mut text = case.playlist.to_owned();
                                if scenario == "epochs" {
                                    let body: String = case
                                        .playlist
                                        .lines()
                                        .filter(|l| {
                                            !l.starts_with("#EXTM3U")
                                                && !l.starts_with("#EXT-X-TARGETDURATION")
                                                && !l.starts_with("#EXT-X-PLAYLIST-TYPE")
                                                && !l.starts_with("#EXT-X-VERSION")
                                                && !l.starts_with("#EXT-X-ENDLIST")
                                        })
                                        .map(|l| format!("{l}\n"))
                                        .collect();
                                    text = text.replace(
                                        "#EXT-X-ENDLIST",
                                        &format!("#EXT-X-DISCONTINUITY\n{body}#EXT-X-ENDLIST"),
                                    );
                                }
                                if open {
                                    text = text
                                        .replace("#EXT-X-ENDLIST", "")
                                        .replace("PLAYLIST-TYPE:VOD", "PLAYLIST-TYPE:EVENT");
                                }
                                h.accept_snapshot(&id(input), &snapshot(input, &text))
                                    .unwrap();
                            }
                            let cc = h.subtitle_track_id(&id("cc")).unwrap();
                            for epoch in 0..if scenario == "epochs" { 2 } else { 1 } {
                                h.accept_cues(
                                    cc,
                                    &[SubtitleCue::new(
                                        9_007_199_254_740_993,
                                        epoch,
                                        MediaTime::new(0, 1000).unwrap(),
                                        MediaTime::new(1000, 1000).unwrap(),
                                        "matrix captions",
                                    )
                                    .with_identifier(format!("epoch-{epoch}"))],
                                )
                                .unwrap();
                            }
                            h.end_subtitles(cc).unwrap();
                            if open {
                                h.stop();
                            }
                            let (bytes, report) = session
                                .into_bytes(16 * 1024 * 1024, format)
                                .await
                                .unwrap_or_else(|e| panic!("{name}: {e:?}, {:?}", e.raw_cause()));
                            assert_eq!(report.tracks().len(), 4, "{name}");
                            let mut row = record(name.clone(), bytes, report);
                            if encrypted {
                                assert_eq!(clear_hash.as_ref().unwrap(), &row["hash"], "{name}");
                            } else {
                                clear_hash = Some(row["hash"].clone());
                            }
                            row["clearReferenceEqual"] = serde_json::json!(true);
                            rows.push(row);
                        }
                    }
                }
            }
        }
    }
    rows
}

async fn split_rotations(provider: Arc<dyn KeyProvider>) -> Vec<serde_json::Value> {
    struct Parts(Arc<Mutex<Vec<Vec<u8>>>>);
    struct Writer(Arc<Mutex<Vec<Vec<u8>>>>, usize);
    impl tokio::io::AsyncWrite for Writer {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            b: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            let n = b.len().min(4096);
            self.0.lock().unwrap()[self.1].extend_from_slice(&b[..n]);
            std::task::Poll::Ready(Ok(n))
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
            panic!("caller owns close")
        }
    }
    impl ContinuousWriterProvider for Parts {
        type Writer = Writer;
        fn acquire<'a>(
            &'a mut self,
            r: ContinuousOutputRequest,
        ) -> Pin<Box<dyn Future<Output = ContinuousResult<Writer>> + 'a>> {
            Box::pin(async move {
                assert_eq!(r.all_tracks().len(), 4);
                let mut parts = self.0.lock().unwrap();
                let n = parts.len();
                parts.push(vec![]);
                Ok(Writer(self.0.clone(), n))
            })
        }
    }
    let cases = sample::cases();
    let packed = packed_cases();
    let mut rows = vec![];
    for family in ["aes128_rotation", "sample_aes_rotation"] {
        for open in [false, true] {
            let mut clear_hashes = vec![];
            for encrypted in [false, true] {
                let avc = cases
                    .iter()
                    .find(|c| {
                        c.name
                            == if encrypted {
                                "fmp4_avc_cenc"
                            } else {
                                "fmp4_avc_clear"
                            }
                    })
                    .unwrap();
                let hevc = cases
                    .iter()
                    .find(|c| {
                        c.name
                            == if encrypted {
                                "fmp4_hevc_cbcs"
                            } else {
                                "fmp4_hevc_clear"
                            }
                    })
                    .unwrap();
                let audio = packed
                    .iter()
                    .find(|c| {
                        c.name
                            == if encrypted {
                                format!("packed-{family}")
                            } else {
                                "packed-clear".to_owned()
                            }
                    })
                    .unwrap();
                let mut src = MemorySource::new();
                let mut video_text = "#EXTM3U\n#EXT-X-TARGETDURATION:4\n".to_owned();
                for (prefix, case) in [("avc", avc), ("hevc", hevc)] {
                    if prefix == "hevc" {
                        video_text.push_str("#EXT-X-DISCONTINUITY\n");
                    }
                    let mut body = case.playlist.to_owned();
                    for (file, bytes) in case.files {
                        src = src.segment(format!("https://multi.test/{prefix}/{file}"), *bytes);
                        body = body.replace(file, &format!("{prefix}/{file}"));
                    }
                    for line in body.lines().filter(|l| {
                        !l.starts_with("#EXTM3U")
                            && !l.starts_with("#EXT-X-TARGETDURATION")
                            && !l.starts_with("#EXT-X-PLAYLIST-TYPE")
                            && !l.starts_with("#EXT-X-ENDLIST")
                    }) {
                        video_text.push_str(line);
                        video_text.push('\n');
                    }
                }
                video_text.push_str("#EXT-X-ENDLIST\n");
                let inputs = MultiTrackInputs::new(
                    ContinuousInput::new(id("main"), Arc::new(src)),
                    EmbeddedAudio::Exclude,
                )
                .with_audio(
                    ContinuousInput::new(id("en"), source(audio)),
                    TrackMetadata::new("en", "English"),
                )
                .with_audio(
                    ContinuousInput::new(id("ja"), source(audio)),
                    TrackMetadata::new("ja", "Japanese"),
                )
                .with_subtitle(SubtitleTrack::new(
                    id("cc"),
                    id("main"),
                    TrackMetadata::new("en", "Captions"),
                ));
                let mut opts = ContinuousOptions::default()
                    .with_change_policy(TimelineChangePolicy::Split)
                    .with_mode(if open {
                        ContinuousMode::Open
                    } else {
                        ContinuousMode::Vod
                    })
                    .with_waiter(Arc::new(Wait), std::time::Duration::from_secs(1));
                for input in ["main", "en", "ja"] {
                    opts = opts.with_anchor(ContinuousAnchor::new(
                        id(input),
                        9_007_199_254_740_993,
                        1,
                        MediaTime::new(0, 1000).unwrap(),
                        MediaTime::new(6000, 1000).unwrap(),
                    ));
                }
                let session =
                    MultiTrackSession::new(inputs, sample::keys(provider.clone()), opts).unwrap();
                let h = session.handle();
                let audio_text = audio.playlist.replace(
                    "#EXT-X-ENDLIST",
                    &format!(
                        "#EXT-X-DISCONTINUITY\n{}",
                        audio
                            .playlist
                            .lines()
                            .filter(|l| !l.starts_with("#EXTM3U")
                                && !l.starts_with("#EXT-X-TARGETDURATION")
                                && !l.starts_with("#EXT-X-VERSION"))
                            .map(|l| format!("{l}\n"))
                            .collect::<String>()
                    ),
                );
                for (input, text) in [
                    ("main", &video_text),
                    ("en", &audio_text),
                    ("ja", &audio_text),
                ] {
                    let text = if open {
                        text.replace("#EXT-X-ENDLIST", "")
                    } else {
                        text.clone()
                    };
                    h.accept_snapshot(&id(input), &snapshot(input, &text))
                        .unwrap_or_else(|e| panic!("split {input} {family} open={open} encrypted={encrypted}: {e:?}\n{text}"));
                }
                let cc = h.subtitle_track_id(&id("cc")).unwrap();
                h.accept_cues(
                    cc,
                    &[SubtitleCue::new(
                        9_007_199_254_740_993,
                        0,
                        MediaTime::new(0, 1000).unwrap(),
                        MediaTime::new(12000, 1000).unwrap(),
                        "spanning split",
                    )
                    .with_identifier("span")],
                )
                .unwrap();
                h.end_subtitles(cc).unwrap();
                if open {
                    h.stop();
                }
                let parts = Arc::new(Mutex::new(vec![]));
                let report = session
                    .write_to_outputs(&mut Parts(parts.clone()))
                    .await
                    .unwrap();
                let parts = std::mem::take(&mut *parts.lock().unwrap());
                assert_eq!(parts.len(), 2);
                assert_eq!(report.tracks().len(), 8);
                for (a, b) in report.tracks()[..4].iter().zip(&report.tracks()[4..]) {
                    assert_eq!(a.id(), b.id());
                    assert_eq!(a.metadata().language(), b.metadata().language());
                }
                for (index, bytes) in parts.into_iter().enumerate() {
                    assert!(bytes.windows(14).any(|s| s == b"spanning split"));
                    let row = record(
                        format!(
                            "split-{family}-{}-{}-{index}",
                            if open { "open" } else { "vod" },
                            if encrypted { "encrypted" } else { "clear" }
                        ),
                        bytes,
                        report.clone(),
                    );
                    if encrypted {
                        assert_eq!(clear_hashes[index], row["hash"]);
                    } else {
                        clear_hashes.push(row["hash"].clone());
                    }
                    rows.push(row);
                }
            }
        }
    }
    rows
}
