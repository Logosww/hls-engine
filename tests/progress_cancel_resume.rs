//! PHASE 4 tests: per-segment progress callback, cooperative cancellation,
//! and resume from checkpoint. Uses a mock `Source` that returns a fixed
//! multi-segment playlist backed by the in-repo H.264+AAC TS fixture.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use hls_transmux::{
    ByteRange, CancelToken, Error, HlsInput, OutputFormat, Source, SourceLocation, TextResource,
    TransmuxOptions, TransmuxProgress, TransmuxResumeState, transmux_hls_to_mp4_async,
};

/// Reads the in-repo H.264 + AAC-LC TS fixture bytes.
fn fixture_bytes() -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("h264_aac_fhd.ts");
    std::fs::read(&path).expect("fixture should exist")
}

fn temp_dir(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("hls-transmux-phase4-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Builds a media playlist string with `count` segments. All segment URIs are
/// identical ("segment.ts") — the mock Source returns the same fixture bytes
/// regardless of which segment is requested.
fn playlist_with(count: usize) -> String {
    let mut s = String::from("#EXTM3U\n#EXT-X-TARGETDURATION:8\n#EXT-X-MEDIA-SEQUENCE:0\n");
    for _ in 0..count {
        s.push_str("#EXTINF:7.0,\nsegment.ts\n");
    }
    s.push_str("#EXT-X-ENDLIST\n");
    s
}

/// Mock `Source` that returns a fixed playlist text and fixed segment bytes.
/// Used to drive the transmux pipeline without real HTTP or filesystem I/O.
#[derive(Debug)]
struct MockSource {
    playlist: String,
    segment_bytes: Arc<Vec<u8>>,
}

impl Source for MockSource {
    fn read_text<'a>(
        &'a self,
        _location: &'a SourceLocation,
    ) -> Pin<Box<dyn Future<Output = hls_transmux::Result<TextResource>> + Send + 'a>> {
        let text = self.playlist.clone();
        Box::pin(async move {
            Ok(TextResource {
                content: text,
                location: SourceLocation::File(PathBuf::from("playlist.m3u8")),
            })
        })
    }

    fn read_bytes<'a>(
        &'a self,
        _location: &'a SourceLocation,
        _range: Option<&'a ByteRange>,
    ) -> Pin<Box<dyn Future<Output = hls_transmux::Result<Vec<u8>>> + Send + 'a>> {
        let bytes = (*self.segment_bytes).clone();
        Box::pin(async move { Ok(bytes) })
    }
}

/// Builds an `HlsInput::custom` backed by a `MockSource` with `segment_count`
/// segments, all returning the same fixture bytes.
fn mock_input(segment_count: usize) -> HlsInput {
    let source = MockSource {
        playlist: playlist_with(segment_count),
        segment_bytes: Arc::new(fixture_bytes()),
    };
    HlsInput::custom(
        Arc::new(source),
        SourceLocation::File(PathBuf::from("playlist.m3u8")),
    )
}

/// Test `CancelToken` backed by an `AtomicBool`. `cancelled()` returns a
/// pending future — tests exercise cancellation via `is_cancelled()` only.
#[derive(Debug, Default)]
struct TestCancelToken {
    flag: Arc<AtomicBool>,
}

impl TestCancelToken {
    fn trigger(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }
}

impl CancelToken for TestCancelToken {
    fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}

/// Zeros out `creation_time` and `modification_time` fields in `mvhd` and
/// `mdhd` and `tkhd` boxes within the `moov` box. These fields use wall-clock time
/// (`SystemTime::now()` in `mp4.rs`), so they differ between outputs
/// produced at different times. Normalizing them allows byte-level
/// comparison of structurally identical files.
fn normalize_moov_timestamps(data: &mut [u8]) {
    walk_and_zero_timestamps(data, 0, data.len());
}

