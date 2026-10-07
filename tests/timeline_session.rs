use hls_engine::legacy::{crypto::resource::*, *};
use std::sync::Arc;
#[allow(dead_code)]
#[path = "support/keyed_corpus.rs"]
mod suite;
fn options() -> TimelinePrepareOptions {
    TimelinePrepareOptions::default().with_resources(
        ResourceOptions::default().with_encrypted_ranges(EncryptedRangePolicy::CompleteResources),
    )
}
fn range(start: i128, end: i128) -> PresentationRange {
    PresentationRange::new(
        MediaTime::new(start, 1000).unwrap(),
        MediaTime::new(end, 1000).unwrap(),
    )
    .unwrap()
}
#[tokio::test]
async fn clear_encrypted_and_legacy_full_outputs_agree() {
    for name in [
        "ts_avc_regular",
        "ts_hevc_regular",
        "fmp4_avc_regular",
        "fmp4_hevc_regular",
        "ts_aac_audio_only",
        "fmp4_aac_audio_only",
    ] {
        for encrypted in [false, true] {
            let (clear, inputs) = suite::pair(name, None, [encrypted, false], false);
            let (_, old) = prepare_hls(clear, PrepareOptions::default())
                .await
                .unwrap()
                .into_mp4_bytes()
                .await
                .unwrap();
            let (_, new) = prepare_hls_timeline(
                inputs,
                suite::keys(Arc::new(suite::corpus::Provider)),
                options(),
            )
            .await
            .unwrap()
            .into_mp4_bytes()
            .await
            .unwrap();
            assert_eq!(
                new.outputs()[0]
                    .media()
                    .tracks
                    .iter()
                    .map(|t| t.sample_count)
                    .collect::<Vec<_>>(),
                old.media()
                    .tracks
                    .iter()
                    .map(|t| t.sample_count)
                    .collect::<Vec<_>>(),
                "{name}/{encrypted}"
            );
        }
    }
}
#[tokio::test]
async fn ranges_expand_to_decodable_gops_and_clip_eof() {
    for name in [
        "ts_avc_regular",
        "fmp4_avc_regular",
        "ts_hevc_regular",
        "fmp4_hevc_regular",
    ] {
        let (_, inputs) = suite::pair(name, None, [true, false], false);
        let (bytes, report) = prepare_hls_timeline(
            inputs,
            suite::keys(Arc::new(suite::corpus::Provider)),
            options().with_range(range(200, 1200)),
        )
        .await
        .unwrap()
        .into_mp4_bytes()
        .await
        .unwrap();
        assert!(!bytes.is_empty());
        assert_eq!(report.requested_range(), Some(range(200, 1200)));
        assert!(!report.outputs()[0].mappings().is_empty());
    }
}
#[tokio::test]
async fn dual_mixed_containers_preserve_all_tracks() {
    for (primary, audio) in [
        ("ts_avc_regular", "fmp4_aac_audio_only"),
        ("fmp4_avc_regular", "ts_aac_audio_only"),
    ] {
        let (_, inputs) = suite::pair(primary, Some(audio), [true, true], false);
        let mut writer = Vec::new();
        let report = prepare_hls_timeline(
            inputs,
            suite::keys(Arc::new(suite::corpus::Provider)),
            options(),
        )
        .await
        .unwrap()
        .write_to(&mut writer)
        .await
        .unwrap();
        assert_eq!(report.outputs()[0].media().tracks.len(), 2);
    }
}
#[test]
fn invalid_ranges_fail_before_execution() {
    for (a, b) in [(1, 1), (2, 1), (-1, 1)] {
        assert_eq!(
            PresentationRange::new(MediaTime::new(a, 1).unwrap(), MediaTime::new(b, 1).unwrap())
                .unwrap_err()
                .kind(),
            TimelineErrorKind::InvalidRange
        );
    }
}

