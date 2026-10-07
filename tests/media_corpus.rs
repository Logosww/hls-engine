//! Retained external FFmpeg fixtures; counts/codecs come from FFprobe, not our muxer.
#![cfg(not(target_arch = "wasm32"))]

use std::{fs, path::PathBuf, sync::Arc};

use hls_engine::legacy::{
    Codec, HlsInput, MemorySource, OutputFormat, SourceLocation, TrackType, TransmuxOptions,
    transmux_hls_to_mp4_async,
};
use sha2::{Digest, Sha256};

#[tokio::test]
async fn retained_media_matches_external_reference_and_support_boundaries() {
    let corpus = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media");
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(corpus.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["schema_version"], 1);
    let cases = manifest["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 14);
    let directory = std::env::temp_dir().join(format!("hls-media-corpus-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();

    for case in cases {
        let name = case["name"].as_str().unwrap();
        let folder = corpus.join(name);
        let playlist = folder.join("input.m3u8");
        let text = fs::read_to_string(&playlist).unwrap();
        let segments = text.lines().filter(|line| !line.starts_with('#')).count();
        assert!(segments >= 3, "{name}");
        let mut source = MemorySource::new().text(playlist.to_str().unwrap(), text);
        for (file, digest) in case["sha256"].as_object().unwrap() {
            let path = folder.join(file);
            let bytes = fs::read(&path).unwrap();
            assert_eq!(
                format!("{:x}", Sha256::digest(&bytes)),
                digest.as_str().unwrap(),
                "{name}/{file}: fixture bytes differ from the external manifest"
            );
            if file != "input.m3u8" {
                source = source.segment(path.to_str().unwrap(), bytes);
            }
        }
        let source = Arc::new(source);
        for (index, format) in [
            OutputFormat::FragmentedMp4,
            OutputFormat::StreamingMp4,
            OutputFormat::Mp4,
        ]
        .into_iter()
        .enumerate()
        {
            let output = directory.join(format!("{name}-{index}.mp4"));
            let result = transmux_hls_to_mp4_async(
                HlsInput::custom(source.clone(), SourceLocation::File(playlist.clone())),
                &output,
                TransmuxOptions {
                    output_format: format,
                    ..Default::default()
                },
            )
            .await;
            let report = result.unwrap_or_else(|error| panic!("{name}/{format:?}: {error}"));
            let reference = case["reference"].as_object().unwrap();
            assert_eq!(report.segment_count, segments, "{name}");
            assert_eq!(report.tracks.len(), reference.len(), "{name}");
            assert_eq!(report.bytes_written, fs::metadata(&output).unwrap().len());
            for track in &report.tracks {
                let kind = match track.track_type {
                    TrackType::Video => "video",
                    TrackType::Audio => "audio",
                };
                let expected = &reference[kind];
                assert_eq!(
                    track.sample_count as u64,
                    expected["sample_count"].as_u64().unwrap(),
                    "{name}/{kind}"
                );
                let codec = match expected["codec"].as_str().unwrap() {
                    "h264" => Codec::Avc,
                    "hevc" => Codec::Hevc,
                    "aac" => Codec::Aac,
                    codec => panic!("unexpected external codec: {codec}"),
                };
                assert_eq!(track.codec, codec, "{name}/{kind}");
            }
        }
    }
    fs::remove_dir_all(directory).unwrap();
}
