//! Actual schema-v1 checkpoints/prefix emitted by v0.3.0 at commit 8cde2e3.
#![cfg(all(feature = "serde", not(target_arch = "wasm32")))]
mod common;
use hls_transmux::*;
use std::future::Future;
use std::pin::Pin;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Debug, Default)]
struct Cancel(AtomicBool);
impl CancelToken for Cancel {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}

#[derive(Debug)]
struct LegacySource {
    source: MemorySource,
    reads: std::sync::atomic::AtomicUsize,
}
impl Source for LegacySource {
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
            let data = self.source.read_bytes(location, range).await?;
            Ok(common::continuous_ts(
                data,
                self.reads.fetch_add(1, Ordering::SeqCst),
            ))
        })
    }
}

#[tokio::test]
async fn released_v030_checkpoint_can_resume_downloading_and_retry_finalize() {
    let dir = std::env::temp_dir().join(format!("hls-v030-compat-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let prefix = include_bytes!("fixtures/v030_prefix.fmp4");
    let checkpoint: TransmuxResumeState =
        serde_json::from_slice(include_bytes!("fixtures/v030_finalizing.json")).unwrap();
    let partial = dir.join("final.partial.mp4");
    let output = dir.join("final.mp4");
    std::fs::write(&partial, prefix).unwrap();
    let report = finalize_partial_mp4_async(
        &partial,
        &output,
        checkpoint,
        TransmuxOptions {
            output_format: OutputFormat::StreamingMp4,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(report.tracks.len(), 2);
    assert_eq!(
        report.bytes_written,
        std::fs::metadata(&output).unwrap().len()
    );
    assert!(!partial.exists());

    let checkpoint: TransmuxResumeState =
        serde_json::from_slice(include_bytes!("fixtures/v030_downloading.json")).unwrap();
    let partial = dir.join("resume.partial.mp4");
    let output = dir.join("resume.mp4");
    std::fs::write(&partial, prefix).unwrap();
    let root = "https://v030.test/list.m3u8";
    let playlist = "#EXTM3U\n#EXT-X-TARGETDURATION:8\n#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:7.0,\nsegment.ts\n#EXTINF:7.0,\nsegment.ts\n#EXT-X-ENDLIST\n";
    let source = MemorySource::new().text(root, playlist).segment(
        "https://v030.test/segment.ts",
        include_bytes!("fixtures/h264_aac_fhd.ts").to_vec(),
    );
    let cancel = Arc::new(Cancel::default());
    let token = cancel.clone();
    let result = transmux_hls_to_mp4_async(
        HlsInput::custom(
            Arc::new(LegacySource {
                source,
                reads: std::sync::atomic::AtomicUsize::new(0),
            }),
            SourceLocation::Url(url::Url::parse(root).unwrap()),
        ),
        &output,
        TransmuxOptions {
            output_format: OutputFormat::StreamingMp4,
            resume: Some(checkpoint),
            cancel: Some(cancel),
            on_progress: Some(Arc::new(move |p| {
                assert_eq!(p.completed_segments, 2);
                token.0.store(true, Ordering::Release);
            })),
            ..Default::default()
        },
    )
    .await;
    assert!(matches!(result, Err(Error::Cancelled)));
    let data = std::fs::read(&partial).unwrap();
    assert!(data.len() > prefix.len());
    assert_eq!(&data[..prefix.len()], prefix);
    std::fs::remove_dir_all(dir).unwrap();
}