fn snapshot_input(text: &str, segments: &[(&str, &[u8])]) -> KeyedInputs {
    let base = "https://timeline.test/";
    let mut source = MemorySource::new();
    for (name, bytes) in segments {
        source = source.segment(format!("{base}{name}"), bytes.to_vec());
    }
    source_input(text, Arc::new(source))
}
fn source_input(text: &str, source: Arc<dyn Source>) -> KeyedInputs {
    use hls_engine::legacy::playlist::*;
    let snapshot = parse_playlist_snapshot(
        &TextResource {
            location: SourceLocation::Url(
                url::Url::parse("https://timeline.test/index.m3u8").unwrap(),
            ),
            content: text.into(),
        },
        PlaylistContext::new(InputId::new("primary").unwrap(), 1),
    )
    .unwrap();
    KeyedInputs::new(KeyedInput::new(snapshot, source))
}
fn gap_input() -> KeyedInputs {
    snapshot_input(
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"init.fmp4\"\n#EXTINF:2,\nseg0.m4s\n#EXT-X-GAP\n#EXTINF:2,\nmissing.m4s\n#EXTINF:2,\nseg2.m4s\n#EXT-X-ENDLIST\n",
        &[
            (
                "init.fmp4",
                include_bytes!("fixtures/media/fmp4_avc_video_only/init.fmp4"),
            ),
            (
                "seg0.m4s",
                include_bytes!("fixtures/media/fmp4_avc_video_only/seg0.m4s"),
            ),
            (
                "seg2.m4s",
                include_bytes!("fixtures/media/fmp4_avc_video_only/seg2.m4s"),
            ),
        ],
    )
}
#[tokio::test]
async fn gap_preserve_collapse_and_classic_split() {
    let keys = || suite::keys(Arc::new(suite::corpus::Provider));
    let report = prepare_hls_timeline(gap_input(), keys(), options())
        .await
        .unwrap()
        .write_to(&mut Vec::new())
        .await
        .unwrap();
    assert!(!report.gaps().is_empty());
    let error = prepare_hls_timeline(gap_input(), keys(), options())
        .await
        .unwrap()
        .into_mp4_bytes()
        .await
        .unwrap_err();
    assert_eq!(error.kind(), TimelineErrorKind::UnrepresentableGap);
    let (_, collapsed) = prepare_hls_timeline(
        gap_input(),
        keys(),
        options().with_gap_policy(GapPolicy::Collapse),
    )
    .await
    .unwrap()
    .into_mp4_bytes()
    .await
    .unwrap();
    assert_eq!(collapsed.outputs().len(), 1);
    let (parts, split) = prepare_hls_timeline(
        gap_input(),
        keys(),
        options().with_change_policy(TimelineChangePolicy::Split),
    )
    .await
    .unwrap()
    .into_mp4_outputs()
    .await
    .unwrap();
    assert_eq!(parts.len(), 2);
    assert_eq!(split.outputs()[1].reason(), TimelineSplitReason::Gap);
}
#[tokio::test]
async fn timestamp_reset_creates_a_second_epoch() {
    let input = snapshot_input(
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\na.ts\n#EXT-X-DISCONTINUITY\n#EXTINF:2,\nb.ts\n#EXT-X-ENDLIST\n",
        &[
            (
                "a.ts",
                include_bytes!("fixtures/media/ts_avc_regular/seg0.ts"),
            ),
            (
                "b.ts",
                include_bytes!("fixtures/media/ts_avc_regular/seg0.ts"),
            ),
        ],
    );
    let report = prepare_hls_timeline(
        input,
        suite::keys(Arc::new(suite::corpus::Provider)),
        options(),
    )
    .await
    .unwrap()
    .write_to(&mut Vec::new())
    .await
    .unwrap();
    assert!(
        report.outputs()[0]
            .mappings()
            .iter()
            .any(|m| m.epoch() == 1)
    );
}