fn walk_and_zero_timestamps(data: &mut [u8], start: usize, end: usize) {
    let mut offset = start;
    while offset + 8 <= end {
        let size = u32::from_be_bytes(data[offset..offset + 4].try_into().unwrap());
        let (size, header) = if size == 1 {
            if offset + 16 > end {
                break;
            }
            let Ok(size) = usize::try_from(u64::from_be_bytes(
                data[offset + 8..offset + 16].try_into().unwrap(),
            )) else {
                break;
            };
            (size, 16)
        } else {
            (size as usize, 8)
        };
        let Some(box_end) = offset
            .checked_add(size)
            .filter(|&box_end| size >= header && box_end <= end)
        else {
            break;
        };
        let payload = offset + header;
        let btype = &data[offset + 4..offset + 8];
        match btype {
            b"mvhd" | b"mdhd" | b"tkhd" => {
                // FullBox fields precede two u32 (v0) or two u64 (v1) timestamps.
                if payload + 4 <= box_end {
                    let width = match data[payload] {
                        0 => 8,
                        1 => 16,
                        _ => 0,
                    };
                    if payload + 4 + width <= box_end {
                        data[payload + 4..payload + 4 + width].fill(0);
                    }
                }
            }
            b"moov" | b"trak" | b"mdia" => {
                walk_and_zero_timestamps(data, payload, box_end);
            }
            _ => {}
        }
        offset = box_end;
    }
}

#[test]
fn timestamp_normalization_handles_tkhd_versions_and_extended_headers() {
    fn boxed(kind: &[u8; 4], payload: &[u8], extended: bool) -> Vec<u8> {
        let mut out = Vec::new();
        if extended {
            out.extend_from_slice(&1u32.to_be_bytes());
            out.extend_from_slice(kind);
            out.extend_from_slice(&(payload.len() as u64 + 16).to_be_bytes());
        } else {
            out.extend_from_slice(&(payload.len() as u32 + 8).to_be_bytes());
            out.extend_from_slice(kind);
        }
        out.extend_from_slice(payload);
        out
    }
    for version in [0, 1] {
        for extended in [false, true] {
            let make_file = |clock_byte| {
                let mut payload = vec![version, 0, 0, 7];
                payload.extend(vec![clock_byte; if version == 0 { 8 } else { 16 }]);
                // Non-clock fields must survive normalization.
                payload.extend_from_slice(&[9, 8, 7, 6]);
                let tkhd = boxed(b"tkhd", &payload, extended);
                let trak = boxed(b"trak", &tkhd, extended);
                boxed(b"moov", &trak, extended)
            };
            let mut first = make_file(42);
            let mut second = make_file(43);
            assert_ne!(first, second);
            normalize_moov_timestamps(&mut first);
            normalize_moov_timestamps(&mut second);
            assert_eq!(first, second);
            assert_eq!(&first[first.len() - 4..], &[9, 8, 7, 6]);
        }
    }
}

// ---------------------------------------------------------------------------
// Test 1: progress callback fires once per segment (§9.2)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn progress_fires_per_segment() {
    let dir = temp_dir("progress");
    let output = dir.join("output.fmp4");

    let events: Arc<Mutex<Vec<TransmuxProgress>>> = Arc::new(Mutex::new(Vec::new()));
    let events_cb = events.clone();

    let opts = TransmuxOptions {
        output_format: OutputFormat::FragmentedMp4,
        on_progress: Some(Arc::new(move |p: TransmuxProgress| {
            if p.stage != hls_transmux::TransmuxStage::Completed {
                events_cb.lock().unwrap().push(p);
            }
        })),
        ..Default::default()
    };

    transmux_hls_to_mp4_async(mock_input(3), &output, opts)
        .await
        .expect("transmux should succeed");

    let evs = events.lock().unwrap();
    assert_eq!(evs.len(), 3, "callback should fire once per segment");
    assert_eq!(evs[0].completed_segments, 1);
    assert_eq!(evs[1].completed_segments, 2);
    assert_eq!(evs[2].completed_segments, 3);
    assert_eq!(evs[2].total_segments, 3);
    // Monotonically non-decreasing downloaded_bytes / bytes_written.
    assert!(evs[1].downloaded_bytes >= evs[0].downloaded_bytes);
    assert!(evs[2].downloaded_bytes >= evs[1].downloaded_bytes);
    assert!(evs[1].bytes_written > evs[0].bytes_written);
    assert!(evs[2].bytes_written > evs[1].bytes_written);
    // current_segment_index tracks the just-completed segment.
    assert_eq!(evs[0].current_segment_index, 0);
    assert_eq!(evs[1].current_segment_index, 1);
    assert_eq!(evs[2].current_segment_index, 2);
    // Resume snapshot fields are populated.
    assert_eq!(evs[2].resume.completed_segments, 3);
    assert!(evs[2].resume.bytes_written > 0);
    assert!(evs[2].resume.next_sequence > 0);
}

