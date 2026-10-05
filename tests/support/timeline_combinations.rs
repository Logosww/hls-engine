use super::*;
use aes::cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
use hls_transmux::{crypto::key::*, playlist::*};

// Synthetic test keys. Bind the selected key to the explicit IV so MAP and
// media requests exercise distinct declarations, including after clock resets.
struct RotatingKeys;
impl KeyProvider for RotatingKeys {
    fn resolve(&self, request: KeyRequest) -> KeyFuture<KeyResolution> {
        Box::pin(async move {
            tokio::task::yield_now().await;
            KeyResolution::Available(AvailableKey::aes128(
                SecretKey::new(vec![request.reference().explicit_iv().unwrap()[15]; 16]).unwrap(),
            ))
        })
    }
}
fn encrypt(bytes: &[u8], revision: u8) -> Vec<u8> {
    let mut iv = [0; 16];
    iv[15] = revision;
    let mut padded = vec![0; (bytes.len() / 16 + 1) * 16];
    padded[..bytes.len()].copy_from_slice(bytes);
    cbc::Encryptor::<aes::Aes128>::new(&[revision; 16].into(), &iv.into())
        .encrypt_padded_mut::<Pkcs7>(&mut padded, bytes.len())
        .unwrap()
        .to_vec()
}
fn keyed_input(id: &str, text: String, resources: Vec<(String, Vec<u8>)>) -> KeyedInput {
    let base = format!("https://combination.test/{id}/");
    let mut source = MemorySource::new();
    for (name, bytes) in resources {
        source = source.segment(format!("{base}{name}"), bytes);
    }
    let snapshot = parse_playlist_snapshot(
        &TextResource {
            content: text,
            location: SourceLocation::Url(format!("{base}index.m3u8").parse().unwrap()),
        },
        PlaylistContext::new(InputId::new(id).unwrap(), 7),
    )
    .unwrap();
    KeyedInput::new(snapshot, Arc::new(source))
}

fn reset_maps(encrypted: bool) -> KeyedInputs {
    let init = include_bytes!("../fixtures/media/fmp4_avc_video_only/init.fmp4");
    let first = include_bytes!("../fixtures/media/fmp4_avc_video_only/seg0.m4s");
    let mut text =
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:9007199254740993\n".to_owned();
    let mut resources = Vec::new();
    for i in 0..3 {
        if i > 0 {
            text.push_str("#EXT-X-DISCONTINUITY\n");
        }
        // A fresh MAP uses its own captured key, then a different media key.
        for (map, payload, revision) in [
            (true, init.as_slice(), i * 2 + 1),
            (false, first.as_slice(), i * 2 + 2),
        ] {
            if encrypted {
                text.push_str(&format!(
                    "#EXT-X-KEY:METHOD=AES-128,URI=\"key-{revision}\",IV=0x{revision:032x}\n"
                ));
            }
            let name = format!("{}-{i}.bin", if map { "map" } else { "segment" });
            if map {
                text.push_str(&format!("#EXT-X-MAP:URI=\"{name}\"\n"));
            } else {
                text.push_str(&format!("#EXTINF:2,\n{name}\n"));
            }
            resources.push((
                name,
                if encrypted {
                    encrypt(payload, revision)
                } else {
                    payload.to_vec()
                },
            ));
        }
    }
    text.push_str("#EXT-X-ENDLIST\n");
    KeyedInputs::new(keyed_input("primary", text, resources))
}

#[tokio::test]
async fn encrypted_range_crosses_resets_map_redeclarations_and_key_rotation() {
    for requested in [None, Some(range(1500, 4500)), Some(range(2500, 3500))] {
        let mut reference = None;
        for encrypted in [false, true] {
            let mut settings = options();
            if let Some(range) = requested {
                settings = settings.with_range(range);
            }
            let (bytes, report) = prepare_hls_timeline(
                reset_maps(encrypted),
                suite::keys(Arc::new(RotatingKeys)),
                settings,
            )
            .await
            .unwrap()
            .into_mp4_bytes()
            .await
            .unwrap();
            let bytes = suite::canonical(bytes);
            if let Some(expected) = &reference {
                assert_eq!(&bytes, expected);
            } else {
                reference = Some(bytes);
            }
            assert_eq!(report.outputs().len(), 1);
            assert_eq!(
                report.dependencies().len(),
                if requested == Some(range(2500, 3500)) {
                    1
                } else {
                    3
                }
            );
            for dependency in report.dependencies() {
                assert!(dependency.slot().sequence() >= 9_007_199_254_740_993);
                if encrypted {
                    assert_eq!(dependency.key_declarations().len(), 1);
                    assert_eq!(dependency.map_key_declarations().len(), 1);
                    assert_ne!(
                        dependency.key_declarations(),
                        dependency.map_key_declarations()
                    );
                }
            }
            if requested == Some(range(2500, 3500)) {
                assert_eq!(report.outputs()[0].mappings()[0].epoch(), 1);
                assert_eq!(report.actual_range().start(), MediaTime::new(2, 1).unwrap());
            }
        }
    }
}

