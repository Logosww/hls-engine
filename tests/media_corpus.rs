//! Retained external FFmpeg fixtures; counts/codecs come from FFprobe, not our muxer.
#![cfg(not(target_arch = "wasm32"))]

use std::{fs, path::PathBuf, sync::Arc};

use hls_engine::*;
mod common;
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
        assert_eq!(
            FileOutputOptions::default().finalization_strategy(),
            FileFinalizationStrategy::Copy
        );
        let mut fragmented_bytes = Vec::new();
        for (index, (format, strategy, reserve)) in [
            (
                OutputFormat::FragmentedMp4,
                FileFinalizationStrategy::Copy,
                0,
            ),
            (
                OutputFormat::StreamingMp4,
                FileFinalizationStrategy::Copy,
                0,
            ),
            (OutputFormat::Mp4, FileFinalizationStrategy::Copy, 0),
            (
                OutputFormat::StreamingMp4,
                FileFinalizationStrategy::InPlace,
                1024 * 1024,
            ),
            (
                OutputFormat::Mp4,
                FileFinalizationStrategy::InPlace,
                1024 * 1024,
            ),
            (
                OutputFormat::StreamingMp4,
                FileFinalizationStrategy::InPlace,
                0,
            ),
            (OutputFormat::Mp4, FileFinalizationStrategy::InPlace, 8),
            (
                OutputFormat::FragmentedMp4,
                FileFinalizationStrategy::InPlace,
                7,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let output = directory.join(format!("{name}-{index}.mp4"));
            let session =
                common::selected_session(source.clone(), SourceLocation::File(playlist.clone()))
                    .await
                    .unwrap();
            let report = session
                .write_to_file(
                    &output,
                    FileOutputOptions::default()
                        .with_format(format)
                        .with_finalization_strategy(strategy)
                        .with_header_reserve_bytes(reserve),
                )
                .await
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            let bytes = fs::read(&output).unwrap();
            let ftyp_size = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
            if index == 0 {
                fragmented_bytes = bytes.clone();
            } else if format != OutputFormat::FragmentedMp4
                && strategy == FileFinalizationStrategy::InPlace
            {
                assert_eq!(&bytes[ftyp_size + 4..ftyp_size + 8], b"moov");
                let moov_size =
                    u32::from_be_bytes(bytes[ftyp_size..ftyp_size + 4].try_into().unwrap())
                        as usize;
                let mut mdat = ftyp_size + moov_size;
                if &bytes[mdat + 4..mdat + 8] == b"free" {
                    mdat += u32::from_be_bytes(bytes[mdat..mdat + 4].try_into().unwrap()) as usize;
                }
                assert_eq!(&bytes[mdat + 4..mdat + 8], b"mdat");
                let front = mdat - ftyp_size;
                assert_eq!(bytes.len(), fragmented_bytes.len() + front);
                assert!(front >= reserve as usize);
                let init_end = ftyp_size
                    + u32::from_be_bytes(
                        fragmented_bytes[ftyp_size..ftyp_size + 4]
                            .try_into()
                            .unwrap(),
                    ) as usize;
                assert_eq!(
                    &bytes[front + init_end..],
                    &fragmented_bytes[init_end..],
                    "{name}: all fragment bytes preserved"
                );
            } else {
                assert_eq!(&bytes[ftyp_size + 4..ftyp_size + 8], b"moov");
            }
            let report = report.media().outputs()[0].media();
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
        let options = FileOutputOptions::default()
            .with_finalization_strategy(FileFinalizationStrategy::InPlace);
        let existing = directory.join(format!("{name}-existing.mp4"));
        fs::write(&existing, b"unrelated file").unwrap();
        let new_session =
            || common::selected_session(source.clone(), SourceLocation::File(playlist.clone()));
        assert!(
            new_session()
                .await
                .unwrap()
                .write_to_file(&existing, options.clone())
                .await
                .is_err()
        );
        assert_eq!(fs::read(&existing).unwrap(), b"unrelated file");
        let invalid = directory.join(format!("{name}-invalid.mp4"));
        let error = new_session()
            .await
            .unwrap()
            .write_to_file(&invalid, options.clone().with_header_reserve_bytes(7))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), EngineErrorKind::InvalidOptions);
        assert!(!invalid.exists());
        let cancelled = directory.join(format!("{name}-cancelled.mp4"));
        let session = new_session().await.unwrap();
        session.handle().cancel();
        assert!(session.write_to_file(&cancelled, options).await.is_err());
        assert!(!cancelled.exists());
    }
    assert!(fs::read_dir(&directory).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".hls-transmux")
    }));
    fs::remove_dir_all(directory).unwrap();
}
