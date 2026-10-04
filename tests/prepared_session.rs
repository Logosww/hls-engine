use hls_transmux::*;
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
};
use tokio::io::AsyncWrite;

fn input(name: &str) -> HlsInput {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/media")
        .join(name);
    let location = SourceLocation::Url(
        url::Url::parse(&format!("https://fixture.test/{name}/input.m3u8")).unwrap(),
    );
    let mut source = MemorySource::new();
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        let url = format!(
            "https://fixture.test/{name}/{}",
            path.file_name().unwrap().to_str().unwrap()
        );
        if path.extension().unwrap() == "m3u8" {
            source = source.text(url, std::fs::read_to_string(path).unwrap());
        } else {
            source = source.segment(url, std::fs::read(path).unwrap());
        }
    }
    HlsInput::custom(Arc::new(source), location)
}
fn pair(v: &str, a: &str) -> HlsInputs {
    HlsInputs::new(input(v)).with_audio(input(a))
}
#[tokio::test]
async fn all_container_pairs_and_outputs_preserve_counts_and_mapping() {
    for v in [
        "ts_avc_regular",
        "fmp4_avc_regular",
        "ts_hevc_regular",
        "fmp4_hevc_regular",
    ] {
        for a in ["ts_aac_audio_only", "fmp4_aac_audio_only"] {
            let prepared = prepare_hls(pair(v, a), PrepareOptions::default())
                .await
                .unwrap();
            assert_eq!(prepared.info().tracks().len(), 2);
            let mapping = prepared.info().timeline().clone();
            let (bytes, batch) = prepared
                .into_mp4_bytes()
                .await
                .unwrap_or_else(|e| panic!("{v}/{a}: {e}"));
            assert!(bytes.windows(4).any(|w| w == b"moov"));
            assert_eq!(batch.timeline(), &mapping);
            assert_eq!(batch.media().tracks[0].sample_count, 180);
            assert_eq!(batch.media().tracks[1].sample_count, 283);
            let mut bytes = Vec::new();
            let stream = prepare_hls(pair(v, a), PrepareOptions::default())
                .await
                .unwrap()
                .write_to(&mut bytes)
                .await
                .unwrap();
            assert_eq!(stream.timeline(), &mapping);
            assert_eq!(stream.media().tracks, batch.media().tracks);
            assert!(!bytes.windows(4).any(|w| w == b"mfra"));
            for format in [
                OutputFormat::Mp4,
                OutputFormat::FragmentedMp4,
                OutputFormat::StreamingMp4,
            ] {
                let path = std::env::temp_dir().join(format!(
                    "prepared-{}-{v}-{a}-{format:?}.mp4",
                    std::process::id()
                ));
                let file = prepare_hls(pair(v, a), PrepareOptions::default())
                    .await
                    .unwrap()
                    .write_to_file(&path, FileOutputOptions::default().with_format(format))
                    .await
                    .unwrap();
                assert_eq!(file.timeline(), &mapping);
                assert_eq!(file.media().tracks, batch.media().tracks);
                assert_eq!(
                    file.media().bytes_written,
                    std::fs::metadata(&path).unwrap().len()
                );
                std::fs::remove_file(path).unwrap();
            }
        }
    }
}
#[tokio::test]
async fn pure_tracks_work_in_prepared_and_legacy_bytes() {
    for name in [
        "ts_aac_audio_only",
        "ts_avc_video_only",
        "fmp4_aac_audio_only",
        "fmp4_avc_video_only",
    ] {
        let (_, prepared) = prepare_hls(HlsInputs::new(input(name)), PrepareOptions::default())
            .await
            .unwrap()
            .into_mp4_bytes()
            .await
            .unwrap();
        let (_, old) = transmux_hls_to_mp4_bytes(input(name), TransmuxOptions::default())
            .await
            .unwrap();
        assert_eq!(prepared.media().tracks.len(), 1);
        assert_eq!(old.tracks, prepared.media().tracks);
    }
}
#[derive(Debug, Default)]
struct Cancel(AtomicBool);
impl CancelToken for Cancel {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async {
            while !self.is_cancelled() {
                tokio::task::yield_now().await;
            }
        })
    }
}
#[derive(Default)]
struct ShortWriter {
    data: Vec<u8>,
    pending: bool,
    fail: bool,
}
impl AsyncWrite for ShortWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if self.fail {
            return Poll::Ready(Err(std::io::Error::other("injected")));
        }
        self.pending = !self.pending;
        if self.pending {
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        let n = data.len().min(17);
        self.data.extend_from_slice(&data[..n]);
        Poll::Ready(Ok(n))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
#[tokio::test]
async fn short_pending_writes_events_and_failure() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let copy = events.clone();
    let options = PrepareOptions::default()
        .with_write_mfra(true)
        .with_on_event(Arc::new(move |e| copy.lock().unwrap().push(e)));
    let mut writer = ShortWriter::default();
    let report = prepare_hls(
        pair("ts_avc_regular", "fmp4_aac_audio_only"),
        options.clone(),
    )
    .await
    .unwrap()
    .write_to(&mut writer)
    .await
    .unwrap();
    assert_eq!(writer.data.len() as u64, report.media().bytes_written);
    assert!(writer.data.windows(4).any(|w| w == b"mfra"));
    let event = events.lock().unwrap().last().unwrap().clone();
    assert_eq!(event.phase(), SessionPhase::Completed);
    assert_eq!(event.processed_segments(), event.total_segments());
    events.lock().unwrap().clear();
    writer.fail = true;
    let err = prepare_hls(pair("ts_avc_regular", "fmp4_aac_audio_only"), options)
        .await
        .unwrap()
        .write_to(&mut writer)
        .await
        .unwrap_err();
    assert_eq!(err.phase(), SessionPhase::Writing);
    assert!(matches!(err.error(), Error::Io(_)));
    assert!(
        !events
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.phase() == SessionPhase::Completed)
    );
}
#[tokio::test]
async fn cancelled_preparation_and_invalid_budget() {
    let token = Arc::new(Cancel(AtomicBool::new(true)));
    let result = prepare_hls(
        HlsInputs::new(input("ts_avc_regular")),
        PrepareOptions::default().with_cancel(token),
    )
    .await;
    assert!(matches!(result.err().unwrap().error(), Error::Cancelled));
    let result = prepare_hls(
        HlsInputs::new(input("ts_avc_regular")),
        PrepareOptions::default()
            .with_budget(ResourceBudget::default().with_max_in_flight_reads(0)),
    )
    .await;
    assert!(matches!(
        result.err().unwrap().error(),
        Error::InvalidInput(_)
    ));
}
#[tokio::test]
async fn mapping_and_wrong_selected_track() {
    let session = prepare_hls(
        pair("ts_avc_regular", "ts_aac_audio_only"),
        PrepareOptions::default(),
    )
    .await
    .unwrap();
    let mapping = session.info().timeline();
    assert_eq!(mapping.to_output(mapping.origin(), 1_000).unwrap(), 0);
    let anchor = mapping.tracks()[0].wrap_anchor().unwrap();
    assert_eq!(
        mapping
            .unwrap_mpegts((anchor as u64) % (1 << 33), anchor)
            .unwrap()
            .ticks(),
        anchor
    );
    let error = prepare_hls(
        pair("ts_avc_regular", "ts_avc_video_only"),
        PrepareOptions::default(),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.role(), Some(InputRole::Audio));
}

