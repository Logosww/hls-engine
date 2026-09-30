//! Explicit v0.4.0 public literals and exhaustive matches must still compile.
use hls_transmux::*;
#[test]
fn v040_public_interface_remains_source_compatible() {
    let options = TransmuxOptions {
        variant: None,
        output_format: OutputFormat::FragmentedMp4,
        finalize_backend: FinalizeBackend::Native,
        on_progress: None,
        cancel: None,
        resume: None,
        write_mfra: true,
        checkpoint_durability: CheckpointDurability::Flush,
    };
    let checkpoint = TransmuxResumeState {
        schema_version: 1,
        stage: TransmuxStage::Downloading,
        completed_segments: 1,
        total_segments: 2,
        bytes_written: 1,
        next_sequence: 2,
        global_base_dts_90k: 0,
        input_digest: [1; 32],
        init_digest: [2; 32],
        output_format: options.output_format,
        write_mfra: true,
        duration_ms: 1000,
    };
    let progress = TransmuxProgress {
        stage: TransmuxStage::Downloading,
        total_segments: 2,
        completed_segments: 1,
        downloaded_bytes: 1,
        bytes_written: 1,
        current_segment_index: 0,
        resume: checkpoint,
    };
    match progress.stage {
        TransmuxStage::Downloading | TransmuxStage::Finalizing | TransmuxStage::Completed => {}
    }
    let error = Error::Cancelled;
    match error {
        Error::Io(_)
        | Error::Http(_)
        | Error::InvalidInput(_)
        | Error::Unsupported(_)
        | Error::Bitstream(_)
        | Error::Muxing(_)
        | Error::Cancelled => {}
    }
    let track = TrackInfo {
        track_type: TrackType::Video,
        codec: Codec::Avc,
        timescale: 90_000,
        duration: 3000,
        sample_count: 1,
        width: Some(320),
        height: Some(180),
        sample_rate: None,
        channel_count: None,
    };
    let _report = TransmuxReport {
        segment_count: 1,
        tracks: vec![track],
        duration: 33,
        duration_timescale: 1000,
        bytes_written: 1,
    };
}
