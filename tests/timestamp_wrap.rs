//! End-to-end TS wrap across segments and checkpoint recovery.
#![cfg(not(target_arch = "wasm32"))]
use hls_transmux::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::{future::Future, pin::Pin};
mod common;
#[derive(Debug, Default)]
struct Cancel(AtomicBool);
impl CancelToken for Cancel {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}
fn input() -> HlsInput {
    let source = MemorySource::new()
        .text(
            "https://wrap.test/list.m3u8",
            "#EXTM3U\n#EXTINF:2,\n0.ts\n#EXTINF:2,\n1.ts\n#EXT-X-ENDLIST\n",
        )
        .segment(
            "https://wrap.test/0.ts",
            common::shift_ts(
                include_bytes!("fixtures/h264_aac_fhd.ts").to_vec(),
                (1 << 33) - 150_000,
            ),
        )
        .segment(
            "https://wrap.test/1.ts",
            common::shift_ts(
                common::continuous_ts(include_bytes!("fixtures/h264_aac_fhd.ts").to_vec(), 1),
                (1 << 33) - 150_000,
            ),
        );
    HlsInput::custom(
        Arc::new(source),
        SourceLocation::Url(url::Url::parse("https://wrap.test/list.m3u8").unwrap()),
    )
}
#[tokio::test]
async fn ts_wrap_matches_uninterrupted_output_after_resume() {
    let dir = std::env::temp_dir().join(format!("hls-wrap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let full = dir.join("full.fmp4");
    let resumed = dir.join("resumed.fmp4");
    let expected = transmux_hls_to_mp4_async(
        input(),
        &full,
        TransmuxOptions {
            output_format: OutputFormat::FragmentedMp4,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let state = Arc::new(Mutex::new(None));
    let save = state.clone();
    let cancel = Arc::new(Cancel::default());
    let token = cancel.clone();
    let result = transmux_hls_to_mp4_async(
        input(),
        &resumed,
        TransmuxOptions {
            output_format: OutputFormat::FragmentedMp4,
            cancel: Some(cancel),
            on_progress: Some(Arc::new(move |p| {
                *save.lock().unwrap() = Some(p.resume);
                token.0.store(true, Ordering::SeqCst);
            })),
            ..Default::default()
        },
    )
    .await;
    assert!(matches!(result, Err(Error::Cancelled)));
    let checkpoint = state.lock().unwrap().clone().unwrap();
    assert!(checkpoint.global_base_dts_90k > 1 << 33);
    let actual = transmux_hls_to_mp4_async(
        input(),
        &resumed,
        TransmuxOptions {
            output_format: OutputFormat::FragmentedMp4,
            resume: Some(checkpoint),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(actual, expected);
    // Initialization includes wall-clock creation times; fragments/index are exact.
    let a = std::fs::read(full).unwrap();
    let b = std::fs::read(resumed).unwrap();
    let start = a.windows(4).position(|v| v == b"styp").unwrap() - 4;
    assert_eq!(&a[start..], &b[start..]);
    std::fs::remove_dir_all(dir).unwrap();
}