#[derive(Debug, Default)]
struct Metrics {
    reads: std::sync::atomic::AtomicUsize,
    active: std::sync::atomic::AtomicUsize,
    peak: std::sync::atomic::AtomicUsize,
    stops: std::sync::atomic::AtomicUsize,
}
struct ReadGuard(Arc<Metrics>);
impl Drop for ReadGuard {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}
#[derive(Debug)]
struct Observed {
    source: Arc<dyn Source>,
    metrics: Arc<Metrics>,
    hang: bool,
    fail: bool,
}
impl Source for Observed {
    fn create_session_with_options(
        &self,
        options: &SourceSessionOptions,
    ) -> Option<Arc<dyn Source>> {
        assert!(options.demand_driven());
        None
    }
    fn stop_session(&self) {
        self.metrics.stops.fetch_add(1, Ordering::SeqCst);
    }
    fn read_text<'a>(
        &'a self,
        location: &'a SourceLocation,
    ) -> Pin<Box<dyn Future<Output = Result<TextResource>> + Send + 'a>> {
        self.source.read_text(location)
    }
    fn read_bytes<'a>(
        &'a self,
        location: &'a SourceLocation,
        range: Option<&'a ByteRange>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>>> + Send + 'a>> {
        Box::pin(async move {
            self.metrics.reads.fetch_add(1, Ordering::SeqCst);
            let active = self.metrics.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.metrics.peak.fetch_max(active, Ordering::SeqCst);
            let _guard = ReadGuard(self.metrics.clone());
            tokio::task::yield_now().await;
            if self.fail {
                return Err(Error::Http("injected read failure".into()));
            }
            if self.hang {
                return std::future::pending().await;
            }
            self.source.read_bytes(location, range).await
        })
    }
}
fn observed(name: &str, metrics: Arc<Metrics>, hang: bool, fail: bool) -> HlsInput {
    let HlsInput::Custom(source, location) = input(name) else {
        unreachable!()
    };
    HlsInput::custom(
        Arc::new(Observed {
            source,
            metrics,
            hang,
            fail,
        }),
        location,
    )
}
struct BlockedWriter;
impl AsyncWrite for BlockedWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Poll::Pending
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Pending
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        panic!("caller owns close")
    }
}
#[tokio::test]
async fn blocked_writer_never_advances_reads_and_cancel_drops_both_sessions() {
    let metrics = Arc::new(Metrics::default());
    let cancel = Arc::new(Cancel::default());
    let pair = HlsInputs::new(observed("ts_avc_regular", metrics.clone(), false, false))
        .with_audio(observed("ts_aac_audio_only", metrics.clone(), false, false));
    let prepared = prepare_hls(pair, PrepareOptions::default().with_cancel(cancel.clone()))
        .await
        .unwrap();
    let reads = metrics.reads.load(Ordering::SeqCst);
    assert_eq!(reads, 2); // Probe resources are retained, no lookahead until execution.
    assert_eq!(metrics.peak.load(Ordering::SeqCst), 2);
    let mut writer = BlockedWriter;
    let mut future = Box::pin(prepared.write_to(&mut writer));
    for _ in 0..50 {
        assert!(
            std::future::poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        assert_eq!(metrics.reads.load(Ordering::SeqCst), reads);
    }
    cancel.0.store(true, Ordering::SeqCst);
    assert!(matches!(
        future.await.unwrap_err().error(),
        Error::Cancelled
    ));
    assert_eq!(metrics.active.load(Ordering::SeqCst), 0);
    assert_eq!(metrics.stops.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn input_failure_drops_other_pending_read_and_preserves_role() {
    let metrics = Arc::new(Metrics::default());
    let pair = HlsInputs::new(observed("ts_avc_regular", metrics.clone(), true, false))
        .with_audio(observed("ts_aac_audio_only", metrics.clone(), false, true));
    let error = prepare_hls(pair, PrepareOptions::default())
        .await
        .err()
        .unwrap();
    assert!(matches!(error.error(), Error::Http(_)));
    assert_eq!(error.role(), Some(InputRole::Audio));
    assert_eq!(error.segment_index(), Some(0));
    assert!(error.resource().unwrap().ends_with("seg0.ts"));
    assert_eq!(metrics.active.load(Ordering::SeqCst), 0);
    assert_eq!(metrics.stops.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn dropping_prepared_session_stops_sources_and_probe_is_reused() {
    let metrics = Arc::new(Metrics::default());
    let prepared = prepare_hls(
        HlsInputs::new(observed("ts_avc_regular", metrics.clone(), false, false)),
        PrepareOptions::default(),
    )
    .await
    .unwrap();
    drop(prepared);
    assert_eq!(metrics.stops.load(Ordering::SeqCst), 1);
    let metrics = Arc::new(Metrics::default());
    let pair = HlsInputs::new(observed("ts_avc_regular", metrics.clone(), false, false))
        .with_audio(observed("ts_aac_audio_only", metrics.clone(), false, false));
    let report = prepare_hls(
        pair,
        PrepareOptions::default()
            .with_budget(ResourceBudget::default().with_max_in_flight_reads(1)),
    )
    .await
    .unwrap()
    .write_to(&mut Vec::new())
    .await
    .unwrap();
    assert_eq!(metrics.peak.load(Ordering::SeqCst), 1);
    assert_eq!(
        metrics.reads.load(Ordering::SeqCst),
        report.media().segment_count
    );
}
#[tokio::test]
async fn probe_cancellation_drops_inflight_futures() {
    let metrics = Arc::new(Metrics::default());
    let cancel = Arc::new(Cancel::default());
    let pair = HlsInputs::new(observed("ts_avc_regular", metrics.clone(), true, false))
        .with_audio(observed("ts_aac_audio_only", metrics.clone(), true, false));
    let mut future = Box::pin(prepare_hls(
        pair,
        PrepareOptions::default().with_cancel(cancel.clone()),
    ));
    while metrics.active.load(Ordering::SeqCst) < 2 {
        assert!(
            std::future::poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx)))
                .await
                .is_pending()
        );
    }
    cancel.0.store(true, Ordering::SeqCst);
    assert!(matches!(
        future.await.err().unwrap().error(),
        Error::Cancelled
    ));
    assert_eq!(metrics.active.load(Ordering::SeqCst), 0);
    assert_eq!(metrics.stops.load(Ordering::SeqCst), 2);
}

mod common;
fn shifted(name: &str, offset: u64) -> HlsInput {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/media")
        .join(name);
    let mut source = MemorySource::new().text(
        "https://shift.test/input.m3u8",
        std::fs::read_to_string(root.join("input.m3u8")).unwrap(),
    );
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().unwrap() == "ts" {
            source = source.segment(
                format!(
                    "https://shift.test/{}",
                    path.file_name().unwrap().to_str().unwrap()
                ),
                common::shift_ts(std::fs::read(path).unwrap(), offset),
            );
        }
    }
    HlsInput::custom(
        Arc::new(source),
        SourceLocation::Url(url::Url::parse("https://shift.test/input.m3u8").unwrap()),
    )
}
#[tokio::test]
async fn cross_input_wrap_preserves_positive_and_negative_offsets() {
    let period = 1u64 << 33;
    for (video_shift, audio_shift, offset) in [
        (period - 132_000, period - 123_000, 9000i128),
        (period - 123_000, period - 132_000, -9000),
    ] {
        let inputs = || {
            HlsInputs::new(shifted("ts_avc_video_only", video_shift))
                .with_audio(shifted("ts_aac_audio_only", audio_shift))
        };
        let session = prepare_hls(inputs(), PrepareOptions::default())
            .await
            .unwrap();
        let mapping = session.info().timeline().clone();
        assert_eq!(
            mapping.tracks()[1].wrap_anchor().unwrap() - mapping.tracks()[0].wrap_anchor().unwrap(),
            offset
        );
        let (_, batch) = session.into_mp4_bytes().await.unwrap();
        let stream = prepare_hls(inputs(), PrepareOptions::default())
            .await
            .unwrap()
            .write_to(&mut Vec::new())
            .await
            .unwrap();
        assert_eq!(batch.media().tracks, stream.media().tracks);
        let cue = mapping
            .unwrap_mpegts(3000, mapping.tracks()[1].wrap_anchor().unwrap())
            .unwrap();
        assert!(mapping.to_output(cue, 90_000).unwrap() >= 0);
    }
}
fn packed(name: &str) -> HlsInput {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/media")
        .join(name);
    let original = std::fs::read_to_string(root.join("input.m3u8")).unwrap();
    let mut playlist = String::from("#EXTM3U\n");
    let mut bytes = Vec::new();
    if root.join("init.fmp4").exists() {
        bytes = std::fs::read(root.join("init.fmp4")).unwrap();
        playlist.push_str(&format!(
            "#EXT-X-MAP:URI=\"all\",BYTERANGE=\"{}@0\"\n",
            bytes.len()
        ));
    }
    let mut first = true;
    for line in original.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let data = std::fs::read(root.join(line)).unwrap();
        playlist.push_str(&format!(
            "#EXTINF:2,\n#EXT-X-BYTERANGE:{}{}\nall\n",
            data.len(),
            if first {
                format!("@{}", bytes.len())
            } else {
                String::new()
            }
        ));
        first = false;
        bytes.extend(data);
    }
    playlist.push_str("#EXT-X-ENDLIST\n");
    let source = MemorySource::new()
        .text("https://packed.test/input.m3u8", playlist)
        .segment("https://packed.test/all", bytes);
    HlsInput::custom(
        Arc::new(source),
        SourceLocation::Url(url::Url::parse("https://packed.test/input.m3u8").unwrap()),
    )
}
#[tokio::test]
async fn map_and_implicit_ranges_do_not_mix_same_url_across_roles() {
    for (v, a) in [
        ("ts_avc_video_only", "ts_aac_audio_only"),
        ("fmp4_avc_video_only", "fmp4_aac_audio_only"),
    ] {
        let inputs = HlsInputs::new(packed(v)).with_audio(packed(a));
        let (_, report) = prepare_hls(inputs, PrepareOptions::default())
            .await
            .unwrap()
            .into_mp4_bytes()
            .await
            .unwrap();
        assert_eq!(report.media().tracks[0].sample_count, 180);
        assert_eq!(report.media().tracks[1].sample_count, 283);
    }
}
#[tokio::test]
async fn resource_limit_and_legacy_options_fail_without_outputs() {
    let options = PrepareOptions::default()
        .with_budget(ResourceBudget::default().with_max_resource_bytes(1000));
    let error = prepare_hls(HlsInputs::new(input("ts_avc_regular")), options)
        .await
        .err()
        .unwrap();
    assert_eq!(error.role(), Some(InputRole::Primary));
    assert!(matches!(error.error(), Error::InvalidInput(_)));
    let legacy = TransmuxOptions {
        on_progress: Some(Arc::new(|_| panic!("must not call checkpoint callback"))),
        ..Default::default()
    };
    assert!(matches!(
        PrepareOptions::try_from(legacy).err().unwrap().error(),
        Error::InvalidInput(_)
    ));
}

