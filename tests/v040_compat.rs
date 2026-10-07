//! Artifacts produced by the unmodified v0.4.0 tree at commit 20f1593.
#![cfg(all(feature = "serde", not(target_arch = "wasm32")))]
use hls_engine::legacy::*;
use std::sync::Arc;
#[tokio::test]
async fn released_v040_fmp4_checkpoint_resumes_with_legacy_timescales() {
    let directory = std::env::temp_dir().join(format!("hls-v040-compat-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let output = directory.join("output.fmp4");
    let prefix = include_bytes!("fixtures/v040_prefix.fmp4");
    std::fs::write(&output, prefix).unwrap();
    let checkpoint: TransmuxResumeState =
        serde_json::from_slice(include_bytes!("fixtures/v040_downloading.json")).unwrap();
    let root = "https://v040.test/list.m3u8";
    let source = MemorySource::new().text(root, "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:2,\nlist0.m4s\n#EXTINF:2,\nlist1.m4s\n#EXT-X-ENDLIST\n")
        .segment("https://v040.test/init.mp4", include_bytes!("fixtures/v040_init.fmp4").to_vec())
        .segment("https://v040.test/list0.m4s", include_bytes!("fixtures/v040_0.m4s").to_vec())
        .segment("https://v040.test/list1.m4s", include_bytes!("fixtures/v040_1.m4s").to_vec());
    let report = transmux_hls_to_mp4_async(
        HlsInput::custom(
            Arc::new(source),
            SourceLocation::Url(url::Url::parse(root).unwrap()),
        ),
        &output,
        TransmuxOptions {
            output_format: OutputFormat::FragmentedMp4,
            resume: Some(checkpoint),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(report.tracks.len(), 2);
    let video = report
        .tracks
        .iter()
        .find(|track| track.track_type == TrackType::Video)
        .unwrap();
    assert_eq!(video.timescale, 90_000);
    assert_eq!(video.sample_count, 120);
    assert!(report.duration >= 4000);
    assert_eq!(&std::fs::read(&output).unwrap()[..prefix.len()], prefix);
    std::fs::remove_dir_all(directory).unwrap();
}
