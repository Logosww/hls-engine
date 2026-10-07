//! Deterministic state/read instrumentation shared by native, Node WASM and Chrome.
use hls_engine::legacy::{crypto::key::*, playlist::*, *};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};
#[path = "../common/mod.rs"]
pub mod clocks;

#[derive(Debug)]
struct Generated {
    reads: Arc<AtomicUsize>,
    long_gop: bool,
}
impl Source for Generated {
    fn read_text<'a>(
        &'a self,
        _: &'a SourceLocation,
    ) -> Pin<Box<dyn Future<Output = Result<TextResource>> + Send + 'a>> {
        unreachable!()
    }
    fn read_bytes<'a>(
        &'a self,
        location: &'a SourceLocation,
        _: Option<&'a ByteRange>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>>> + Send + 'a>> {
        Box::pin(async move {
            self.reads.fetch_add(1, Ordering::SeqCst);
            let index = clocks::segment_index(location);
            let mut bytes = clocks::shift_ts(
                include_bytes!("../fixtures/media/ts_avc_video_only/seg0.ts").to_vec(),
                index as u64 * 180_000,
            );
            // Synthetic budget-only vector: retain the first RAP, remove later
            // IDR declarations. This vector makes no independent decode claim.
            if self.long_gop && index != 0 {
                for i in 0..bytes.len().saturating_sub(4) {
                    if bytes[i..i + 3] == [0, 0, 1] && bytes[i + 3] & 31 == 5 {
                        bytes[i + 3] = (bytes[i + 3] & 0xe0) | 1;
                    }
                }
            }
            Ok(bytes)
        })
    }
}
pub type Observer = std::rc::Rc<dyn Fn(&str, usize, &str)>;
struct Sink {
    observer: Option<Observer>,
    segments: usize,
    mode: &'static str,
    reads: Arc<AtomicUsize>,
    first_read_count: Option<usize>,
    bytes: usize,
}
impl tokio::io::AsyncWrite for Sink {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if self.first_read_count.is_none() && !bytes.is_empty() {
            self.first_read_count = Some(self.reads.load(Ordering::SeqCst));
            if let Some(observer) = &self.observer {
                observer("first-write", self.segments, self.mode);
            }
        }
        self.bytes += bytes.len();
        Poll::Ready(Ok(bytes.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        panic!("caller owns shutdown")
    }
}
struct NoKeys;
impl KeyProvider for NoKeys {
    fn resolve(&self, _: KeyRequest) -> KeyFuture<KeyResolution> {
        panic!("clear input")
    }
}
struct Clock;
impl KeyClock for Clock {
    fn now(&self) -> u64 {
        0
    }
}

#[allow(dead_code)]
pub async fn run() -> serde_json::Value {
    run_observed(None).await
}
pub async fn run_observed(observer: Option<Observer>) -> serde_json::Value {
    let mut cases = Vec::new();
    for segments in [8, 64, 256] {
        for mode in ["near", "far", "full", "long-gop", "resource-budget"] {
            if let Some(observer) = &observer {
                observer("start", segments as usize, mode);
            }
            let reads = Arc::new(AtomicUsize::new(0));
            let mut text = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n".to_owned();
            for i in 0..segments {
                text.push_str(&format!("#EXTINF:2,\nsegment-{i}.ts\n"));
            }
            text.push_str("#EXT-X-ENDLIST\n");
            let snapshot = parse_playlist_snapshot(
                &TextResource {
                    content: text,
                    location: SourceLocation::Url(
                        "https://budget.invalid/media.m3u8".parse().unwrap(),
                    ),
                },
                PlaylistContext::new(InputId::new("primary").unwrap(), 0),
            )
            .unwrap();
            let input = KeyedInputs::new(KeyedInput::new(
                snapshot,
                Arc::new(Generated {
                    reads: reads.clone(),
                    long_gop: mode == "long-gop",
                }),
            ));
            let keys = KeySession::new(
                "budget",
                "test",
                Arc::new(NoKeys),
                Arc::new(Clock),
                KeySessionOptions::default(),
            )
            .unwrap();
            let mut options = TimelinePrepareOptions::default().with_planning_limits(
                TimelinePlanningLimits::new(if mode == "resource-budget" { 59 } else { 240 }, 4)
                    .unwrap(),
            );
            if mode != "full" && mode != "resource-budget" {
                let start = if mode == "near" {
                    100
                } else {
                    (segments - 2) * 2000 + 100
                };
                options = options.with_range(
                    PresentationRange::new(
                        MediaTime::new(start, 1000).unwrap(),
                        MediaTime::new(start + 300, 1000).unwrap(),
                    )
                    .unwrap(),
                );
            }
            let mut sink = Sink {
                observer: observer.clone(),
                segments: segments as usize,
                mode,
                reads: reads.clone(),
                first_read_count: None,
                bytes: 0,
            };
            let result = prepare_hls_timeline(input, keys, options)
                .await
                .unwrap()
                .write_to(&mut sink)
                .await;
            let result = if mode == "resource-budget" {
                assert!(sink.first_read_count.is_none());
                let error = result.unwrap_err();
                assert_eq!(error.kind(), TimelineErrorKind::PlanningBudgetExceeded);
                serde_json::json!({"error": format!("{:?}", error.kind())})
            } else {
                let report = result.unwrap();
                assert!(report.peak_planned_samples() <= 180);
                assert!(report.peak_planned_resources() <= 3);
                assert_eq!(
                    report.outputs()[0].media().tracks[0].sample_count,
                    if mode == "full" || mode == "long-gop" {
                        segments as usize * 60
                    } else {
                        60
                    }
                );
                if mode == "near" {
                    assert_eq!(sink.first_read_count, Some(3));
                }
                serde_json::json!({"samples": report.peak_planned_samples().to_string(), "resources": report.peak_planned_resources().to_string(),
                    "first_write_reads": sink.first_read_count.unwrap().to_string(), "source_bytes": report.source_bytes().to_string()})
            };
            if let Some(observer) = &observer {
                observer("end", segments as usize, mode);
            }
            cases.push(serde_json::json!({"segments": segments.to_string(), "mode": mode, "reads": reads.load(Ordering::SeqCst).to_string(), "result": result}));
        }
    }
    serde_json::json!(cases)
}
