//! Sample-path requested allocation and first-write measurements (not total RSS).
use hls_transmux::*;
use std::{
    pin::Pin,
    sync::Arc,
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
    start: f64,
    first: Option<f64>,
    bytes: usize,
    pending: bool,
}
impl tokio::io::AsyncWrite for Sink {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.pending = !self.pending;
        if self.pending {
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        if self.first.is_none() {
            self.first = Some(now() - self.start);
        }
        let n = bytes.len().min(4093);
        self.bytes += n;
        Poll::Ready(Ok(n))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        panic!("caller owns writer")
    }
}
pub async fn run(provider: Arc<dyn hls_transmux::crypto::key::KeyProvider>) -> serde_json::Value {
    let mut rows = Vec::new();
    for name in ["fmp4_avc_cenc", "fmp4_hevc_cbcs", "ts_avc_sample"] {
        let inputs = KeyedInputs::new(crate::sample_corpus::input(name, "primary"));
        let (live, total) = crate::allocation::reset();
        let start = now();
        let mut sink = Sink {
            start,
            first: None,
            bytes: 0,
            pending: false,
        };
        let session = prepare_hls_timeline(
            inputs,
            crate::sample_corpus::keys(provider.clone()),
            TimelinePrepareOptions::default(),
        )
        .await
        .unwrap();
        let report = session.write_to(&mut sink).await.unwrap();
        let (end, peak, allocated) = crate::allocation::snapshot();
        rows.push(serde_json::json!({"name":name,"raw_payload_peak_bytes":report.peak_raw_sample_bytes(),"replay_payload_peak_bytes":report.peak_replay_sample_bytes(),"peak_increment_bytes":peak.saturating_sub(live),"retained_increment_bytes":end.saturating_sub(live),"allocated_bytes":allocated.wrapping_sub(total),"first_write_ms":sink.first,"elapsed_ms":now()-start,"output_bytes":sink.bytes}));
    }
    serde_json::json!({"measurements":rows})
}
