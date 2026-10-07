use hls_engine::legacy::*;
use std::sync::{Arc, Mutex};
mod common;
fn input() -> HlsInput {
    let root = "https://media.test/list.m3u8";
    let source = MemorySource::new()
        .text(
            root,
            "#EXTM3U\n#EXTINF:2,\nsegment-0.ts\n#EXTINF:2,\nsegment-1.ts\n#EXT-X-ENDLIST\n",
        )
        .segment(
            "https://media.test/segment-0.ts",
            include_bytes!("fixtures/h264_aac_fhd.ts").to_vec(),
        )
        .segment(
            "https://media.test/segment-1.ts",
            common::continuous_ts(include_bytes!("fixtures/h264_aac_fhd.ts").to_vec(), 1),
        );
    HlsInput::custom(
        Arc::new(source),
        SourceLocation::Url(url::Url::parse(root).unwrap()),
    )
}
#[tokio::test]
async fn independent_runtime_events_include_processing_and_final_completion() {
    for format in [OutputFormat::Mp4, OutputFormat::FragmentedMp4] {
        let events = Arc::new(Mutex::new(Vec::new()));
        let collected = events.clone();
        let mut runtime = TransmuxRuntimeOptions::default();
        runtime.on_event = Some(Arc::new(move |event| collected.lock().unwrap().push(event)));
        let checkpoints = Arc::new(Mutex::new(Vec::new()));
        let committed = checkpoints.clone();
        let mut bytes = Vec::new();
        let report = transmux_hls_to_writer_async_with_runtime(
            input(),
            &mut bytes,
            TransmuxOptions {
                output_format: format,
                on_progress: Some(Arc::new(move |progress| {
                    committed.lock().unwrap().push(progress)
                })),
                ..Default::default()
            },
            runtime,
        )
        .await
        .unwrap();
        if format == OutputFormat::Mp4 {
            assert!(checkpoints.lock().unwrap().is_empty());
        } else {
            let checkpoints = checkpoints.lock().unwrap();
            assert_eq!(checkpoints.len(), 3);
            assert_eq!(checkpoints.last().unwrap().stage, TransmuxStage::Completed);
        }
        let events = events.lock().unwrap();
        assert_eq!(events.first().unwrap().phase, TransmuxPhase::Downloading);
        assert!(events.iter().any(|e| e.phase == TransmuxPhase::Processing));
        if format == OutputFormat::Mp4 {
            assert!(events.iter().any(|e| e.phase == TransmuxPhase::Finalizing));
        }
        assert_eq!(
            events
                .iter()
                .filter(|e| e.phase == TransmuxPhase::Completed)
                .count(),
            1
        );
        assert_eq!(events.last().unwrap().bytes_written, report.bytes_written);
        assert_eq!(report.tracks.len(), 2);
        assert!(report.duration >= 4000);
        assert_eq!(
            report
                .tracks
                .iter()
                .find(|t| t.track_type == TrackType::Video)
                .unwrap()
                .sample_count,
            120
        );
    }
}
#[tokio::test]
async fn runtime_failure_never_emits_completed() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let collected = events.clone();
    let mut runtime = TransmuxRuntimeOptions::default();
    runtime.on_event = Some(Arc::new(move |event| collected.lock().unwrap().push(event)));
    let source = MemorySource::new().text(
        "https://media.test/list.m3u8",
        "#EXTM3U\n#EXTINF:1,\nmissing.ts\n#EXT-X-ENDLIST\n",
    );
    let result = transmux_hls_to_mp4_bytes_with_runtime(
        HlsInput::custom(
            Arc::new(source),
            SourceLocation::Url(url::Url::parse("https://media.test/list.m3u8").unwrap()),
        ),
        TransmuxOptions::default(),
        runtime,
    )
    .await;
    assert!(result.is_err());
    assert!(
        !events
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.phase == TransmuxPhase::Completed)
    );
}