#[tokio::test]
async fn range_does_not_read_unneeded_suffix() {
    let input = snapshot_input(
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\na.ts\n#EXTINF:2,\nb.ts\n#EXTINF:2,\nunavailable.ts\n#EXT-X-ENDLIST\n",
        &[
            (
                "a.ts",
                include_bytes!("fixtures/media/ts_avc_regular/seg0.ts"),
            ),
            (
                "b.ts",
                include_bytes!("fixtures/media/ts_avc_regular/seg1.ts"),
            ),
        ],
    );
    let (_, report) = prepare_hls_timeline(
        input,
        suite::keys(Arc::new(suite::corpus::Provider)),
        options().with_range(range(100, 400)),
    )
    .await
    .unwrap()
    .into_mp4_bytes()
    .await
    .unwrap();
    assert!(report.preroll().unwrap().is_some());
}
#[cfg(feature = "serde")]
#[test]
fn timeline_wire_never_rounds_i128_through_js_numbers() {
    let time = MediaTime::new(i128::MAX - 17, 90_000).unwrap();
    let value = serde_json::to_value(time).unwrap();
    assert!(value["ticks"].is_string());
    assert_eq!(serde_json::from_value::<MediaTime>(value).unwrap(), time);
    assert!(
        serde_json::from_str::<MediaTime>(r#"{"ticks":9007199254740993,"timescale":1}"#).is_err()
    );
    assert!(serde_json::from_str::<MediaTime>(r#"{"ticks":"1","timescale":0}"#).is_err());
}
#[tokio::test]
async fn output_failure_does_not_emit_completion() {
    use std::{
        pin::Pin,
        sync::Mutex,
        task::{Context, Poll},
    };
    struct Broken;
    impl tokio::io::AsyncWrite for Broken {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Poll::Ready(Ok(bytes.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Err(std::io::Error::other("test flush failure")))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            panic!("caller owns shutdown")
        }
    }
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed = events.clone();
    let (_, inputs) = suite::pair("ts_avc_regular", None, [true, false], false);
    let error = prepare_hls_timeline(
        inputs,
        suite::keys(Arc::new(suite::corpus::Provider)),
        options().with_on_event(Arc::new(move |e| observed.lock().unwrap().push(e.kind()))),
    )
    .await
    .unwrap()
    .write_to(&mut Broken)
    .await
    .unwrap_err();
    assert_eq!(
        error.kind(),
        TimelineErrorKind::Output,
        "{:?}",
        error.raw_cause()
    );
    assert!(events.lock().unwrap().is_empty());
}
#[test]
fn old_capability_does_not_inherit_new_timeline_support() {
    use hls_engine::legacy::capabilities::*;
    let input = KeyedInputCapability::new(
        KeyedContainer::TransportStream,
        KeyedEncryption::Clear,
        vec![KeyedCodec::Avc],
    );
    let query = KeyedCapabilityQuery::new(input, KeyedOutput::FragmentedWriter)
        .with_range(KeyedRange::PresentationRange)
        .with_timeline_changes(true);
    assert!(!query_keyed_capability(&query).supported());
    assert!(query_timeline_capability(&TimelineCapabilityQuery::new(query)).supported());
}

#[tokio::test]
async fn configuration_change_requires_explicit_decodable_split() {
    let input = || {
        snapshot_input(
            "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\na.ts\n#EXT-X-DISCONTINUITY\n#EXTINF:2,\nb.ts\n#EXT-X-ENDLIST\n",
            &[
                (
                    "a.ts",
                    include_bytes!("fixtures/media/ts_avc_regular/seg0.ts"),
                ),
                (
                    "b.ts",
                    include_bytes!("fixtures/media/ts_hevc_regular/seg0.ts"),
                ),
            ],
        )
    };
    let keys = || suite::keys(Arc::new(suite::corpus::Provider));
    let error = prepare_hls_timeline(input(), keys(), options())
        .await
        .unwrap()
        .into_mp4_bytes()
        .await
        .unwrap_err();
    assert_eq!(error.kind(), TimelineErrorKind::ConfigurationChanged);
    let (outputs, report) = prepare_hls_timeline(
        input(),
        keys(),
        options().with_change_policy(TimelineChangePolicy::Split),
    )
    .await
    .unwrap()
    .into_mp4_outputs()
    .await
    .unwrap();
    assert_eq!(outputs.len(), 2);
    assert_eq!(
        report.outputs()[1].reason(),
        TimelineSplitReason::ConfigurationChanged
    );
}
#[tokio::test]
async fn split_writer_acquisition_is_async_and_caller_owns_shutdown() {
    use std::{
        cell::RefCell,
        future::Future,
        pin::Pin,
        rc::Rc,
        task::{Context, Poll},
    };
    struct Writer(Rc<RefCell<Vec<u8>>>);
    impl tokio::io::AsyncWrite for Writer {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            self.0.borrow_mut().extend_from_slice(bytes);
            Poll::Ready(Ok(bytes.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            panic!("caller owns shutdown")
        }
    }
    struct Provider(Vec<Rc<RefCell<Vec<u8>>>>);
    impl TimelineWriterProvider for Provider {
        type Writer = Writer;
        fn acquire<'a>(
            &'a mut self,
            request: TimelineOutputRequest,
        ) -> Pin<Box<dyn Future<Output = TimelineResult<Writer>> + 'a>> {
            Box::pin(async move {
                tokio::task::yield_now().await;
                assert_eq!(request.index(), self.0.len());
                let bytes = Rc::new(RefCell::new(Vec::new()));
                self.0.push(bytes.clone());
                Ok(Writer(bytes))
            })
        }
    }
    let mut provider = Provider(Vec::new());
    let report = prepare_hls_timeline(
        gap_input(),
        suite::keys(Arc::new(suite::corpus::Provider)),
        options().with_change_policy(TimelineChangePolicy::Split),
    )
    .await
    .unwrap()
    .write_to_outputs(&mut provider)
    .await
    .unwrap();
    assert_eq!(provider.0.len(), report.outputs().len());
    assert!(!provider.0[0].borrow().is_empty());
}

#[tokio::test]
async fn pdt_reset_uses_wall_clock_anchor() {
    let input = snapshot_input(
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-PROGRAM-DATE-TIME:2026-10-05T10:00:00.123+08:00\n#EXTINF:2,\na.ts\n#EXT-X-DISCONTINUITY\n#EXT-X-PROGRAM-DATE-TIME:2026-10-05T02:00:03.123Z\n#EXTINF:2,\nb.ts\n#EXT-X-ENDLIST\n",
        &[
            (
                "a.ts",
                include_bytes!("fixtures/media/ts_avc_video_only/seg0.ts"),
            ),
            (
                "b.ts",
                include_bytes!("fixtures/media/ts_avc_video_only/seg0.ts"),
            ),
        ],
    );
    let report = prepare_hls_timeline(
        input,
        suite::keys(Arc::new(suite::corpus::Provider)),
        options(),
    )
    .await
    .unwrap()
    .write_to(&mut Vec::new())
    .await
    .unwrap();
    assert_eq!(report.gaps().len(), 1);
    assert!(
        report.outputs()[0]
            .mappings()
            .iter()
            .any(|m| m.epoch() == 1 && m.program_date_time().is_some())
    );
}

fn three_configurations() -> KeyedInputs {
    snapshot_input(
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\na.ts\n#EXT-X-DISCONTINUITY\n#EXTINF:2,\nb.ts\n#EXT-X-DISCONTINUITY\n#EXTINF:2,\nc.ts\n#EXT-X-ENDLIST\n",
        &[
            (
                "a.ts",
                include_bytes!("fixtures/media/ts_avc_video_only/seg0.ts"),
            ),
            (
                "b.ts",
                include_bytes!("fixtures/media/ts_hevc_regular/seg0.ts"),
            ),
            (
                "c.ts",
                include_bytes!("fixtures/media/ts_avc_video_only/seg0.ts"),
            ),
        ],
    )
}

#[derive(Debug)]
struct Cancel(tokio::sync::watch::Sender<bool>);
impl Cancel {
    fn new() -> Self {
        Self(tokio::sync::watch::channel(false).0)
    }
    fn cancel(&self) {
        self.0.send_replace(true);
    }
}
impl CancelToken for Cancel {
    fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }
    fn cancelled(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        let mut rx = self.0.subscribe();
        Box::pin(async move {
            rx.wait_for(|v| *v).await.unwrap();
        })
    }
}

#[tokio::test]
async fn cancellation_at_second_completion_keeps_both_completed_reports() {
    use std::sync::Mutex;
    let token = Arc::new(Cancel::new());
    let signal = token.clone();
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed = events.clone();
    let error = prepare_hls_timeline(
        three_configurations(),
        suite::keys(Arc::new(suite::corpus::Provider)),
        options()
            .with_change_policy(TimelineChangePolicy::Split)
            .with_cancel(token)
            .with_on_event(Arc::new(move |event| {
                observed
                    .lock()
                    .unwrap()
                    .push((event.kind(), event.output_index()));
                if event.kind() == TimelineEventKind::OutputCompleted && event.output_index() == 1 {
                    signal.cancel();
                }
            })),
    )
    .await
    .unwrap()
    .into_mp4_outputs()
    .await
    .unwrap_err();
    assert_eq!(
        error.kind(),
        TimelineErrorKind::Cancelled,
        "{:?}",
        error.raw_cause()
    );
    assert_eq!(
        error
            .completed_outputs()
            .iter()
            .map(|r| r.index())
            .collect::<Vec<_>>(),
        [0, 1]
    );
    assert_eq!(
        events.lock().unwrap().last(),
        Some(&(TimelineEventKind::OutputCompleted, 1))
    );
}

#[tokio::test]
async fn provider_failure_keeps_only_previously_completed_outputs() {
    use std::{future::Future, pin::Pin, sync::Mutex};
    struct Provider(Arc<Mutex<Vec<(TimelineEventKind, usize)>>>);
    impl TimelineWriterProvider for Provider {
        type Writer = Vec<u8>;
        fn acquire<'a>(
            &'a mut self,
            request: TimelineOutputRequest,
        ) -> Pin<Box<dyn Future<Output = TimelineResult<Vec<u8>>> + 'a>> {
            Box::pin(async move {
                tokio::task::yield_now().await;
                if request.index() > 0 {
                    assert!(
                        !self
                            .0
                            .lock()
                            .unwrap()
                            .contains(&(TimelineEventKind::OutputCompleted, request.index() - 1))
                    );
                }
                if request.index() == 2 {
                    Err(TimelineSessionError::output(std::io::Error::other(
                        "lease unavailable",
                    )))
                } else {
                    Ok(Vec::new())
                }
            })
        }
    }
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed = events.clone();
    let error = prepare_hls_timeline(
        three_configurations(),
        suite::keys(Arc::new(suite::corpus::Provider)),
        options()
            .with_change_policy(TimelineChangePolicy::Split)
            .with_on_event(Arc::new(move |e| {
                observed.lock().unwrap().push((e.kind(), e.output_index()));
            })),
    )
    .await
    .unwrap()
    .write_to_outputs(&mut Provider(events.clone()))
    .await
    .unwrap_err();
    assert_eq!(
        error.kind(),
        TimelineErrorKind::Output,
        "{:?}",
        error.raw_cause()
    );
    assert_eq!(error.completed_outputs().len(), 1);
    assert_eq!(error.completed_outputs()[0].index(), 0);
    assert_eq!(
        events.lock().unwrap().last(),
        Some(&(TimelineEventKind::OutputCompleted, 0))
    );
}