// ---------------------------------------------------------------------------
// Test 2: cancel returns Error::Cancelled and retains .partial.mp4 (§9.3)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cancel_returns_cancelled_error() {
    let dir = temp_dir("cancel");
    let output = dir.join("output.mp4");
    let partial = dir.join("output.partial.mp4");

    let token = Arc::new(TestCancelToken::default());
    let token_for_cb = token.clone();

    let opts = TransmuxOptions {
        output_format: OutputFormat::StreamingMp4,
        cancel: Some(token),
        on_progress: Some(Arc::new(move |p: TransmuxProgress| {
            // Trigger cancel after 2 segments complete. The next loop
            // iteration's check_cancel() will return Error::Cancelled.
            if p.completed_segments >= 2 {
                token_for_cb.trigger();
            }
        })),
        ..Default::default()
    };

    let err = transmux_hls_to_mp4_async(mock_input(3), &output, opts)
        .await
        .expect_err("should be cancelled");
    assert!(
        matches!(err, Error::Cancelled),
        "expected Error::Cancelled, got {err:?}"
    );

    // .partial.mp4 should be retained on cancel (StreamingMp4 stage 1 keeps
    // it so the caller can resume).
    assert!(
        partial.exists(),
        ".partial.mp4 should be retained on cancel"
    );
    let bytes = std::fs::read(&partial).expect("partial should be readable");
    assert!(
        bytes.len() >= 8 && &bytes[4..8] == b"ftyp",
        "partial file should start with ftyp box"
    );
}

// ---------------------------------------------------------------------------
// Test 3: resume produces byte-identical output (§9.4)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resume_produces_identical_output() {
    let dir = temp_dir("resume");
    let full_output = dir.join("full.fmp4");
    let resume_output = dir.join("resume.fmp4");

    // (a) Run to completion → C_full (includes mfra).
    transmux_hls_to_mp4_async(
        mock_input(3),
        &full_output,
        TransmuxOptions {
            output_format: OutputFormat::FragmentedMp4,
            ..Default::default()
        },
    )
    .await
    .expect("full run should succeed");
    let c_full = std::fs::read(&full_output).expect("full output should exist");

    // (b) Cancel after 2 segments → capture resume snapshot R.
    let token = Arc::new(TestCancelToken::default());
    let token_for_cb = token.clone();
    let snapshot: Arc<Mutex<Option<TransmuxResumeState>>> = Arc::new(Mutex::new(None));
    let snapshot_cb = snapshot.clone();

    let opts = TransmuxOptions {
        output_format: OutputFormat::FragmentedMp4,
        cancel: Some(token),
        on_progress: Some(Arc::new(move |p: TransmuxProgress| {
            *snapshot_cb.lock().unwrap() = Some(p.resume.clone());
            if p.completed_segments >= 2 {
                token_for_cb.trigger();
            }
        })),
        ..Default::default()
    };
    let err = transmux_hls_to_mp4_async(mock_input(3), &resume_output, opts)
        .await
        .expect_err("should be cancelled");
    assert!(matches!(err, Error::Cancelled));

    let r = snapshot
        .lock()
        .unwrap()
        .clone()
        .expect("should have a resume snapshot");
    assert_eq!(
        r.completed_segments, 2,
        "snapshot should record 2 completed segments"
    );

    // (c) Resume from R → C_resumed (omits mfra).
    let resumed_events: Arc<Mutex<Vec<TransmuxProgress>>> = Arc::new(Mutex::new(Vec::new()));
    let re_cb = resumed_events.clone();
    let opts = TransmuxOptions {
        output_format: OutputFormat::FragmentedMp4,
        resume: Some(r),
        on_progress: Some(Arc::new(move |p: TransmuxProgress| {
            if p.stage != hls_transmux::TransmuxStage::Completed {
                re_cb.lock().unwrap().push(p);
            }
        })),
        ..Default::default()
    };
    transmux_hls_to_mp4_async(mock_input(3), &resume_output, opts)
        .await
        .expect("resume should succeed");
    let c_resumed = std::fs::read(&resume_output).expect("resumed output should exist");

    // (d) Normalize wall-clock timestamps, then compare byte-for-byte.
    // mvhd/mdhd creation_time and modification_time use SystemTime::now(), so
    // they differ between runs — normalize them to zero before byte comparison.
    // Both outputs include a complete mfra box: the resumed run rebuilds
    // historical tfra entries by scanning the existing .partial.mp4's moof
    // boxes (plan §5.5 enhancement), so the mfra boxes are byte-identical.
    let mut c_full_norm = c_full.clone();
    let mut c_resumed_norm = c_resumed.clone();
    normalize_moov_timestamps(&mut c_full_norm);
    normalize_moov_timestamps(&mut c_resumed_norm);
    assert_eq!(
        c_resumed_norm.as_slice(),
        c_full_norm.as_slice(),
        "resumed output should be byte-identical to full output (including \
         mfra, after wall-clock timestamp normalization)"
    );

    // (e) First (and only) resumed progress event: current_segment_index == K
    // (the resume start index), completed_segments == K + 1.
    let re = resumed_events.lock().unwrap();
    assert_eq!(
        re.len(),
        1,
        "resumed run processes 1 segment, should emit 1 progress event"
    );
    assert_eq!(
        re[0].current_segment_index, 2,
        "first resumed segment index"
    );
    assert_eq!(
        re[0].completed_segments, 3,
        "completed_segments after resume"
    );
}

