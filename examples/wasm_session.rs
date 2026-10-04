//! No JS bindings required: execute the actual wasm through scripts/test_wasm_session.mjs.
use hls_transmux::*;
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, Waker},
};
use tokio::io::AsyncWrite;
fn primary() -> HlsInput {
    let source = MemorySource::new()
        .text(
            "https://test/video.m3u8",
            "#EXTM3U\n#EXTINF:2,\na.ts\n#EXTINF:2,\nb.ts\n#EXTINF:2,\nc.ts\n#EXT-X-ENDLIST\n",
        )
        .segment(
            "https://test/a.ts",
            include_bytes!("../tests/fixtures/media/ts_avc_video_only/seg0.ts").as_slice(),
        )
        .segment(
            "https://test/b.ts",
            include_bytes!("../tests/fixtures/media/ts_avc_video_only/seg1.ts").as_slice(),
        )
        .segment(
            "https://test/c.ts",
            include_bytes!("../tests/fixtures/media/ts_avc_video_only/seg2.ts").as_slice(),
        );
    HlsInput::custom(
        Arc::new(source),
        SourceLocation::Url(url::Url::parse("https://test/video.m3u8").unwrap()),
    )
}
fn audio() -> HlsInput {
    let source = MemorySource::new()
        .text("https://test/audio.m3u8", "#EXTM3U\n#EXT-X-MAP:URI=\"init\"\n#EXTINF:2,\nx\n#EXTINF:2,\ny\n#EXTINF:2,\nz\n#EXTINF:0.04,\nw\n#EXT-X-ENDLIST\n")
        .segment("https://test/init", include_bytes!("../tests/fixtures/media/fmp4_aac_audio_only/init.fmp4").as_slice())
        .segment("https://test/x", include_bytes!("../tests/fixtures/media/fmp4_aac_audio_only/seg0.m4s").as_slice())
        .segment("https://test/y", include_bytes!("../tests/fixtures/media/fmp4_aac_audio_only/seg1.m4s").as_slice())
        .segment("https://test/z", include_bytes!("../tests/fixtures/media/fmp4_aac_audio_only/seg2.m4s").as_slice())
        .segment("https://test/w", include_bytes!("../tests/fixtures/media/fmp4_aac_audio_only/seg3.m4s").as_slice());
    HlsInput::custom(
        Arc::new(source),
        SourceLocation::Url(url::Url::parse("https://test/audio.m3u8").unwrap()),
    )
}
#[derive(Debug, Default)]
struct Cancel(AtomicBool);
impl CancelToken for Cancel {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::poll_fn(|_| {
            if self.is_cancelled() {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }))
    }
}
struct Writer {
    bytes: Vec<u8>,
    pending: bool,
    cancel: Option<Arc<Cancel>>,
}
impl AsyncWrite for Writer {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if let Some(cancel) = &self.cancel {
            cancel.0.store(true, Ordering::Relaxed);
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        self.pending = !self.pending;
        if self.pending {
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        let n = data.len().min(997);
        self.bytes.extend_from_slice(&data[..n]);
        Poll::Ready(Ok(n))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
async fn contracts() {
    let prepare = || {
        prepare_hls(
            HlsInputs::new(primary()).with_audio(audio()),
            PrepareOptions::default(),
        )
    };
    let prepared = prepare().await.unwrap();
    let mapping = prepared.info().timeline().clone();
    let (bytes, batch) = prepared.into_mp4_bytes().await.unwrap();
    assert!(bytes.windows(4).any(|b| b == b"moov"));
    let mut writer = Writer {
        bytes: vec![],
        pending: false,
        cancel: None,
    };
    let streamed = prepare()
        .await
        .unwrap()
        .write_to(&mut writer)
        .await
        .unwrap();
    assert_eq!(batch.media().tracks, streamed.media().tracks);
    assert_eq!(batch.media().tracks[0].sample_count, 180);
    assert_eq!(batch.media().tracks[1].sample_count, 283);
    assert_eq!(streamed.timeline(), &mapping);
    assert_eq!(mapping.to_output(mapping.origin(), 90_000).unwrap(), 0);
    let cancel = Arc::new(Cancel::default());
    let prepared = prepare_hls(
        HlsInputs::new(primary()).with_audio(audio()),
        PrepareOptions::default().with_cancel(cancel.clone()),
    )
    .await
    .unwrap();
    writer.cancel = Some(cancel);
    assert!(matches!(
        prepared.write_to(&mut writer).await.unwrap_err().error(),
        Error::Cancelled
    ));
}
#[unsafe(no_mangle)]
pub extern "C" fn run_contract_tests() -> u32 {
    let mut future = std::pin::pin!(contracts());
    let mut cx = Context::from_waker(Waker::noop());
    for _ in 0..100_000 {
        if future.as_mut().poll(&mut cx).is_ready() {
            return 1;
        }
    }
    0
}