#[tokio::test]
async fn terminal_callback_cannot_cancel_already_completed_operation() {
    let token = Arc::new(Cancel::new());
    let signal = token.clone();
    let (_, inputs) = suite::pair("ts_avc_regular", None, [false, false], false);
    let (_, report) = prepare_hls_timeline(
        inputs,
        suite::keys(Arc::new(suite::corpus::Provider)),
        options()
            .with_cancel(token)
            .with_on_event(Arc::new(move |event| {
                if event.kind() == TimelineEventKind::Completed {
                    signal.cancel();
                }
            })),
    )
    .await
    .unwrap()
    .into_mp4_bytes()
    .await
    .unwrap();
    assert_eq!(report.outputs().len(), 1);
}

#[tokio::test]
async fn all_gap_and_outside_ranges_have_distinct_errors() {
    for (request, kind) in [
        (range(2500, 3500), TimelineErrorKind::EmptyRange),
        (range(10000, 11000), TimelineErrorKind::OutOfBounds),
    ] {
        let error = prepare_hls_timeline(
            gap_input(),
            suite::keys(Arc::new(suite::corpus::Provider)),
            options().with_range(request),
        )
        .await
        .unwrap()
        .into_mp4_bytes()
        .await
        .unwrap_err();
        assert_eq!(error.kind(), kind);
    }
}