// ---------------------------------------------------------------------------
// Test 4: resume boundary — already complete (§9.5)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resume_boundary_already_complete() {
    let dir = temp_dir("resume-complete");
    let output = dir.join("output.fmp4");

    let opts = TransmuxOptions {
        output_format: OutputFormat::FragmentedMp4,
        resume: Some(TransmuxResumeState {
            completed_segments: 3, // == segments.len()
            bytes_written: 1000,
            next_sequence: 4,
            global_base_dts_90k: 0,
            ..Default::default()
        }),
        ..Default::default()
    };

    let err = transmux_hls_to_mp4_async(mock_input(3), &output, opts)
        .await
        .expect_err("should reject already-complete resume");
    assert!(
        matches!(err, Error::InvalidInput(_)),
        "expected Error::InvalidInput, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 5: Mp4 + resume is rejected (user decision, §5.4 deviation)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mp4_with_resume_rejected() {
    let dir = temp_dir("mp4-resume");
    let output = dir.join("output.mp4");

    let opts = TransmuxOptions {
        output_format: OutputFormat::Mp4,
        resume: Some(TransmuxResumeState {
            completed_segments: 1,
            bytes_written: 1000,
            next_sequence: 2,
            global_base_dts_90k: 0,
            ..Default::default()
        }),
        ..Default::default()
    };

    let err = transmux_hls_to_mp4_async(mock_input(3), &output, opts)
        .await
        .expect_err("should reject Mp4 + resume");
    assert!(
        matches!(err, Error::InvalidInput(_)),
        "expected Error::InvalidInput, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 6: default path (no hooks) is unchanged (§9.6)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn default_path_unchanged() {
    let dir = temp_dir("default");
    let output = dir.join("output.mp4");

    transmux_hls_to_mp4_async(mock_input(1), &output, TransmuxOptions::default())
        .await
        .expect("default path should succeed");

    let bytes = std::fs::read(&output).expect("output should exist");
    assert!(
        bytes.len() >= 8 && &bytes[4..8] == b"ftyp",
        "output should start with ftyp box"
    );
}

// v0.3 failure injection: committed prefixes survive crashes and failed finalize.
async fn paused_file(
    name: &str,
    format: OutputFormat,
    after: usize,
) -> (PathBuf, TransmuxResumeState) {
    paused_file_with_count(name, format, after, 3).await
}

async fn paused_file_with_count(
    name: &str,
    format: OutputFormat,
    after: usize,
    count: usize,
) -> (PathBuf, TransmuxResumeState) {
    let output = temp_dir(name).join("output.mp4");
    let token = Arc::new(TestCancelToken::default());
    let cancel = token.clone();
    let saved = Arc::new(Mutex::new(None));
    let snapshot = saved.clone();
    let result = transmux_hls_to_mp4_async(
        mock_input(count),
        &output,
        TransmuxOptions {
            output_format: format,
            cancel: Some(token),
            checkpoint_durability: hls_transmux::CheckpointDurability::SyncAll,
            on_progress: Some(Arc::new(move |p| {
                *snapshot.lock().unwrap() = Some(p.resume);
                if p.completed_segments == after {
                    cancel.flag.store(true, Ordering::SeqCst);
                }
            })),
            ..Default::default()
        },
    )
    .await;
    assert!(matches!(result, Err(Error::Cancelled)));
    let state = saved.lock().unwrap().clone().unwrap();
    (output, state)
}

#[tokio::test]
async fn recovery_truncates_half_and_complete_uncommitted_fragments() {
    for complete in [false, true] {
        let (output, checkpoint) = paused_file(
            if complete { "tail-full" } else { "tail-half" },
            OutputFormat::FragmentedMp4,
            1,
        )
        .await;
        let committed = std::fs::read(&output).unwrap();
        let full = output.with_extension("reference.mp4");
        transmux_hls_to_mp4_async(
            mock_input(3),
            &full,
            TransmuxOptions {
                output_format: OutputFormat::FragmentedMp4,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let reference = std::fs::read(&full).unwrap();
        let tail = &reference[checkpoint.bytes_written as usize..];
        let end = if complete { tail.len() } else { tail.len() / 2 };
        let mut crashed = committed;
        crashed.extend_from_slice(&tail[..end]);
        std::fs::write(&output, crashed).unwrap();
        transmux_hls_to_mp4_async(
            mock_input(3),
            &output,
            TransmuxOptions {
                output_format: OutputFormat::FragmentedMp4,
                resume: Some(checkpoint),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let mut actual = std::fs::read(&output).unwrap();
        let mut expected = reference;
        normalize_moov_timestamps(&mut actual);
        normalize_moov_timestamps(&mut expected);
        assert_eq!(actual, expected);
    }
}

#[tokio::test]
async fn invalid_recovery_never_changes_file() {
    let (output, checkpoint) =
        paused_file("invalid-recovery", OutputFormat::FragmentedMp4, 1).await;
    let original = std::fs::read(&output).unwrap();
    for case in 0..8 {
        let mut r = checkpoint.clone();
        let mut bytes = original.clone();
        match case {
            0 => {
                bytes.pop();
            }
            1 => {
                r.bytes_written -= 1;
            }
            2 => {
                r.next_sequence += 1;
            }
            3 => {
                r.input_digest[0] ^= 1;
            }
            4 => {
                r.init_digest[0] ^= 1;
            }
            5 => {
                r.write_mfra = false;
            }
            6 => {
                r.schema_version = 99;
            }
            _ => {
                let at = bytes.windows(4).position(|b| b == b"mfhd").unwrap();
                bytes[at + 8..at + 12].copy_from_slice(&2u32.to_be_bytes());
            }
        }
        std::fs::write(&output, &bytes).unwrap();
        let result = transmux_hls_to_mp4_async(
            mock_input(3),
            &output,
            TransmuxOptions {
                output_format: OutputFormat::FragmentedMp4,
                resume: Some(r),
                ..Default::default()
            },
        )
        .await;
        assert!(result.is_err(), "case {case}");
        assert_eq!(std::fs::read(&output).unwrap(), bytes, "case {case}");
    }
    std::fs::write(&output, &original).unwrap();
    assert!(
        transmux_hls_to_mp4_async(
            mock_input(4),
            &output,
            TransmuxOptions {
                output_format: OutputFormat::FragmentedMp4,
                resume: Some(checkpoint),
                ..Default::default()
            }
        )
        .await
        .is_err()
    );
    assert_eq!(std::fs::read(output).unwrap(), original);
}

#[derive(Debug)]
struct NoNetwork;
impl Source for NoNetwork {
    fn read_text<'a>(
        &'a self,
        _: &'a SourceLocation,
    ) -> Pin<Box<dyn Future<Output = hls_transmux::Result<TextResource>> + Send + 'a>> {
        panic!("finalize must not read playlist")
    }
    fn read_bytes<'a>(
        &'a self,
        _: &'a SourceLocation,
        _: Option<&'a ByteRange>,
    ) -> Pin<Box<dyn Future<Output = hls_transmux::Result<Vec<u8>>> + Send + 'a>> {
        panic!("finalize must not read media")
    }
}

#[tokio::test]
async fn finalize_failure_preserves_partial_and_retry_uses_zero_network() {
    let (output, checkpoint) =
        paused_file_with_count("finalize-retry", OutputFormat::StreamingMp4, 1, 1).await;
    assert_eq!(checkpoint.stage, hls_transmux::TransmuxStage::Finalizing);
    let partial = output.with_file_name("output.partial.mp4");
    let original = std::fs::read(&partial).unwrap();
    // An existing complete target remains unchanged on validation failure.
    std::fs::write(&output, b"previous complete target").unwrap();
    let mut invalid = checkpoint.clone();
    invalid.init_digest[0] ^= 1;
    assert!(
        hls_transmux::finalize_partial_mp4_async(
            &partial,
            &output,
            invalid,
            TransmuxOptions {
                output_format: OutputFormat::StreamingMp4,
                ..Default::default()
            }
        )
        .await
        .is_err()
    );
    assert_eq!(std::fs::read(&output).unwrap(), b"previous complete target");
    assert_eq!(std::fs::read(&partial).unwrap(), original);
    // Real replacement failure: rename cannot replace a directory.
    let blocked = output.with_extension("directory");
    std::fs::create_dir_all(&blocked).unwrap();
    assert!(
        hls_transmux::finalize_partial_mp4_async(
            &partial,
            &blocked,
            checkpoint.clone(),
            TransmuxOptions {
                output_format: OutputFormat::StreamingMp4,
                ..Default::default()
            }
        )
        .await
        .is_err()
    );
    assert_eq!(std::fs::read(&partial).unwrap(), original);
    let mut with_tail = original.clone();
    with_tail.extend_from_slice(b"uncommitted garbage");
    std::fs::write(&partial, &with_tail).unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let saved = events.clone();
    let report = transmux_hls_to_mp4_async(
        HlsInput::custom(
            Arc::new(NoNetwork),
            SourceLocation::File("unavailable.m3u8".into()),
        ),
        &output,
        TransmuxOptions {
            output_format: OutputFormat::StreamingMp4,
            resume: Some(checkpoint.clone()),
            on_progress: Some(Arc::new(move |p| saved.lock().unwrap().push(p))),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(report.segment_count, 1);
    assert!(!partial.exists());
    assert_eq!(
        events.lock().unwrap().last().unwrap().stage,
        hls_transmux::TransmuxStage::Completed
    );
    let reference = output.with_extension("reference.mp4");
    transmux_hls_to_mp4_async(mock_input(1), &reference, TransmuxOptions::default())
        .await
        .unwrap();
    let mut expected = std::fs::read(reference).unwrap();
    let mut actual = std::fs::read(&output).unwrap();
    normalize_moov_timestamps(&mut expected);
    normalize_moov_timestamps(&mut actual);
    // This fixture has the same stored sample durations as the batch path.
    assert_eq!(actual, expected);
    // Repeated stale finalize checkpoints cannot damage the completed target.
    assert!(
        hls_transmux::finalize_partial_mp4_async(
            &partial,
            &output,
            checkpoint,
            TransmuxOptions {
                output_format: OutputFormat::StreamingMp4,
                ..Default::default()
            }
        )
        .await
        .is_err()
    );
    let mut untouched = std::fs::read(output).unwrap();
    normalize_moov_timestamps(&mut untouched);
    assert_eq!(untouched, actual);
}

#[tokio::test]
async fn all_segments_downloaded_fragmented_checkpoint_can_finish_index() {
    let (output, checkpoint) = paused_file("index-retry", OutputFormat::FragmentedMp4, 3).await;
    let options = TransmuxOptions {
        output_format: OutputFormat::FragmentedMp4,
        resume: Some(checkpoint.clone()),
        ..Default::default()
    };
    transmux_hls_to_mp4_async(mock_input(3), &output, options.clone())
        .await
        .unwrap();
    let once = std::fs::read(&output).unwrap();
    transmux_hls_to_mp4_async(mock_input(3), &output, options)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(output).unwrap(),
        once,
        "index must not be duplicated"
    );
}

#[cfg(feature = "serde")]
#[tokio::test]
async fn checkpoint_serde_roundtrip_and_legacy_rejection() {
    let (output, checkpoint) =
        paused_file("serde-checkpoint", OutputFormat::FragmentedMp4, 1).await;
    let json = serde_json::to_string(&checkpoint).unwrap();
    assert_eq!(
        serde_json::from_str::<TransmuxResumeState>(&json).unwrap(),
        checkpoint
    );
    let legacy: TransmuxResumeState = serde_json::from_str(r#"{"completed_segments":1,"bytes_written":1000,"next_sequence":2,"global_base_dts_90k":0}"#).unwrap();
    let bytes = std::fs::read(&output).unwrap();
    let err = transmux_hls_to_mp4_async(
        mock_input(3),
        &output,
        TransmuxOptions {
            output_format: OutputFormat::FragmentedMp4,
            resume: Some(legacy),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("schema"));
    assert_eq!(std::fs::read(output).unwrap(), bytes);
}

#[derive(Debug)]
struct CancelDuringFinalize(std::sync::atomic::AtomicUsize);
impl CancelToken for CancelDuringFinalize {
    fn is_cancelled(&self) -> bool {
        self.0.fetch_add(1, Ordering::SeqCst) >= 50
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}

#[tokio::test]
async fn finalize_cpu_cancellation_waits_for_worker_and_preserves_target() {
    let (output, checkpoint) =
        paused_file_with_count("finalize-cancel", OutputFormat::StreamingMp4, 1, 1).await;
    let partial = output.with_file_name("output.partial.mp4");
    let original = std::fs::read(&partial).unwrap();
    std::fs::write(&output, b"complete target").unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        hls_transmux::finalize_partial_mp4_async(
            &partial,
            &output,
            checkpoint,
            TransmuxOptions {
                output_format: OutputFormat::StreamingMp4,
                cancel: Some(Arc::new(CancelDuringFinalize(
                    std::sync::atomic::AtomicUsize::new(0),
                ))),
                ..Default::default()
            },
        ),
    )
    .await
    .unwrap();
    assert!(matches!(result, Err(Error::Cancelled)));
    assert_eq!(std::fs::read(output).unwrap(), b"complete target");
    assert_eq!(std::fs::read(partial).unwrap(), original);
}

#[derive(Debug)]
struct FailSecond {
    reads: std::sync::atomic::AtomicUsize,
}
impl Source for FailSecond {
    fn read_text<'a>(
        &'a self,
        location: &'a SourceLocation,
    ) -> Pin<Box<dyn Future<Output = hls_transmux::Result<TextResource>> + Send + 'a>> {
        Box::pin(async move {
            Ok(TextResource {
                content: playlist_with(3),
                location: location.clone(),
            })
        })
    }
    fn read_bytes<'a>(
        &'a self,
        _: &'a SourceLocation,
        _: Option<&'a ByteRange>,
    ) -> Pin<Box<dyn Future<Output = hls_transmux::Result<Vec<u8>>> + Send + 'a>> {
        Box::pin(async move {
            if self.reads.fetch_add(1, Ordering::SeqCst) == 1 {
                Err(Error::Http("injected network failure".into()))
            } else {
                Ok(fixture_bytes())
            }
        })
    }
}

