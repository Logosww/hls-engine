//! Identical GCM media semantics in native Rust, Node WASM and real browsers.
use hls_engine::{crypto::key::*, playlist::*, *};
use sha2::{Digest, Sha256};
use std::sync::Arc;

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
            let rotated = matches!(request.reference().location().location(), SourceLocation::Url(url) if url.path().ends_with("rotated"));
            let start = if rotated { 32 } else { 0 };
            KeyResolution::Available(
                AvailableKey::aes256_gcm(SecretKey::aes256((start..start + 32).collect()).unwrap())
                    .with_version("fixture-v1"),
            )
        })
    }
}
struct Wait;
impl EngineWait for Wait {
    #[cfg(not(target_arch = "wasm32"))]
    fn wait(
        &self,
        _: std::time::Duration,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
    #[cfg(target_arch = "wasm32")]
    fn wait(
        &self,
        _: std::time::Duration,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + '_>> {
        Box::pin(std::future::pending())
    }
}
pub fn input_id() -> InputId {
    InputId::new("primary").unwrap()
}
pub fn rotation_session(
    encrypted: bool,
    options: EngineOptions,
    cp: Option<EngineCheckpoint>,
) -> EngineSession {
    rotation_with_provider(encrypted, options, cp, Arc::new(Provider))
}
pub fn rotation_with_provider(
    encrypted: bool,
    options: EngineOptions,
    cp: Option<EngineCheckpoint>,
    provider: Arc<dyn KeyProvider>,
) -> EngineSession {
    rotation_selected(encrypted, options, cp, provider, false)
}
pub fn rotation_multitrack(
    encrypted: bool,
    options: EngineOptions,
    cp: Option<EngineCheckpoint>,
) -> EngineSession {
    rotation_selected(encrypted, options, cp, Arc::new(Provider), true)
}
fn rotation_selected(
    encrypted: bool,
    options: EngineOptions,
    cp: Option<EngineCheckpoint>,
    provider: Arc<dyn KeyProvider>,
    multi: bool,
) -> EngineSession {
    let source = if encrypted {
        MemorySource::new()
            .segment(
                "https://fixture.test/init",
                include_bytes!("../fixtures/gcm/resource-1.gcm"),
            )
            .segment(
                "https://fixture.test/seg0",
                include_bytes!("../fixtures/gcm/resource-2.gcm"),
            )
            .segment(
                "https://fixture.test/seg1",
                include_bytes!("../fixtures/gcm/resource-4.gcm"),
            )
            .segment(
                "https://fixture.test/seg2",
                include_bytes!("../fixtures/gcm/resource-5.gcm"),
            )
    } else {
        MemorySource::new()
            .segment(
                "https://fixture.test/init",
                include_bytes!("../fixtures/media/fmp4_avc_regular/init.fmp4"),
            )
            .segment(
                "https://fixture.test/seg0",
                include_bytes!("../fixtures/media/fmp4_avc_regular/seg0.m4s"),
            )
            .segment(
                "https://fixture.test/seg1",
                include_bytes!("../fixtures/media/fmp4_avc_regular/seg1.m4s"),
            )
            .segment(
                "https://fixture.test/seg2",
                include_bytes!("../fixtures/media/fmp4_avc_regular/seg2.m4s"),
            )
    };
    let source = Arc::new(source);
    let mut inputs = EngineInputs::new(
        EngineInput::new(input_id(), source.clone()),
        if multi {
            EmbeddedAudio::Exclude
        } else {
            EmbeddedAudio::Keep
        },
    );
    if multi {
        for (name, language) in [("en", "en"), ("ja", "ja")] {
            inputs = inputs.with_audio(
                EngineInput::new(InputId::new(name).unwrap(), source.clone()),
                TrackMetadata::new(language, name),
            );
        }
        inputs = inputs.with_subtitle(SubtitleTrack::new(
            InputId::new("cc").unwrap(),
            input_id(),
            TrackMetadata::new("en", "Captions"),
        ));
    }
    let keys = KeySession::new(
        "gcm",
        "fresh-authorization",
        provider,
        Arc::new(Clock),
        KeySessionOptions::default(),
    )
    .unwrap();
    let options = options
        .with_experimental_gcm(encrypted)
        .with_waiter(Arc::new(Wait), std::time::Duration::from_secs(1));
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(cp) = cp {
        return EngineSession::restore(inputs, keys, options, cp).unwrap();
    }
    #[cfg(target_arch = "wasm32")]
    assert!(cp.is_none());
    EngineSession::new(inputs, keys, options).unwrap()
}
pub fn snapshot(encrypted: bool, end: bool) -> PlaylistSnapshot {
    snapshot_window(encrypted, 0, 3, end)
}
pub fn snapshot_window(encrypted: bool, first: u64, count: u64, end: bool) -> PlaylistSnapshot {
    snapshot_for(input_id(), encrypted, first, count, end)
}
pub fn snapshot_for(
    input: InputId,
    encrypted: bool,
    first: u64,
    count: u64,
    end: bool,
) -> PlaylistSnapshot {
    let mut content = format!(
        "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:{first}\n"
    );
    if encrypted {
        content.push_str("#EXT-X-KEY:METHOD=AES-256-GCM,URI=\"key\"\n");
    }
    content.push_str("#EXT-X-MAP:URI=\"init\"\n");
    for index in first..first + count {
        if encrypted && (index == 1 || index == first && first > 1) {
            content.push_str("#EXT-X-KEY:METHOD=AES-256-GCM,URI=\"rotated\"\n");
        }
        content.push_str(&format!("#EXTINF:2,\nseg{index}\n"));
    }
    if end {
        content.push_str("#EXT-X-ENDLIST\n");
    }
    parse_playlist_snapshot(
        &TextResource {
            location: SourceLocation::Url(url::Url::parse("https://fixture.test/list").unwrap()),
            content,
        },
        PlaylistContext::new(input, 0),
    )
    .unwrap()
}
fn canonical(bytes: &mut [u8]) {
    let mut offset = 0;
    while offset < bytes.len() {
        let n = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        let kind: [u8; 4] = bytes[offset + 4..offset + 8].try_into().unwrap();
        let content = &mut bytes[offset + 8..offset + n];
        match &kind {
            b"moov" | b"trak" | b"mdia" => canonical(content),
            b"mvhd" | b"tkhd" | b"mdhd" => {
                let end = if content[0] == 1 { 20 } else { 12 };
                content[4..end].fill(0);
            }
            _ => {}
        }
        offset += n;
    }
}
pub async fn suite() -> serde_json::Value {
    let mut records = Vec::new();
    for multi in [false, true] {
        for open in [false, true] {
            for ranged in [false, true] {
                for format in [OutputFormat::FragmentedMp4, OutputFormat::Mp4] {
                    let options = || {
                        let mut options = EngineOptions::default().with_mode(if open {
                            EngineMode::Open
                        } else {
                            EngineMode::Vod
                        });
                        if ranged {
                            options = options.with_range(
                                PresentationRange::new(
                                    MediaTime::new(1, 1).unwrap(),
                                    MediaTime::new(5, 1).unwrap(),
                                )
                                .unwrap(),
                            );
                        }
                        options
                    };
                    let mut outputs = Vec::new();
                    for encrypted in [false, true] {
                        let s = if multi {
                            rotation_multitrack(encrypted, options(), None)
                        } else {
                            rotation_session(encrypted, options(), None)
                        };
                        if multi {
                            feed_additional_tracks(&s, encrypted, open);
                        }
                        s.handle()
                            .accept_snapshot(&input_id(), &snapshot(encrypted, !open))
                            .unwrap();
                        s.handle().end_input(&input_id()).unwrap();
                        let (mut bytes, report) =
                            s.into_bytes(8 * 1024 * 1024, format).await.unwrap();
                        canonical(&mut bytes);
                        outputs.push(bytes);
                        if encrypted {
                            records.push(serde_json::json!({"multi":multi,"open":open,"range":ranged,"format":format!("{format:?}"),
                            "bytes":outputs.last().unwrap().len().to_string(),
                            "sha256":format!("{:x}", Sha256::digest(outputs.last().unwrap())),
                            "tracks":report.tracks().iter().map(|t| serde_json::json!({"id":t.id().get(),"timescale":t.timescale(),"samples":t.sample_count().to_string(),"duration":t.duration().to_string()})).collect::<Vec<_>>()}));
                        }
                    }
                    assert_eq!(outputs[0], outputs[1]);
                }
            }
        }
    }
    serde_json::json!({"profile":"hls-draft-22", "clearEncryptedEqual":true,"cases":records})
}

pub fn feed_additional_tracks(session: &EngineSession, encrypted: bool, open: bool) {
    let h = session.handle();
    for (input, count) in [("en", 2), ("ja", 3)] {
        let input = InputId::new(input).unwrap();
        h.accept_snapshot(
            &input,
            &snapshot_for(input.clone(), encrypted, 0, count, !open),
        )
        .unwrap();
        h.end_input(&input).unwrap();
    }
    let cc = h.subtitle_track_id(&InputId::new("cc").unwrap()).unwrap();
    h.accept_cues(
        cc,
        &[SubtitleCue::new(
            0,
            0,
            MediaTime::new(0, 1).unwrap(),
            MediaTime::new(5, 1).unwrap(),
            "authenticated multi-track captions",
        )],
    )
    .unwrap();
    h.end_subtitles(cc).unwrap();
}