#[tokio::test]
async fn explicit_anchor_must_agree_with_pdt_and_reference_existing_epoch() {
    use hls_engine::legacy::playlist::InputId;
    let input = || {
        snapshot_input(
            "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-PROGRAM-DATE-TIME:2026-10-05T00:00:00Z\n#EXTINF:2,\na.ts\n#EXT-X-DISCONTINUITY\n#EXT-X-PROGRAM-DATE-TIME:2026-10-05T00:00:03Z\n#EXTINF:2,\nb.ts\n#EXT-X-ENDLIST\n",
            &[
                (
                    "a.ts",
                    include_bytes!("fixtures/media/ts_avc_video_only/seg0.ts"),
                ),
                (
                    "b.ts",
                    include_bytes!("fixtures/media/ts_avc_video_only/seg0.ts"),
                ),
            ],
        )
    };
    let anchor = |epoch| {
        EpochAnchor::new(
            InputId::new("primary").unwrap(),
            epoch,
            MediaTime::new(0, 1).unwrap(),
            MediaTime::new(20, 1).unwrap(),
        )
    };
    let error = prepare_hls_timeline(
        input(),
        suite::keys(Arc::new(suite::corpus::Provider)),
        options().with_epoch_anchor(anchor(1)),
    )
    .await
    .unwrap()
    .write_to(&mut Vec::new())
    .await
    .unwrap_err();
    assert_eq!(error.kind(), TimelineErrorKind::TimelineAmbiguous);
    let error = prepare_hls_timeline(
        input(),
        suite::keys(Arc::new(suite::corpus::Provider)),
        options().with_epoch_anchor(anchor(2)),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.kind(), TimelineErrorKind::InvalidOptions);
}

#[tokio::test]
async fn reread_rejects_changed_content_before_demux_or_output() {
    use std::{
        future::Future,
        pin::Pin,
        sync::atomic::{AtomicUsize, Ordering},
    };
    #[derive(Debug)]
    struct Changing(AtomicUsize);
    impl Source for Changing {
        fn read_text<'a>(
            &'a self,
            _: &'a SourceLocation,
        ) -> Pin<Box<dyn Future<Output = Result<TextResource>> + Send + 'a>> {
            panic!("snapshot already supplied")
        }
        fn read_bytes<'a>(
            &'a self,
            _: &'a SourceLocation,
            _: Option<&'a ByteRange>,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>>> + Send + 'a>> {
            Box::pin(async move {
                let mut bytes = include_bytes!("fixtures/media/ts_avc_regular/seg0.ts").to_vec();
                if self.0.fetch_add(1, Ordering::SeqCst) > 0 {
                    // Keep the TS envelope valid, but change resource identity.
                    let last = bytes.len() - 1;
                    bytes[last] ^= 1;
                }
                Ok(bytes)
            })
        }
    }
    let input = source_input(
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\na.ts\n#EXT-X-ENDLIST\n",
        Arc::new(Changing(AtomicUsize::new(0))),
    );
    let error = prepare_hls_timeline(
        input,
        suite::keys(Arc::new(suite::corpus::Provider)),
        options(),
    )
    .await
    .unwrap()
    .into_mp4_bytes()
    .await
    .unwrap_err();
    assert_eq!(error.kind(), TimelineErrorKind::ResourceChanged);
    assert!(error.completed_outputs().is_empty());
}

#[tokio::test]
async fn cue_mapping_splits_gap_and_obeys_collapse_and_output_origins() {
    use hls_engine::legacy::playlist::InputId;
    let input = InputId::new("primary").unwrap();
    let keys = || suite::keys(Arc::new(suite::corpus::Provider));
    let preserved = prepare_hls_timeline(gap_input(), keys(), options())
        .await
        .unwrap()
        .write_to(&mut Vec::new())
        .await
        .unwrap();
    let collapsed = prepare_hls_timeline(
        gap_input(),
        keys(),
        options().with_gap_policy(GapPolicy::Collapse),
    )
    .await
    .unwrap()
    .into_mp4_bytes()
    .await
    .unwrap()
    .1;
    let split = prepare_hls_timeline(
        gap_input(),
        keys(),
        options().with_change_policy(TimelineChangePolicy::Split),
    )
    .await
    .unwrap()
    .into_mp4_outputs()
    .await
    .unwrap()
    .1;
    let before = preserved
        .map_interval(&input, 1, range(1000, 5000))
        .unwrap();
    let after = collapsed
        .map_interval(&input, 1, range(1000, 5000))
        .unwrap();
    let separate = split.map_interval(&input, 1, range(1000, 5000)).unwrap();
    assert_eq!(before.len(), 2);
    assert_eq!(after.len(), 2);
    assert_eq!(separate.len(), 2);
    let millis = |time: MediaTime| time.ticks() * 1000 / i128::from(time.timescale());
    assert_eq!(
        millis(before[1].output_range().start()) - millis(after[1].output_range().start()),
        2000
    );
    assert_eq!(
        millis(after[0].output_range().end()),
        millis(after[1].output_range().start())
    );
    assert_eq!(separate[1].output_index(), 1);
    // Local zero is the earliest DTS. The cue retains the video's CTS offset.
    assert_eq!(
        separate[1].output_range().start(),
        split.outputs()[1].mappings()[0].output_start()
    );
}