fn gap_tracks(primary_gap: bool, audio_gap: bool, encrypted: bool) -> KeyedInputs {
    fn input(audio: bool, gap: bool, encrypted: bool) -> KeyedInput {
        let (id, init, segments): (&str, &[u8], [&[u8]; 3]) = if audio {
            (
                "audio",
                include_bytes!("../fixtures/media/fmp4_aac_audio_only/init.fmp4"),
                [
                    include_bytes!("../fixtures/media/fmp4_aac_audio_only/seg0.m4s"),
                    include_bytes!("../fixtures/media/fmp4_aac_audio_only/seg1.m4s"),
                    include_bytes!("../fixtures/media/fmp4_aac_audio_only/seg2.m4s"),
                ],
            )
        } else {
            (
                "primary",
                include_bytes!("../fixtures/media/fmp4_avc_video_only/init.fmp4"),
                [
                    include_bytes!("../fixtures/media/fmp4_avc_video_only/seg0.m4s"),
                    include_bytes!("../fixtures/media/fmp4_avc_video_only/seg1.m4s"),
                    include_bytes!("../fixtures/media/fmp4_avc_video_only/seg2.m4s"),
                ],
            )
        };
        let mut text = format!(
            "#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXT-X-MEDIA-SEQUENCE:{}\n",
            if audio { 9123 } else { 33 }
        );
        let mut resources = Vec::new();
        if encrypted {
            text.push_str(
                "#EXT-X-KEY:METHOD=AES-128,URI=\"key\",IV=0x00000000000000000000000000000001\n",
            );
        }
        text.push_str("#EXT-X-MAP:URI=\"init.mp4\"\n");
        resources.push((
            "init.mp4".into(),
            if encrypted {
                encrypt(init, 1)
            } else {
                init.to_vec()
            },
        ));
        for (index, bytes) in segments.into_iter().enumerate() {
            if gap && index == 1 {
                text.push_str("#EXT-X-GAP\n");
            }
            text.push_str(&format!("#EXTINF:2,\nseg{index}.m4s\n"));
            if !gap || index != 1 {
                resources.push((
                    format!("seg{index}.m4s"),
                    if encrypted {
                        encrypt(bytes, 1)
                    } else {
                        bytes.to_vec()
                    },
                ));
            }
        }
        text.push_str("#EXT-X-ENDLIST\n");
        keyed_input(id, text, resources)
    }
    KeyedInputs::new(input(false, primary_gap, encrypted))
        .with_audio(input(true, audio_gap, encrypted))
}

#[tokio::test]
async fn common_gap_matrix_keeps_other_tracks_media_and_applies_one_shift() {
    for primary_gap in [false, true] {
        for audio_gap in [false, true] {
            for policy in [GapPolicy::Preserve, GapPolicy::Collapse] {
                let mut reference = None;
                for encrypted in [false, true] {
                    let mut bytes = Vec::new();
                    let report = prepare_hls_timeline(
                        gap_tracks(primary_gap, audio_gap, encrypted),
                        suite::keys(Arc::new(RotatingKeys)),
                        options().with_gap_policy(policy),
                    )
                    .await
                    .unwrap()
                    .write_to(&mut bytes)
                    .await
                    .unwrap();
                    let bytes = suite::canonical(bytes);
                    if let Some(expected) = &reference {
                        assert_eq!(&bytes, expected);
                    } else {
                        reference = Some(bytes);
                    }
                    assert_eq!(report.gaps().is_empty(), !(primary_gap && audio_gap));
                    let mappings = report.outputs()[0].mappings();
                    let offset = mappings[0].output_start().seconds()
                        - mappings[0].presentation_range().start().seconds();
                    for mapping in mappings {
                        let source = mapping.presentation_range();
                        let mut removed = 0.0;
                        for gap in report.gaps() {
                            assert!(
                                source.end().seconds() <= gap.start().seconds()
                                    || source.start().seconds() >= gap.end().seconds()
                            );
                            if policy == GapPolicy::Collapse
                                && gap.end().seconds() <= source.start().seconds()
                            {
                                removed += gap.end().seconds() - gap.start().seconds();
                            }
                        }
                        assert!(
                            (mapping.output_start().seconds()
                                - (source.start().seconds() - removed + offset))
                                .abs()
                                < 0.000001
                        );
                    }
                }
            }
        }
    }
}

#[tokio::test]
async fn single_ts_frame_requires_tail_evidence_and_never_uses_extinf() {
    let fixture = include_bytes!("../fixtures/media/ts_avc_video_only/seg0.ts");
    let mut first = Vec::new();
    let mut started = false;
    for packet in fixture.as_chunks::<188>().0 {
        let offset = match (packet[3] >> 4) & 3 {
            1 => 4,
            3 => 5 + packet[4] as usize,
            _ => 188,
        };
        if packet[1] & 0x40 != 0
            && packet.get(offset..offset + 3) == Some(&[0, 0, 1])
            && packet
                .get(offset + 3)
                .is_some_and(|v| (0xe0..=0xef).contains(v))
        {
            if started {
                break;
            }
            started = true;
        }
        first.extend_from_slice(packet);
    }
    for explicit in [false, true] {
        let mut settings = options();
        if explicit {
            settings = settings
                .with_tail_duration(TailDurationPolicy::Explicit(MediaTime::new(1, 30).unwrap()));
        }
        let result = prepare_hls_timeline(
            snapshot_input(
                "#EXTM3U\n#EXT-X-TARGETDURATION:9\n#EXTINF:9,\na.ts\n#EXT-X-ENDLIST\n",
                &[("a.ts", &first)],
            ),
            suite::keys(Arc::new(RotatingKeys)),
            settings,
        )
        .await
        .unwrap()
        .into_mp4_bytes()
        .await;
        if explicit {
            let (_, report) = result.unwrap();
            assert_eq!(report.outputs()[0].media().tracks[0].sample_count, 1);
            assert!((report.actual_range().end().seconds() - 1.0 / 30.0).abs() < 0.000001);
        } else {
            assert_eq!(
                result.unwrap_err().kind(),
                TimelineErrorKind::MissingTailDuration
            );
        }
    }
}

trait Seconds {
    fn seconds(self) -> f64;
}
impl Seconds for MediaTime {
    fn seconds(self) -> f64 {
        self.ticks() as f64 / f64::from(self.timescale())
    }
}
