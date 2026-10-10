//! Shared production-path assertions, executed unchanged on native and WASM.
#![allow(dead_code)]
use hls_engine::legacy::{
    crypto::{key::*, resource::*},
    playlist::*,
    *,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
pub fn id(s: &str) -> InputId {
    InputId::new(s).unwrap()
}
pub fn time(ms: i128) -> MediaTime {
    MediaTime::new(ms, 1000).unwrap()
}
pub struct Clock;
impl KeyClock for Clock {
    fn now(&self) -> u64 {
        0
    }
}
pub struct Provider {
    pub version: AtomicUsize,
    pub calls: AtomicUsize,
}
impl Default for Provider {
    fn default() -> Self {
        Self {
            version: AtomicUsize::new(1),
            calls: AtomicUsize::new(0),
        }
    }
}
impl KeyProvider for Provider {
    fn resolve(&self, request: KeyRequest) -> KeyFuture<KeyResolution> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let version = self.version.load(Ordering::SeqCst);
        let rotated = matches!(request.reference().location().location(), SourceLocation::Url(u) if u.path().ends_with("rotated"));
        Box::pin(async move {
            KeyResolution::Available(
                AvailableKey::aes128(
                    SecretKey::new(if rotated {
                        (16..32).collect()
                    } else {
                        (0..16).collect()
                    })
                    .unwrap(),
                )
                .with_version(format!("v{version}")),
            )
        })
    }
}
pub fn keys(provider: Arc<dyn KeyProvider>) -> KeySession {
    KeySession::new(
        "subtitle-test",
        "fixture",
        provider,
        Arc::new(Clock),
        KeySessionOptions::default(),
    )
    .unwrap()
}
pub fn snapshot(input: &str, text: &str) -> PlaylistSnapshot {
    parse_playlist_snapshot(
        &TextResource {
            location: SourceLocation::Url(
                url::Url::parse("https://subtitle.test/input.m3u8").unwrap(),
            ),
            content: text.into(),
        },
        PlaylistContext::new(id(input), 7),
    )
    .unwrap()
}
pub const CLEAR: &[u8] = include_bytes!("../fixtures/webvtt/segment.vtt");
pub const HEADER: &[u8] = include_bytes!("../fixtures/webvtt/header.vtt");
pub const SEQUENCE: &[u8] = include_bytes!("../fixtures/webvtt/sequence.cbc");
pub const EXPLICIT: &[u8] = include_bytes!("../fixtures/webvtt/explicit.cbc");
pub const ROTATED: &[u8] = include_bytes!("../fixtures/webvtt/rotated.cbc");
pub const ENCRYPTED_HEADER: &[u8] = include_bytes!("../fixtures/webvtt/header.cbc");
pub async fn resources() -> serde_json::Value {
    let provider = Arc::new(Provider::default());
    let resources =
        ResourceSession::new(keys(provider.clone()), ResourceOptions::default()).unwrap();
    let source = Arc::new(
        MemorySource::new()
            .segment("https://subtitle.test/seq", SEQUENCE)
            .segment("https://subtitle.test/explicit", EXPLICIT)
            .segment("https://subtitle.test/rot", ROTATED)
            .segment("https://subtitle.test/clear", CLEAR)
            .segment("https://subtitle.test/header", HEADER)
            .segment("https://subtitle.test/encrypted-header", ENCRYPTED_HEADER),
    );
    for mode in [
        "",
        "#EXT-X-PLAYLIST-TYPE:EVENT\n",
        "#EXT-X-PLAYLIST-TYPE:VOD\n",
    ] {
        let end = if mode.contains("VOD") {
            "#EXT-X-ENDLIST\n"
        } else {
            ""
        };
        let p = snapshot(
            "cc",
            &format!(
                "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:42\n{mode}#EXT-X-MAP:URI=\"header\"\n#EXT-X-KEY:METHOD=AES-128,URI=\"key\"\n#EXTINF:1,\nseq\n#EXT-X-KEY:METHOD=AES-128,URI=\"rotated\"\n#EXTINF:1,\nrot\n#EXT-X-KEY:METHOD=NONE\n#EXTINF:1,\nclear\n{end}"
            ),
        );
        for i in 0..3 {
            let request = ResourceRequest::webvtt_media(&p, i).unwrap();
            assert_eq!(request.resource().slot().sequence(), 42 + i as u64);
            let clear = resources.read(source.clone(), request).await.unwrap();
            assert_eq!(clear.container(), ClearContainer::WebVtt);
            assert_eq!(clear.bytes(), CLEAR);
            assert_eq!(clear.key_version().is_some(), i < 2);
            if i < 2 {
                assert_eq!(clear.iv(), Some(sequence_iv(42 + i as u64)));
            }
            let header = resources
                .read(source.clone(), ResourceRequest::webvtt_map(&p, i).unwrap())
                .await
                .unwrap();
            assert_eq!(header.container(), ClearContainer::WebVttHeader);
            assert_eq!(header.bytes(), HEADER);
            assert!(header.key_version().is_none());
        }
    }
    let p = snapshot(
        "cc",
        "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-KEY:METHOD=AES-128,URI=\"key\",IV=0x000102030405060708090a0b0c0d0e0f\n#EXT-X-MAP:URI=\"encrypted-header\"\n#EXTINF:1,\nexplicit\n#EXT-X-ENDLIST\n",
    );
    let old = resources
        .read(
            source.clone(),
            ResourceRequest::webvtt_media(&p, 0).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(old.bytes(), CLEAR);
    assert_eq!(
        resources
            .read(source.clone(), ResourceRequest::webvtt_map(&p, 0).unwrap())
            .await
            .unwrap()
            .bytes(),
        HEADER
    );
    assert_eq!(
        resources
            .read(source.clone(), ResourceRequest::media(&p, 0).unwrap())
            .await
            .unwrap_err()
            .kind(),
        ResourceErrorKind::MediaValidation
    );
    let cue = SubtitleCue::new(7, 0, time(0), time(1000), "hello")
        .with_resource(&old)
        .unwrap();
    provider.version.store(2, Ordering::SeqCst);
    resources.invalidate_keys().unwrap();
    let new = resources
        .read(source, ResourceRequest::webvtt_media(&p, 0).unwrap())
        .await
        .unwrap();
    let changed = SubtitleCue::new(7, 0, time(0), time(1000), "hello")
        .with_resource(&new)
        .unwrap();
    assert_ne!(cue.source_identity(), changed.source_identity());
    serde_json::json!({"playlistModes":3,"rotation":true,"map":true,"strictMedia":true,"versionEvidence":true})
}
#[derive(Default)]
pub struct Collector {
    pub cues: Mutex<Vec<CommittedSubtitleCue>>,
    pub frontiers: Mutex<Vec<SubtitleFrontier>>,
    pub closes: AtomicUsize,
}
impl SubtitleSink for Collector {
    fn commit<'a>(&'a self, batch: &'a SubtitleCommit) -> SubtitleSinkFuture<'a> {
        Box::pin(async move {
            self.cues.lock().unwrap().extend_from_slice(batch.cues());
            self.frontiers
                .lock()
                .unwrap()
                .extend_from_slice(batch.frontiers());
            Ok(())
        })
    }
    fn finish(&self) -> SubtitleSinkFuture<'_> {
        Box::pin(async move {
            self.closes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}
pub fn inputs(embedded: bool) -> MultiTrackInputs {
    let source = Arc::new(
        MemorySource::new()
            .segment(
                "https://subtitle.test/init",
                include_bytes!("../fixtures/media/fmp4_aac_audio_only/init.fmp4"),
            )
            .segment(
                "https://subtitle.test/seg0",
                include_bytes!("../fixtures/media/fmp4_aac_audio_only/seg0.m4s"),
            )
            .segment(
                "https://subtitle.test/seg1",
                include_bytes!("../fixtures/media/fmp4_aac_audio_only/seg1.m4s"),
            )
            .segment(
                "https://subtitle.test/seg2",
                include_bytes!("../fixtures/media/fmp4_aac_audio_only/seg2.m4s"),
            ),
    );
    MultiTrackInputs::new(
        ContinuousInput::new(id("media"), source),
        EmbeddedAudio::Keep,
    )
    .with_subtitle(
        SubtitleTrack::new(id("cc"), id("media"), TrackMetadata::new("en", "Captions"))
            .with_embedded(embedded),
    )
}
pub fn session(embedded: bool, options: ContinuousOptions) -> MultiTrackSession {
    MultiTrackSession::new(
        inputs(embedded),
        keys(Arc::new(Provider::default())),
        options,
    )
    .unwrap()
}
pub fn feed(session: &MultiTrackSession) -> Vec<u64> {
    let h = session.handle();
    h.accept_snapshot(&id("media"), &snapshot("media", "#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXT-X-MAP:URI=\"init\"\n#EXTINF:2,\nseg0\n#EXTINF:2,\nseg1\n#EXTINF:2,\nseg2\n#EXT-X-ENDLIST\n")).unwrap();
    let track = h.subtitle_track_id(&id("cc")).unwrap();
    let accepted = h
        .accept_cues(
            track,
            &[
                SubtitleCue::new(7, 0, time(123), time(8000), "spanning")
                    .with_identifier("same")
                    .with_settings("align:start"),
                SubtitleCue::new(7, 0, time(500), time(900), "overlap").with_identifier("same"),
            ],
        )
        .unwrap();
    h.end_subtitles(track).unwrap();
    vec![
        accepted.first_receipt().unwrap(),
        accepted.first_receipt().unwrap() + 1,
    ]
}
pub async fn sidecars() -> serde_json::Value {
    let mut expected = None;
    for embedded in [true, false] {
        let sink = Arc::new(Collector::default());
        let session = session(
            embedded,
            ContinuousOptions::default().with_limits(ContinuousLimits::default().with_history(1)),
        )
        .with_subtitle_sink(sink.clone());
        let receipts = feed(&session);
        let (bytes, report) = session
            .into_bytes(2 * 1024 * 1024, OutputFormat::FragmentedMp4)
            .await
            .unwrap();
        assert_eq!(sink.closes.load(Ordering::SeqCst), 1);
        assert_eq!(bytes.windows(4).any(|w| w == b"wvtt"), embedded);
        let cues = sink.cues.lock().unwrap();
        assert!(cues.len() > report.subtitle_reports().len());
        assert!(report.subtitle_history_truncated());
        assert!(
            cues.iter()
                .any(|c| c.receipt() == receipts[1] && c.cue().payload() == "overlap")
        );
        if embedded {
            let sidecar = cues
                .iter()
                .map(|c| {
                    (
                        c.cue().payload().to_string(),
                        c.start().ticks(),
                        c.end().ticks(),
                    )
                })
                .collect();
            assert_eq!(
                interval_union(embedded_intervals(&bytes)),
                interval_union(sidecar)
            );
        }
        let spanning: Vec<_> = cues.iter().filter(|c| c.receipt() == receipts[0]).collect();
        assert!(spanning.len() > 1);
        assert_eq!(
            spanning.first().unwrap().start(),
            MediaTime::new(11070, 90000).unwrap()
        );
        assert_eq!(
            spanning.last().unwrap().end(),
            MediaTime::new(720000, 90000).unwrap()
        );
        for pair in spanning.windows(2) {
            assert_eq!(pair[0].end(), pair[1].start());
        }
        let timing: Vec<_> = cues
            .iter()
            .map(|c| (c.receipt(), c.start().ticks(), c.end().ticks()))
            .collect();
        if let Some(expected) = &expected {
            assert_eq!(&timing, expected);
        } else {
            expected = Some(timing);
        }
        let frontiers = sink.frontiers.lock().unwrap();
        assert_eq!(
            frontiers.last().unwrap().end(),
            MediaTime::new(720000, 90000).unwrap()
        );
        for pair in frontiers.windows(2) {
            assert!(pair[0].end().ticks() <= pair[1].end().ticks());
        }
    }
    serde_json::json!({"timing":expected,"duplicateIds":true,"embeddedParity":true,"closed":true})
}
pub async fn run() -> serde_json::Value {
    serde_json::json!({"resources":resources().await,"sidecars":sidecars().await,"controls":controls().await,"ranges":ranges().await,"longOpen":long_open().await,"gaps":gaps().await})
}

pub struct GateSink {
    pub mode: AtomicUsize,
    pub calls: AtomicUsize,
    pub closes: AtomicUsize,
}
impl GateSink {
    fn new(mode: usize) -> Self {
        Self {
            mode: AtomicUsize::new(mode),
            calls: AtomicUsize::new(0),
            closes: AtomicUsize::new(0),
        }
    }
}
impl SubtitleSink for GateSink {
    fn commit<'a>(&'a self, _: &'a SubtitleCommit) -> SubtitleSinkFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::poll_fn(|_| {
            match self.mode.load(Ordering::SeqCst) {
                0 => std::task::Poll::Pending,
                2 => std::task::Poll::Ready(Err(std::io::Error::other("private write cause"))),
                _ => std::task::Poll::Ready(Ok(())),
            }
        }))
    }
    fn finish(&self) -> SubtitleSinkFuture<'_> {
        self.closes.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if self.mode.load(Ordering::SeqCst) == 3 {
                Err(std::io::Error::other("private close cause"))
            } else {
                Ok(())
            }
        })
    }
}
struct Writer {
    writes: Arc<AtomicUsize>,
    mode: Arc<AtomicUsize>,
}
impl tokio::io::AsyncWrite for Writer {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        use std::task::Poll;
        if self.writes.load(Ordering::SeqCst) > 0 {
            match self.mode.load(Ordering::SeqCst) {
                0 => return Poll::Pending,
                2 => return Poll::Ready(Err(std::io::Error::other("media failed"))),
                _ => (),
            }
        }
        self.writes.fetch_add(1, Ordering::SeqCst);
        Poll::Ready(Ok(bytes.len()))
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        panic!("borrowed writer must not be closed")
    }
}
pub async fn controls() -> serde_json::Value {
    use std::{
        future::Future,
        task::{Context, Waker},
    };
    let mut cx = Context::from_waker(Waker::noop());
    for cancel in [false, true] {
        let sink = Arc::new(GateSink::new(0));
        let s = session(true, ContinuousOptions::default()).with_subtitle_sink(sink.clone());
        let h = s.handle();
        feed(&s);
        let writes = Arc::new(AtomicUsize::new(0));
        let mode = Arc::new(AtomicUsize::new(0));
        let mut writer = Writer {
            writes: writes.clone(),
            mode: mode.clone(),
        };
        let mut running = Box::pin(s.write_to(&mut writer));
        assert!(running.as_mut().poll(&mut cx).is_pending());
        assert_eq!(
            sink.calls.load(Ordering::SeqCst),
            0,
            "no subtitle commit before media write"
        );
        mode.store(1, Ordering::SeqCst);
        assert!(running.as_mut().poll(&mut cx).is_pending());
        assert_eq!(sink.calls.load(Ordering::SeqCst), 1);
        let count = writes.load(Ordering::SeqCst);
        assert!(running.as_mut().poll(&mut cx).is_pending());
        assert_eq!(
            writes.load(Ordering::SeqCst),
            count,
            "only one batch may await acknowledgement"
        );
        h.stop();
        assert!(
            running.as_mut().poll(&mut cx).is_pending(),
            "stop must drain"
        );
        if cancel {
            h.cancel();
            assert_eq!(
                running.await.unwrap_err().kind(),
                ContinuousErrorKind::Cancelled
            );
            sink.mode.store(1, Ordering::SeqCst);
            assert_eq!(sink.closes.load(Ordering::SeqCst), 0);
        } else {
            sink.mode.store(1, Ordering::SeqCst);
            running.await.unwrap();
            assert_eq!(sink.closes.load(Ordering::SeqCst), 1);
        }
    }
    for mode in [2, 3] {
        let sink = Arc::new(GateSink::new(mode));
        let s = session(false, ContinuousOptions::default()).with_subtitle_sink(sink.clone());
        feed(&s);
        let error = s
            .into_bytes(2 * 1024 * 1024, OutputFormat::FragmentedMp4)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ContinuousErrorKind::SubtitleOutput);
        assert!(!format!("{error:?}").contains("private"));
        assert_eq!(sink.closes.load(Ordering::SeqCst), usize::from(mode == 3));
    }
    let sink = Arc::new(Collector::default());
    let s = session(true, ContinuousOptions::default()).with_subtitle_sink(sink.clone());
    feed(&s);
    let mut writer = Writer {
        writes: Arc::new(AtomicUsize::new(0)),
        mode: Arc::new(AtomicUsize::new(2)),
    };
    assert_eq!(
        s.write_to(&mut writer).await.unwrap_err().kind(),
        ContinuousErrorKind::Output
    );
    assert!(sink.cues.lock().unwrap().is_empty());
    serde_json::json!({"mediaBeforeCues":true,"acknowledged":true,"cancel":true,"stopDrains":true,"writeAndCloseFailure":true})
}
pub async fn ranges() -> serde_json::Value {
    let p = snapshot(
        "cc",
        &format!(
            "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:42\n#EXT-X-KEY:METHOD=AES-128,URI=\"key\"\n#EXT-X-BYTERANGE:{}@3\n#EXTINF:1,\nbundle\n",
            SEQUENCE.len()
        ),
    );
    let request = ResourceRequest::webvtt_media(&p, 0).unwrap();
    let mut bytes = vec![255; 3];
    bytes.extend_from_slice(SEQUENCE);
    let source = Arc::new(MemorySource::new().segment("https://subtitle.test/bundle", bytes));
    let provider = Arc::new(Provider::default());
    let denied = ResourceSession::new(keys(provider.clone()), ResourceOptions::default()).unwrap();
    assert_eq!(
        denied
            .read(source.clone(), request.clone())
            .await
            .unwrap_err()
            .kind(),
        ResourceErrorKind::UnconfirmedRange
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    let bounded = ResourceSession::new(
        keys(provider.clone()),
        ResourceOptions::default()
            .with_encrypted_ranges(EncryptedRangePolicy::CompleteResources)
            .with_limits(16, 16, 1),
    )
    .unwrap();
    assert_eq!(
        bounded
            .read(source.clone(), request.clone())
            .await
            .unwrap_err()
            .kind(),
        ResourceErrorKind::ResourceTooLarge
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    let complete = ResourceSession::new(
        keys(provider),
        ResourceOptions::default().with_encrypted_ranges(EncryptedRangePolicy::CompleteResources),
    )
    .unwrap();
    assert_eq!(complete.read(source, request).await.unwrap().bytes(), CLEAR);
    let sink = Arc::new(Collector::default());
    let s = session(
        false,
        ContinuousOptions::default()
            .with_range(PresentationRange::new(time(1000), time(3000)).unwrap()),
    )
    .with_subtitle_sink(sink.clone());
    feed(&s);
    s.into_bytes(2 * 1024 * 1024, OutputFormat::FragmentedMp4)
        .await
        .unwrap();
    let cues = sink.cues.lock().unwrap();
    for cue in cues.iter().filter(|c| {
        matches!(
            c.disposition(),
            SubtitleDisposition::Written | SubtitleDisposition::Clipped
        )
    }) {
        assert!(cue.start().ticks() >= 0);
        assert!(cue.end().ticks() <= 180000);
    }
    serde_json::json!({"completeResourceRanges":true,"budgetBeforeProvider":true,"presentationRange":true})
}

/// Independent minimal ISO-BMFF reader: compare the committed sidecar union to
/// actual tfdt/trun presentation intervals and vttc/payl payloads in output bytes.
pub fn embedded_intervals(bytes: &[u8]) -> Vec<(String, i128, i128)> {
    fn u32be(b: &[u8]) -> u32 {
        u32::from_be_bytes(b[..4].try_into().unwrap())
    }
    fn boxes(bytes: &[u8]) -> Vec<(&[u8], usize, &[u8])> {
        let mut result = Vec::new();
        let mut offset = 0;
        while offset < bytes.len() {
            let size = u32be(&bytes[offset..]) as usize;
            assert!(size >= 8 && offset + size <= bytes.len());
            result.push((
                &bytes[offset + 4..offset + 8],
                offset,
                &bytes[offset + 8..offset + size],
            ));
            offset += size;
        }
        result
    }
    let mut result = Vec::new();
    for (kind, moof_offset, moof) in boxes(bytes) {
        if kind != b"moof" {
            continue;
        }
        for (kind, _, traf) in boxes(moof) {
            if kind != b"traf" {
                continue;
            }
            let children = boxes(traf);
            let field = |kind: &[u8]| children.iter().find(|(k, _, _)| *k == kind).unwrap().2;
            if u32be(&field(b"tfhd")[4..]) != 65 {
                continue;
            }
            let tfdt = field(b"tfdt");
            assert_eq!(tfdt[0], 1);
            let mut dts = i128::from(u64::from_be_bytes(tfdt[4..12].try_into().unwrap()));
            let trun = field(b"trun");
            assert_eq!(u32be(trun), 0x01000f01);
            let count = u32be(&trun[4..]) as usize;
            let mut data = moof_offset + u32be(&trun[8..]) as usize;
            for entry in trun[12..].as_chunks::<16>().0.iter().take(count) {
                let duration = i128::from(u32be(entry));
                let size = u32be(&entry[4..]) as usize;
                let start = dts + i128::from(u32be(&entry[12..]) as i32);
                for (kind, _, cue) in boxes(&bytes[data..data + size]) {
                    if kind == b"vttc" {
                        let payload = boxes(cue).iter().find(|(k, _, _)| *k == b"payl").unwrap().2;
                        result.push((
                            std::str::from_utf8(payload).unwrap().into(),
                            start,
                            start + duration,
                        ));
                    }
                }
                dts += duration;
                data += size;
            }
        }
    }
    result
}
pub fn interval_union(mut intervals: Vec<(String, i128, i128)>) -> Vec<(String, i128, i128)> {
    intervals.sort();
    let mut result: Vec<(String, i128, i128)> = Vec::new();
    for (body, start, end) in intervals {
        if let Some(last) = result.last_mut()
            && last.0 == body
            && last.2 == start
        {
            last.2 = end;
        } else {
            result.push((body, start, end));
        }
    }
    result
}
struct CounterSink {
    count: AtomicUsize,
    last: Mutex<Option<(u64, i128)>>,
}
impl SubtitleSink for CounterSink {
    fn commit<'a>(&'a self, batch: &'a SubtitleCommit) -> SubtitleSinkFuture<'a> {
        Box::pin(async move {
            assert!(batch.cues().len() <= 2);
            for cue in batch.cues() {
                assert_eq!(cue.disposition(), SubtitleDisposition::Written);
                let mut last = self.last.lock().unwrap();
                if let Some((receipt, end)) = *last {
                    assert!(cue.receipt() > receipt);
                    assert!(cue.start().ticks() >= end);
                }
                *last = Some((cue.receipt(), cue.end().ticks()));
                self.count.fetch_add(1, Ordering::SeqCst);
            }
            Ok(())
        })
    }
    fn finish(&self) -> SubtitleSinkFuture<'_> {
        Box::pin(async { Ok(()) })
    }
}
pub async fn long_open() -> serde_json::Value {
    use std::{
        future::Future,
        task::{Context, Waker},
    };
    let sink = Arc::new(CounterSink {
        count: AtomicUsize::new(0),
        last: Mutex::new(None),
    });
    let s = session(
        false,
        ContinuousOptions::default()
            .with_mode(ContinuousMode::Open)
            .with_limits(
                ContinuousLimits::default()
                    .with_history(4)
                    .with_queue(4, 1024 * 1024)
                    .with_samples(1024, 1024 * 1024),
            ),
    )
    .with_subtitle_sink(sink.clone());
    let h = s.handle();
    let track = h.subtitle_track_id(&id("cc")).unwrap();
    let mut writer = tokio::io::sink();
    let mut running = Box::pin(s.write_to(&mut writer));
    let mut cx = Context::from_waker(Waker::noop());
    for n in 0u64..300 {
        let p = parse_playlist_snapshot(&TextResource {
            location:SourceLocation::Url(url::Url::parse("https://subtitle.test/input.m3u8").unwrap()),
            content:format!("#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXT-X-MEDIA-SEQUENCE:{first}\n#EXT-X-DISCONTINUITY-SEQUENCE:{first}\n#EXT-X-MAP:URI=\"init\"\n#EXTINF:2,\nseg0\n{tail}", first=n.saturating_sub(1), tail=if n>0 { "#EXT-X-DISCONTINUITY\n#EXTINF:2,\nseg0\n" } else { "" }),
        },PlaylistContext::new(id("media"),7).with_revision(n)).unwrap();
        h.accept_snapshot(&id("media"), &p).unwrap();
        let receipt = h
            .accept_cues(
                track,
                &[SubtitleCue::new(7, n, time(0), time(500), "repeat")
                    .with_identifier("duplicate")],
            )
            .unwrap()
            .first_receipt();
        assert_eq!(receipt, Some(n));
        assert!(running.as_mut().poll(&mut cx).is_pending());
        tokio::task::yield_now().await;
    }
    assert!(
        sink.count.load(Ordering::SeqCst) > 290,
        "cues must be delivered while input remains open"
    );
    h.end_input(&id("media")).unwrap();
    h.end_subtitles(track).unwrap();
    let report = running.await.unwrap();
    assert_eq!(sink.count.load(Ordering::SeqCst), 300);
    assert!(report.subtitle_history_truncated());
    assert!(report.subtitle_reports().len() <= 4);
    serde_json::json!({"windows":300,"boundedHistory":4,"beforeEof":true})
}

pub async fn gaps() -> serde_json::Value {
    for policy in [GapPolicy::Preserve, GapPolicy::Collapse] {
        let sink = Arc::new(Collector::default());
        let s = session(
            true,
            ContinuousOptions::default()
                .with_gap_policy(policy)
                .with_missing_segments(MissingSegmentPolicy::Skip),
        )
        .with_subtitle_sink(sink.clone());
        let h = s.handle();
        h.accept_snapshot(&id("media"), &snapshot("media","#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXT-X-MAP:URI=\"init\"\n#EXTINF:2.005333,\nseg0\n#EXT-X-GAP\n#EXTINF:2.005333,\nmissing\n#EXTINF:2.005333,\nseg2\n#EXT-X-ENDLIST\n")).unwrap();
        let cc = h.subtitle_track_id(&id("cc")).unwrap();
        h.accept_cues(cc, &[SubtitleCue::new(7, 0, time(0), time(8000), "gap")])
            .unwrap();
        h.end_subtitles(cc).unwrap();
        let (bytes, _) = s
            .into_bytes(2 * 1024 * 1024, OutputFormat::FragmentedMp4)
            .await
            .unwrap();
        let cues = sink.cues.lock().unwrap();
        let intervals = cues
            .iter()
            .map(|c| {
                (
                    c.cue().payload().to_string(),
                    c.start().ticks(),
                    c.end().ticks(),
                )
            })
            .collect();
        assert_eq!(
            interval_union(embedded_intervals(&bytes)),
            interval_union(intervals)
        );
    }
    serde_json::json!({"preserve":true,"collapse":true,"embeddedIntervals":true})
}