#[cfg(not(target_arch = "wasm32"))]
#[tokio::test]
async fn file_provider_alias_cannot_replace_a_completed_output() {
    use std::{future::Future, path::PathBuf, pin::Pin};
    struct Directory(PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    struct Provider(PathBuf);
    impl TimelineFileProvider for Provider {
        fn acquire<'a>(
            &'a mut self,
            request: TimelineOutputRequest,
        ) -> Pin<Box<dyn Future<Output = TimelineResult<PathBuf>> + 'a>> {
            Box::pin(async move {
                Ok(if request.index() == 0 {
                    self.0.join("output.mp4")
                } else {
                    self.0.join(".").join("output.mp4")
                })
            })
        }
    }
    let directory = Directory(std::env::temp_dir().join(format!(
            "hls-timeline-alias-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )));
    std::fs::create_dir(&directory.0).unwrap();
    let error = prepare_hls_timeline(
        three_configurations(),
        suite::keys(Arc::new(suite::corpus::Provider)),
        options().with_change_policy(TimelineChangePolicy::Split),
    )
    .await
    .unwrap()
    .write_to_files(
        &mut Provider(directory.0.clone()),
        FileOutputOptions::default(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), TimelineErrorKind::InvalidOptions);
    assert_eq!(error.completed_outputs().len(), 1);
    assert_eq!(
        std::fs::metadata(directory.0.join("output.mp4"))
            .unwrap()
            .len(),
        error.completed_outputs()[0].media().bytes_written
    );
    assert_eq!(
        std::fs::read_dir(&directory.0).unwrap().count(),
        1,
        "temporary outputs must be cleaned up"
    );
}

use budget::clocks;

#[tokio::test]
async fn ts_gap_preserves_frame_durations_and_collapse_removes_only_missing_time() {
    let fixture = include_bytes!("fixtures/media/ts_avc_video_only/seg0.ts");
    let later = clocks::shift_ts(fixture.to_vec(), 4 * 90_000);
    let text = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\na.ts\n#EXT-X-GAP\n#EXTINF:2,\nmissing.ts\n#EXTINF:2,\nb.ts\n#EXT-X-ENDLIST\n";
    for policy in [GapPolicy::Preserve, GapPolicy::Collapse] {
        let mut writer = Vec::new();
        let report = prepare_hls_timeline(
            snapshot_input(text, &[("a.ts", fixture), ("b.ts", &later)]),
            suite::keys(Arc::new(suite::corpus::Provider)),
            options().with_gap_policy(policy),
        )
        .await
        .unwrap()
        .write_to(&mut writer)
        .await
        .unwrap();
        assert_eq!(report.outputs()[0].media().tracks[0].sample_count, 120);
        assert_eq!(report.gaps().len(), 1);
        let duration = report.outputs()[0].media().duration;
        assert_eq!(
            duration,
            if policy == GapPolicy::Preserve {
                6066
            } else {
                4066
            }
        );
    }
}

#[tokio::test]
async fn timeline_wrap_matches_unwrapped_media_and_half_period_is_rejected() {
    let fixture = include_bytes!("fixtures/media/ts_avc_video_only/seg0.ts");
    let text =
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\na.ts\n#EXTINF:2,\nb.ts\n#EXT-X-ENDLIST\n";
    let mut baseline = None;
    for offset in [0, (1 << 33) - 180_000] {
        let first = clocks::shift_ts(fixture.to_vec(), offset);
        let second = clocks::shift_ts(fixture.to_vec(), offset + 180_000);
        let (mut bytes, report) = prepare_hls_timeline(
            snapshot_input(text, &[("a.ts", &first), ("b.ts", &second)]),
            suite::keys(Arc::new(suite::corpus::Provider)),
            options(),
        )
        .await
        .unwrap()
        .into_mp4_bytes()
        .await
        .unwrap();
        clocks::normalize_moov_timestamps(&mut bytes);
        assert_eq!(report.outputs()[0].media().tracks[0].sample_count, 120);
        if let Some(expected) = &baseline {
            assert_eq!(&bytes, expected);
        } else {
            baseline = Some(bytes);
        }
    }
    // Last video DTS is first + 59 * 3000. The next DTS is exactly half a wrap away.
    let second = clocks::shift_ts(fixture.to_vec(), 59 * 3000 + (1 << 32));
    let error = prepare_hls_timeline(
        snapshot_input(text, &[("a.ts", fixture), ("b.ts", &second)]),
        suite::keys(Arc::new(suite::corpus::Provider)),
        options(),
    )
    .await
    .unwrap()
    .into_mp4_bytes()
    .await
    .unwrap_err();
    assert_eq!(error.kind(), TimelineErrorKind::TimelineAmbiguous);
}

#[tokio::test]
async fn pdt_rejects_invalid_calendar_dates_and_preserves_epoch_mapping_under_rounding() {
    let fixture = include_bytes!("fixtures/media/ts_avc_video_only/seg0.ts");
    let second = clocks::shift_ts(fixture.to_vec(), 180_000);
    let mut baseline = None;
    for fraction in ["000", "001"] {
        let text = format!(
            "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-PROGRAM-DATE-TIME:2024-02-29T00:00:00Z\n#EXTINF:2,\na.ts\n#EXT-X-PROGRAM-DATE-TIME:2024-02-29T00:00:02.{fraction}Z\n#EXTINF:2,\nb.ts\n#EXT-X-ENDLIST\n"
        );
        let (mut bytes, report) = prepare_hls_timeline(
            snapshot_input(&text, &[("a.ts", fixture), ("b.ts", &second)]),
            suite::keys(Arc::new(suite::corpus::Provider)),
            options(),
        )
        .await
        .unwrap()
        .into_mp4_bytes()
        .await
        .unwrap();
        clocks::normalize_moov_timestamps(&mut bytes);
        assert!(report.gaps().is_empty());
        if let Some(expected) = &baseline {
            assert_eq!(&bytes, expected);
        } else {
            baseline = Some(bytes);
        }
    }
}

#[tokio::test]
async fn planning_budget_rejects_before_output_and_reports_live_high_water_marks() {
    let fixture = include_bytes!("fixtures/media/ts_avc_video_only/seg0.ts");
    let second = clocks::shift_ts(fixture.to_vec(), 180_000);
    let text =
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\na.ts\n#EXTINF:2,\nb.ts\n#EXT-X-ENDLIST\n";
    assert!(TimelinePlanningLimits::new(0, 1).is_err());
    assert!(TimelinePlanningLimits::new(1, 0).is_err());
    for (samples, resources) in [(59, 2), (120, 1), (120, 2)] {
        let mut writer = Vec::new();
        let result = prepare_hls_timeline(
            snapshot_input(text, &[("a.ts", fixture), ("b.ts", &second)]),
            suite::keys(Arc::new(suite::corpus::Provider)),
            options()
                .with_planning_limits(TimelinePlanningLimits::new(samples, resources).unwrap()),
        )
        .await
        .unwrap()
        .write_to(&mut writer)
        .await;
        if samples == 120 && resources == 2 {
            let report = result.unwrap();
            assert_eq!(report.peak_planned_samples(), 120);
            assert_eq!(report.peak_planned_resources(), 2);
        } else {
            assert_eq!(
                result.unwrap_err().kind(),
                TimelineErrorKind::PlanningBudgetExceeded
            );
            assert!(writer.is_empty());
        }
    }
}

#[path = "support/timeline_lifecycle.rs"]
mod lifecycle;

#[path = "support/timeline_combinations.rs"]
mod combinations;

#[path = "support/timeline_budget.rs"]
mod budget;
#[tokio::test]
async fn near_far_and_long_gop_planning_budgets_are_deterministic() {
    budget::run().await;
}

#[tokio::test]
async fn map_redeclaration_with_identical_configuration_keeps_one_output() {
    let input = snapshot_input(
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"first.mp4\"\n#EXTINF:2,\na.m4s\n#EXT-X-MAP:URI=\"second.mp4\"\n#EXTINF:2,\nb.m4s\n#EXT-X-ENDLIST\n",
        &[
            (
                "first.mp4",
                include_bytes!("fixtures/media/fmp4_avc_video_only/init.fmp4"),
            ),
            (
                "second.mp4",
                include_bytes!("fixtures/media/fmp4_avc_video_only/init.fmp4"),
            ),
            (
                "a.m4s",
                include_bytes!("fixtures/media/fmp4_avc_video_only/seg0.m4s"),
            ),
            (
                "b.m4s",
                include_bytes!("fixtures/media/fmp4_avc_video_only/seg1.m4s"),
            ),
        ],
    );
    let (_, report) = prepare_hls_timeline(
        input,
        suite::keys(Arc::new(suite::corpus::Provider)),
        options(),
    )
    .await
    .unwrap()
    .into_mp4_bytes()
    .await
    .unwrap();
    assert_eq!(report.outputs().len(), 1);
    assert_eq!(report.dependencies().len(), 2);
    assert_ne!(
        report.dependencies()[0].map_declaration(),
        report.dependencies()[1].map_declaration()
    );
    let mappings = report.outputs()[0].mappings();
    assert!(
        mappings
            .iter()
            .all(|m| m.configuration_id() == mappings[0].configuration_id())
    );
    assert_eq!(report.outputs()[0].media().tracks[0].sample_count, 120);
}

#[tokio::test]
async fn external_audio_reset_requires_cross_input_evidence() {
    use hls_engine::legacy::playlist::*;
    let primary = || {
        snapshot_input(
            "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\na.ts\n#EXT-X-DISCONTINUITY\n#EXTINF:2,\nb.ts\n#EXT-X-ENDLIST\n",
            &[
                (
                    "a.ts",
                    include_bytes!("fixtures/media/ts_avc_video_only/seg0.ts"),
                ),
                (
                    "b.ts",
                    include_bytes!("fixtures/media/ts_avc_video_only/seg0.ts"),
                ),
            ],
        )
    };
    let (_, reference) = prepare_hls_timeline(
        primary(),
        suite::keys(Arc::new(suite::corpus::Provider)),
        options(),
    )
    .await
    .unwrap()
    .into_mp4_bytes()
    .await
    .unwrap();
    let origin = reference.outputs()[0].mappings()[0].source_origin();
    for anchored in [false, true] {
        let snapshot = parse_playlist_snapshot(&TextResource {
            content: "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:999\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:2,\na.m4s\n#EXTINF:2,\nb.m4s\n#EXT-X-ENDLIST\n".into(),
            location: SourceLocation::Url("https://audio.test/media.m3u8".parse().unwrap()),
        }, PlaylistContext::new(InputId::new("audio").unwrap(), 1)).unwrap();
        let source = MemorySource::new()
            .segment(
                "https://audio.test/init.mp4",
                include_bytes!("fixtures/media/fmp4_aac_audio_only/init.fmp4").to_vec(),
            )
            .segment(
                "https://audio.test/a.m4s",
                include_bytes!("fixtures/media/fmp4_aac_audio_only/seg0.m4s").to_vec(),
            )
            .segment(
                "https://audio.test/b.m4s",
                include_bytes!("fixtures/media/fmp4_aac_audio_only/seg1.m4s").to_vec(),
            );
        let input = primary().with_audio(KeyedInput::new(snapshot, Arc::new(source)));
        let mut options = options();
        if anchored {
            options = options.with_epoch_anchor(EpochAnchor::new(
                InputId::new("primary").unwrap(),
                1,
                origin,
                MediaTime::new(4, 1).unwrap(),
            ));
        }
        let result = prepare_hls_timeline(
            input,
            suite::keys(Arc::new(suite::corpus::Provider)),
            options,
        )
        .await
        .unwrap()
        .write_to(&mut Vec::new())
        .await;
        if anchored {
            let report = result.unwrap();
            assert_eq!(report.outputs()[0].media().tracks.len(), 2);
            let mapping = report.outputs()[0]
                .mappings()
                .iter()
                .find(|m| m.input_id().as_str() == "primary" && m.epoch() == 1)
                .unwrap();
            assert_eq!(
                mapping.source_to_presentation(origin).unwrap(),
                MediaTime::new(4, 1).unwrap()
            );
        } else {
            assert_eq!(
                result.unwrap_err().kind(),
                TimelineErrorKind::TimelineAmbiguous
            );
        }
    }
}

#[tokio::test]
async fn reset_after_wrap_does_not_inherit_the_previous_epoch_wrap_cycle() {
    use hls_engine::legacy::playlist::InputId;
    let fixture = include_bytes!("fixtures/media/ts_avc_video_only/seg0.ts");
    let first = clocks::shift_ts(fixture.to_vec(), (1 << 33) - 180_000);
    let text = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\na.ts\n#EXT-X-DISCONTINUITY\n#EXTINF:2,\nb.ts\n#EXT-X-ENDLIST\n";
    let (bytes, baseline) = prepare_hls_timeline(
        snapshot_input(
            "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\na.ts\n#EXT-X-ENDLIST\n",
            &[("a.ts", fixture)],
        ),
        suite::keys(Arc::new(suite::corpus::Provider)),
        options(),
    )
    .await
    .unwrap()
    .into_mp4_bytes()
    .await
    .unwrap();
    assert!(!bytes.is_empty());
    let raw_origin = baseline.outputs()[0].mappings()[0].source_origin();
    let report = prepare_hls_timeline(
        snapshot_input(text, &[("a.ts", &first), ("b.ts", fixture)]),
        suite::keys(Arc::new(suite::corpus::Provider)),
        options().with_epoch_anchor(EpochAnchor::new(
            InputId::new("primary").unwrap(),
            1,
            raw_origin,
            MediaTime::new(3, 1).unwrap(),
        )),
    )
    .await
    .unwrap()
    .write_to(&mut Vec::new())
    .await
    .unwrap();
    let mapping = report.outputs()[0]
        .mappings()
        .iter()
        .find(|m| m.epoch() == 1)
        .unwrap();
    assert_eq!(mapping.source_origin(), raw_origin);
    assert_eq!(
        mapping.presentation_range().start(),
        MediaTime::new(3, 1).unwrap()
    );
}

#[tokio::test]
async fn removing_video_after_an_unresolved_ts_tail_returns_a_typed_error() {
    let text =
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nv.ts\n#EXTINF:2,\na.ts\n#EXT-X-ENDLIST\n";
    for changes in [TimelineChangePolicy::Fail, TimelineChangePolicy::Split] {
        let inputs = snapshot_input(
            text,
            &[
                (
                    "v.ts",
                    include_bytes!("fixtures/media/ts_avc_video_only/seg0.ts"),
                ),
                (
                    "a.ts",
                    include_bytes!("fixtures/media/ts_aac_audio_only/seg1.ts"),
                ),
            ],
        );
        let error = prepare_hls_timeline(
            inputs,
            suite::keys(Arc::new(suite::corpus::Provider)),
            options().with_change_policy(changes),
        )
        .await
        .unwrap()
        .into_mp4_outputs()
        .await
        .unwrap_err();
        assert!(matches!(
            error.kind(),
            TimelineErrorKind::ConfigurationChanged | TimelineErrorKind::NoRandomAccessPoint
        ));
    }
}