#[tokio::test]
async fn network_failure_preserves_streaming_partial_for_resume() {
    let output = temp_dir("network-failure").join("output.mp4");
    std::fs::write(&output, b"complete target").unwrap();
    let saved = Arc::new(Mutex::new(None));
    let snapshot = saved.clone();
    let result = transmux_hls_to_mp4_async(
        HlsInput::custom(
            Arc::new(FailSecond {
                reads: std::sync::atomic::AtomicUsize::new(0),
            }),
            SourceLocation::File("playlist.m3u8".into()),
        ),
        &output,
        TransmuxOptions {
            output_format: OutputFormat::StreamingMp4,
            on_progress: Some(Arc::new(move |p| {
                *snapshot.lock().unwrap() = Some(p.resume)
            })),
            ..Default::default()
        },
    )
    .await;
    assert!(matches!(result, Err(Error::Http(_))));
    assert_eq!(
        std::fs::read(output.with_file_name("output.partial.mp4"))
            .unwrap()
            .len() as u64,
        saved.lock().unwrap().as_ref().unwrap().bytes_written
    );
    assert_eq!(std::fs::read(output).unwrap(), b"complete target");
}

#[tokio::test]
async fn streaming_completed_progress_matches_final_output() {
    let output = temp_dir("completed-progress").join("output.mp4");
    let events = Arc::new(Mutex::new(Vec::new()));
    let saved = events.clone();
    let report = transmux_hls_to_mp4_async(
        mock_input(1),
        &output,
        TransmuxOptions {
            output_format: OutputFormat::StreamingMp4,
            on_progress: Some(Arc::new(move |p| saved.lock().unwrap().push(p))),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let events = events.lock().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].stage, hls_transmux::TransmuxStage::Finalizing);
    let completed = &events[1];
    assert_eq!(completed.stage, hls_transmux::TransmuxStage::Completed);
    assert_eq!(completed.bytes_written, report.bytes_written);
    assert_eq!(completed.resume.bytes_written, report.bytes_written);
    assert_eq!(completed.resume.duration_ms, report.duration);
    assert_eq!(completed.downloaded_bytes, events[0].downloaded_bytes);
}
