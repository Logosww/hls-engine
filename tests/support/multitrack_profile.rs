use super::sample_corpus as sample;
use hls_engine::legacy::{playlist::*, *};
use std::{
    cell::Cell,
    pin::Pin,
    rc::Rc,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};
#[cfg(not(target_arch = "wasm32"))]
fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
        * 1000.0
}
#[cfg(target_arch = "wasm32")]
fn now() -> f64 {
    js_sys::Date::now()
}
struct Sink {
    first: Rc<Cell<Option<f64>>>,
    started: f64,
    yielded: bool,
}
impl tokio::io::AsyncWrite for Sink {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        b: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.yielded = !self.yielded;
        if self.yielded {
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        if self.first.get().is_none() {
            self.first.set(Some(now() - self.started));
        }
        Poll::Ready(Ok(b.len().min(4096)))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        panic!("caller owns close")
    }
}
#[cfg(not(target_arch = "wasm32"))]
pub async fn run() -> serde_json::Value {
    serde_json::json!({"fragmented":run_counts(&[64,512,4096]).await,"classic":run_counts_inner(&[64,512,4096],true).await})
}
pub async fn run_counts(counts: &[u64]) -> serde_json::Value {
    run_counts_inner(counts, false).await
}
async fn run_counts_inner(counts: &[u64], classic: bool) -> serde_json::Value {
    let mut rows = Vec::new();
    for &count in counts {
        let (baseline, total) = crate::allocation::reset();
        let started = now();
        let id = InputId::new("primary").unwrap();
        let source = MemorySource::new()
            .segment(
                "https://profile.test/init",
                include_bytes!("../fixtures/sample_crypto/fmp4_aac_clear/init.mp4"),
            )
            .segment(
                "https://profile.test/media",
                include_bytes!("../fixtures/sample_crypto/fmp4_aac_clear/seg1.m4s"),
            );
        let holder = Arc::new(Mutex::new(None::<MultiTrackHandle>));
        let callback = holder.clone();
        let options = ContinuousOptions::default()
            .with_waiter(Arc::new(Wait), std::time::Duration::from_secs(1))
            .with_limits(
                ContinuousLimits::default()
                    .with_queue(8, 16384)
                    .with_history(4),
            )
            .with_on_event(Arc::new(move |event| {
                if let ContinuousEvent::Committed { input, .. } = event {
                    let n = input.committed();
                    let guard = callback.lock().unwrap();
                    let h = guard.as_ref().unwrap();
                    if n == count {
                        h.end_input(input.input_id()).unwrap();
                        return;
                    }
                    h.accept_snapshot(
                        input.input_id(),
                        &snapshot(input.input_id(), n - 1, 2, n + 1),
                    )
                    .unwrap();
                    if input.input_id().as_str() == "primary" {
                        let cc = h.subtitle_track_id(&InputId::new("cc").unwrap()).unwrap();
                        h.accept_cues(
                            cc,
                            &[SubtitleCue::new(
                                0,
                                n,
                                MediaTime::new(0, 1000).unwrap(),
                                MediaTime::new(500, 1000).unwrap(),
                                "bounded captions",
                            )],
                        )
                        .unwrap();
                    }
                }
            }));
        let source = Arc::new(source);
        let inputs = MultiTrackInputs::new(
            ContinuousInput::new(id.clone(), source.clone()),
            EmbeddedAudio::Keep,
        )
        .with_audio(
            ContinuousInput::new(InputId::new("a").unwrap(), source.clone()),
            TrackMetadata::new("en", "English"),
        )
        .with_audio(
            ContinuousInput::new(InputId::new("b").unwrap(), source),
            TrackMetadata::new("ja", "Japanese"),
        )
        .with_subtitle(SubtitleTrack::new(
            InputId::new("cc").unwrap(),
            id.clone(),
            TrackMetadata::default(),
        ));
        let session =
            MultiTrackSession::new(inputs, sample::keys(Arc::new(sample::Provider)), options)
                .unwrap();
        let h = session.handle();
        *holder.lock().unwrap() = Some(h.clone());
        for name in ["primary", "a", "b"] {
            let id = InputId::new(name).unwrap();
            h.accept_snapshot(&id, &snapshot(&id, 0, 1, 1)).unwrap();
        }
        h.accept_cues(
            h.subtitle_track_id(&InputId::new("cc").unwrap()).unwrap(),
            &[SubtitleCue::new(
                0,
                0,
                MediaTime::new(0, 1000).unwrap(),
                MediaTime::new(500, 1000).unwrap(),
                "bounded captions",
            )],
        )
        .unwrap();
        let first = Rc::new(Cell::new(None));
        let mut sink = Sink {
            first: first.clone(),
            started,
            yielded: false,
        };
        let report = if classic {
            #[cfg(not(target_arch = "wasm32"))]
            {
                let path = std::env::temp_dir().join(format!(
                    "hls-multi-profile-{}-{count}.mp4",
                    std::process::id()
                ));
                let report = session
                    .write_to_file(
                        &path,
                        FileOutputOptions::default().with_format(OutputFormat::StreamingMp4),
                    )
                    .await
                    .unwrap();
                std::fs::remove_file(path).unwrap();
                report
            }
            #[cfg(target_arch = "wasm32")]
            {
                unreachable!("native file backend only")
            }
        } else {
            session.write_to(&mut sink).await.unwrap()
        };
        assert!(
            report
                .media()
                .inputs()
                .iter()
                .all(|i| i.committed() == count)
        );
        assert!(report.subtitle_reports().len() <= 4);
        assert!(report.media().mappings().len() <= 4);
        let (_, peak, allocated) = crate::allocation::snapshot();
        drop(h);
        drop(holder);
        let retained = crate::allocation::snapshot().0;
        rows.push(serde_json::json!({"segments":count,"classicIndexSamples":report.media().outputs()[0].classic_index_samples().to_string(),"peakIncrementBytes":peak.saturating_sub(baseline),"retainedIncrementBytes":retained.saturating_sub(baseline),"allocatedBytes":allocated.wrapping_sub(total),"firstWriteMs":first.get(),"elapsedMs":now()-started,"peaks":report.media().peaks(),"retainedMappings":report.media().mappings().len(),"bytes":report.media().bytes_written().to_string()}));
    }
    serde_json::json!(rows)
}

