use super::sample_corpus as sample;
use hls_transmux::{playlist::*, *};
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
    run_counts(&[8, 64, 256]).await
}
pub async fn run_counts(counts: &[u64]) -> serde_json::Value {
    let mut rows = Vec::new();
    for &count in counts {
        let (baseline, total) = crate::allocation::reset();
        let started = now();
        let id = InputId::new("primary").unwrap();
        let parse = |text: String, revision| {
            parse_playlist_snapshot(
                &TextResource {
                    location: SourceLocation::Url(
                        url::Url::parse("https://profile.test/input").unwrap(),
                    ),
                    content: text,
                },
                PlaylistContext::new(InputId::new("primary").unwrap(), 0).with_revision(revision),
            )
            .unwrap()
        };
        let source = MemorySource::new()
            .segment(
                "https://profile.test/init",
                include_bytes!("../fixtures/sample_crypto/fmp4_aac_clear/init.mp4"),
            )
            .segment(
                "https://profile.test/media",
                include_bytes!("../fixtures/sample_crypto/fmp4_aac_clear/seg1.m4s"),
            );
        let holder = Arc::new(Mutex::new(None::<ContinuousHandle>));
        let callback = holder.clone();
        let options=ContinuousOptions::default().with_limits(ContinuousLimits::default().with_queue(2,4096).with_history(4)).with_on_event(Arc::new(move|event|{
            if let ContinuousEvent::Committed {input,..}=event {
                let n=input.committed();let guard=callback.lock().unwrap();let h=guard.as_ref().unwrap();
                if n==count {h.stop();return;}
                let text=format!("#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:{}\n#EXT-X-DISCONTINUITY-SEQUENCE:{}\n#EXT-X-MAP:URI=\"init\"\n#EXTINF:2,\nmedia\n#EXT-X-DISCONTINUITY\n#EXTINF:2,\nmedia\n",n-1,n-1);
                h.accept_snapshot(&InputId::new("primary").unwrap(),&parse(text,n+1)).unwrap();
            }
        }));
        let session = ContinuousSession::new(
            ContinuousInputs::new(ContinuousInput::new(id.clone(), Arc::new(source))),
            sample::keys(Arc::new(sample::Provider)),
            options,
        )
        .unwrap();
        let h = session.handle();
        *holder.lock().unwrap() = Some(h.clone());
        h.accept_snapshot(
            &id,
            &parse(
                "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"init\"\n#EXTINF:2,\nmedia\n"
                    .into(),
                1,
            ),
        )
        .unwrap();
        let first = Rc::new(Cell::new(None));
        let mut sink = Sink {
            first: first.clone(),
            started,
            yielded: false,
        };
        let report = session.write_to(&mut sink).await.unwrap();
        assert_eq!(report.inputs()[0].committed(), count);
        assert!(report.mappings().len() <= 4);
        let (_, peak, allocated) = crate::allocation::snapshot();
        drop(h);
        drop(holder);
        let retained = crate::allocation::snapshot().0;
        rows.push(serde_json::json!({"segments":count,"peakIncrementBytes":peak.saturating_sub(baseline),"retainedIncrementBytes":retained.saturating_sub(baseline),"allocatedBytes":allocated.wrapping_sub(total),"firstWriteMs":first.get(),"elapsedMs":now()-started,"peaks":report.peaks(),"retainedMappings":report.mappings().len(),"bytes":report.bytes_written().to_string()}));
    }
    serde_json::json!(rows)
}
