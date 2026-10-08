//! Independent decoder regression for classic finalization (issue #3).
#![cfg(not(target_arch = "wasm32"))]

use hls_engine::{crypto::key::*, playlist::*, *};
use std::{path::Path, process::Command, sync::Arc};

struct Clock;
impl KeyClock for Clock {
    fn now(&self) -> u64 {
        0
    }
}
struct NoKeys;
impl KeyProvider for NoKeys {
    fn resolve(&self, _: KeyRequest) -> KeyFuture<KeyResolution> {
        Box::pin(async { KeyResolution::Unavailable })
    }
}

fn session() -> EngineSession {
    let root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media/fmp4_avc_negative_cts");
    let input = InputId::new("primary").unwrap();
    let mut source = MemorySource::new();
    for name in ["init.fmp4", "seg0.m4s", "seg1.m4s", "seg2.m4s"] {
        source = source.segment(
            format!("https://fixture.test/{name}"),
            std::fs::read(root.join(name)).unwrap(),
        );
    }
    let keys = KeySession::new(
        "test",
        "test",
        Arc::new(NoKeys),
        Arc::new(Clock),
        KeySessionOptions::default(),
    )
    .unwrap();
    let range = PresentationRange::new(
        MediaTime::new(200, 1000).unwrap(),
        MediaTime::new(1200, 1000).unwrap(),
    )
    .unwrap();
    let engine = EngineSession::new(
        EngineInputs::new(
            EngineInput::new(input.clone(), Arc::new(source)),
            EmbeddedAudio::Keep,
        ),
        keys,
        EngineOptions::default().with_range(range),
    )
    .unwrap();
    let snapshot = parse_playlist_snapshot(
        &TextResource {
            location: SourceLocation::Url(
                url::Url::parse("https://fixture.test/input.m3u8").unwrap(),
            ),
            content: std::fs::read_to_string(root.join("input.m3u8")).unwrap(),
        },
        PlaylistContext::new(input.clone(), 0),
    )
    .unwrap();
    engine.handle().accept_snapshot(&input, &snapshot).unwrap();
    engine.handle().end_input(&input).unwrap();
    engine
}

fn hashes(path: &Path, stream: &str) -> Vec<String> {
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-map",
            stream,
            "-fps_mode",
            "passthrough",
            "-f",
            "framemd5",
            "-",
        ])
        .output()
        .expect("FFmpeg is required for this regression test");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter(|s| !s.starts_with('#') && !s.is_empty())
        .map(|s| s.rsplit(',').next().unwrap().trim().to_string())
        .collect()
}

fn packets(path: &Path, stream: &str) -> serde_json::Value {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            stream,
            "-show_packets",
            "-show_streams",
            "-show_data_hash",
            "sha256",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .expect("FFprobe is required for this regression test");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[tokio::test]
#[ignore = "requires FFmpeg; run explicitly in the FFmpeg CI job"]
async fn classic_and_fragmented_preserve_decoded_payloads_across_gaps() {
    let directory = std::env::temp_dir().join(format!("hls-output-parity-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let fragmented_path = directory.join("fragmented.mp4");
    let (bytes, fragmented_report) = session()
        .into_bytes(8 * 1024 * 1024, OutputFormat::FragmentedMp4)
        .await
        .unwrap();
    std::fs::write(&fragmented_path, bytes).unwrap();
    for format in [OutputFormat::Mp4, OutputFormat::StreamingMp4] {
        let classic_path = directory.join(format!("{format:?}.mp4"));
        let classic_report = session()
            .write_to_file(
                &classic_path,
                FileOutputOptions::default().with_format(format),
            )
            .await
            .unwrap();
        assert_eq!(classic_report.tracks().len(), 2);
        for (a, b) in classic_report
            .tracks()
            .iter()
            .zip(fragmented_report.tracks())
        {
            assert_eq!(a.kind(), b.kind());
            assert_eq!(a.timescale(), b.timescale());
            assert_eq!(a.duration(), b.duration());
            assert_eq!(a.sample_count(), b.sample_count());
            match a.kind() {
                OutputTrackKind::Audio => {
                    assert_eq!(a.sample_count(), 62);
                    assert_eq!(a.duration(), 192512);
                }
                OutputTrackKind::Video => assert_eq!(a.sample_count(), 60),
                _ => panic!("unexpected track kind"),
            }
        }
        for (stream, expected) in [("0:a:0", 62), ("0:v:0", 60)] {
            let classic = hashes(&classic_path, stream);
            let fragmented = hashes(&fragmented_path, stream);
            assert_eq!(fragmented.len(), expected, "{stream}: fixture frame count");
            assert_eq!(
                classic.len(),
                fragmented.len(),
                "{stream}: decoded frame count; outputs in {}",
                directory.display()
            );
            assert_eq!(classic, fragmented, "{stream}: decoded payloads");
            let classic = packets(&classic_path, &stream[2..]);
            let fragmented = packets(&fragmented_path, &stream[2..]);
            assert_eq!(
                classic["streams"][0]["time_base"],
                fragmented["streams"][0]["time_base"]
            );
            let actual = classic["packets"].as_array().unwrap();
            let expected = fragmented["packets"].as_array().unwrap();
            assert_eq!(
                actual.len(),
                expected.len(),
                "{stream}: packet count including discard packets"
            );
            for (a, b) in actual.iter().zip(expected) {
                for field in ["pts", "duration", "data_hash"] {
                    assert_eq!(a[field], b[field], "{stream}: packet {field}");
                }
                if stream == "0:v:0" {
                    assert_eq!(a["dts"], b["dts"], "video decode timestamp");
                }
            }
            if stream == "0:a:0" {
                assert!(
                    expected.windows(2).any(|p| p[1]["pts"].as_i64().unwrap()
                        > p[0]["pts"].as_i64().unwrap() + p[0]["duration"].as_i64().unwrap()),
                    "fixture must contain a real presentation gap"
                );
                for pair in actual.windows(2) {
                    assert_eq!(
                        pair[1]["dts"].as_i64().unwrap(),
                        pair[0]["dts"].as_i64().unwrap() + pair[0]["duration"].as_i64().unwrap(),
                        "AAC decode run stays contiguous"
                    );
                }
            }
        }
    }
    std::fs::remove_dir_all(directory).unwrap();
}