struct Wait;
impl ContinuousWait for Wait {
    #[cfg(not(target_arch = "wasm32"))]
    fn wait(
        &self,
        _: std::time::Duration,
    ) -> Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
    #[cfg(target_arch = "wasm32")]
    fn wait(&self, _: std::time::Duration) -> Pin<Box<dyn std::future::Future<Output = ()> + '_>> {
        Box::pin(std::future::pending())
    }
}
fn snapshot(id: &InputId, start: u64, segments: u64, revision: u64) -> PlaylistSnapshot {
    let mut text = format!(
        "#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXT-X-MEDIA-SEQUENCE:{start}\n#EXT-X-DISCONTINUITY-SEQUENCE:{start}\n#EXT-X-MAP:URI=\"init\"\n"
    );
    for i in 0..segments {
        if i > 0 {
            text.push_str("#EXT-X-DISCONTINUITY\n");
        }
        let nanos = (start + i) * 94 * 1024 * 1_000_000_000 / 48000;
        let seconds = nanos / 1_000_000_000;
        text.push_str(&format!(
            "#EXT-X-PROGRAM-DATE-TIME:2026-10-06T{:02}:{:02}:{:02}.{:09}Z\n#EXTINF:2.005333333,\nmedia\n",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60,
            nanos%1_000_000_000
        ));
    }
    parse_playlist_snapshot(
        &TextResource {
            location: SourceLocation::Url(url::Url::parse("https://profile.test/input").unwrap()),
            content: text,
        },
        PlaylistContext::new(id.clone(), 0).with_revision(revision),
    )
    .unwrap()
}