#[derive(Debug)]
struct Synthetic {
    count: usize,
    reset: bool,
}
impl Source for Synthetic {
    fn read_text<'a>(
        &'a self,
        location: &'a SourceLocation,
    ) -> Pin<Box<dyn Future<Output = Result<TextResource>> + Send + 'a>> {
        Box::pin(async move {
            Ok(TextResource {
                location: location.clone(),
                content: format!(
                    "#EXTM3U\n{}#EXT-X-ENDLIST\n",
                    (0..self.count)
                        .map(|i| format!("#EXTINF:2,\n{i}.ts\n"))
                        .collect::<String>()
                ),
            })
        })
    }
    fn read_bytes<'a>(
        &'a self,
        location: &'a SourceLocation,
        _: Option<&'a ByteRange>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>>> + Send + 'a>> {
        Box::pin(async move {
            let SourceLocation::Url(url) = location else {
                unreachable!()
            };
            let index = url
                .path()
                .trim_start_matches('/')
                .trim_end_matches(".ts")
                .parse()
                .unwrap();
            Ok(common::continuous_ts(
                include_bytes!("fixtures/h264_aac_fhd.ts").to_vec(),
                if self.reset { 0 } else { index },
            ))
        })
    }
}
fn synthetic(count: usize, reset: bool) -> HlsInput {
    HlsInput::custom(
        Arc::new(Synthetic { count, reset }),
        SourceLocation::Url(url::Url::parse("https://synthetic.test/input.m3u8").unwrap()),
    )
}
#[derive(Default)]
struct MeasuredSink {
    largest_write: usize,
    bytes: usize,
}
impl AsyncWrite for MeasuredSink {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.largest_write = self.largest_write.max(data.len());
        self.bytes += data.len();
        Poll::Ready(Ok(data.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
#[tokio::test]
async fn media_window_is_independent_of_total_duration() {
    for mfra in [false, true] {
        let mut peaks = Vec::new();
        for count in [4, 64] {
            let peak = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let record = peak.clone();
            let options = PrepareOptions::default()
                .with_write_mfra(mfra)
                .with_on_event(Arc::new(move |e| {
                    record.fetch_max(
                        e.downloaded_segments() - e.processed_segments(),
                        Ordering::SeqCst,
                    );
                }));
            let mut sink = MeasuredSink::default();
            let report = prepare_hls(
                HlsInputs::new(synthetic(count, false)).with_audio(synthetic(count + 1, false)),
                options,
            )
            .await
            .unwrap()
            .write_to(&mut sink)
            .await
            .unwrap();
            assert_eq!(report.media().segment_count, 2 * count + 1);
            assert!(peak.load(Ordering::SeqCst) <= 4);
            peaks.push(sink.largest_write);
        }
        assert_eq!(peaks[0], peaks[1]);
    }
}
#[tokio::test]
async fn late_timestamp_reset_fails_stream_without_completion() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let copy = events.clone();
    let mut output = Vec::new();
    let err = prepare_hls(
        HlsInputs::new(synthetic(3, false)).with_audio(synthetic(3, true)),
        PrepareOptions::default().with_on_event(Arc::new(move |e| copy.lock().unwrap().push(e))),
    )
    .await
    .unwrap()
    .write_to(&mut output)
    .await
    .unwrap_err();
    assert_eq!(err.role(), Some(InputRole::Audio));
    assert_eq!(err.segment_index(), Some(1));
    assert!(matches!(err.error(), Error::Unsupported(_)));
    assert!(output.windows(4).any(|b| b == b"moov"));
    assert!(
        !events
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.phase() == SessionPhase::Completed)
    );
}

#[tokio::test]
async fn cancelled_finalize_preserves_existing_target_and_nonresumable_partial() {
    let directory =
        std::env::temp_dir().join(format!("prepared-finalize-cancel-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let target = directory.join("out.mp4");
    std::fs::write(&target, b"previous output").unwrap();
    let cancel = Arc::new(Cancel::default());
    let token = cancel.clone();
    let options = PrepareOptions::default()
        .with_cancel(cancel)
        .with_on_event(Arc::new(move |e| {
            if e.phase() == SessionPhase::Finalizing {
                token.0.store(true, Ordering::SeqCst);
            }
            assert_ne!(e.phase(), SessionPhase::Completed);
        }));
    let error = prepare_hls(pair("ts_avc_regular", "fmp4_aac_audio_only"), options)
        .await
        .unwrap()
        .write_to_file(&target, FileOutputOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(error.error(), Error::Cancelled));
    assert_eq!(std::fs::read(&target).unwrap(), b"previous output");
    let partials: Vec<_> = std::fs::read_dir(&directory)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().ends_with("partial.mp4"))
        .collect();
    assert_eq!(partials.len(), 1);
    assert!(
        std::fs::read(&partials[0])
            .unwrap()
            .windows(4)
            .any(|b| b == b"moof")
    );
    std::fs::remove_dir_all(directory).unwrap();
}
