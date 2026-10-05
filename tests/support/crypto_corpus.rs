use hls_transmux::{
    MemorySource, SourceLocation, TextResource,
    crypto::{key::*, resource::*},
    parse_playlist_snapshot,
    playlist::*,
};
use sha2::{Digest, Sha256};
use std::sync::Arc;
pub const SEQUENCE: u64 = 9_007_199_254_740_993;
#[cfg(not(target_arch = "wasm32"))]
pub const KEY_A: [u8; 16] = [
    0x2b, 0x7e, 0x15, 0x16, 0x28, 0xae, 0xd2, 0xa6, 0xab, 0xf7, 0x15, 0x88, 0x09, 0xcf, 0x4f, 0x3c,
];
#[cfg(not(target_arch = "wasm32"))]
pub const KEY_B: [u8; 16] = [
    0x60, 0x3d, 0xeb, 0x10, 0x15, 0xca, 0x71, 0xbe, 0x2b, 0x73, 0xae, 0xf0, 0x85, 0x7d, 0x77, 0x81,
];
pub struct Clock;
impl KeyClock for Clock {
    fn now(&self) -> u64 {
        0
    }
}
#[cfg(not(target_arch = "wasm32"))]
pub struct Provider;
#[cfg(not(target_arch = "wasm32"))]
impl KeyProvider for Provider {
    fn resolve(&self, request: KeyRequest) -> KeyFuture<KeyResolution> {
        Box::pin(async move {
            let key = if request.resource().kind() == KeyResourceKind::Media
                && request.resource().slot().sequence() == SEQUENCE + 1
            {
                KEY_B
            } else {
                KEY_A
            };
            KeyResolution::Available(
                AvailableKey::aes128(SecretKey::new(key.to_vec()).unwrap())
                    .with_version(format!("revision-{}", request.resolve_revision())),
            )
        })
    }
}
pub struct Case {
    pub name: &'static str,
    playlist: &'static str,
    ranges: &'static str,
    encrypted: [&'static [u8]; 2],
    clear: [&'static [u8]; 3],
    bundle: &'static [u8],
    map: Option<(&'static [u8], &'static [u8], &'static [u8])>,
}
macro_rules! case {
    ($name:literal,$suffix:literal,$map:expr) => {
        Case {
            name: $name,
            playlist: include_str!(concat!("../fixtures/crypto/", $name, "/input.m3u8")),
            ranges: include_str!(concat!("../fixtures/crypto/", $name, "/range.m3u8")),
            encrypted: [
                include_bytes!(concat!("../fixtures/crypto/", $name, "/seg0.cbc")),
                include_bytes!(concat!("../fixtures/crypto/", $name, "/seg1.cbc")),
            ],
            clear: [
                include_bytes!(concat!("../fixtures/media/", $name, "/seg0.", $suffix)),
                include_bytes!(concat!("../fixtures/media/", $name, "/seg1.", $suffix)),
                include_bytes!(concat!("../fixtures/media/", $name, "/seg2.", $suffix)),
            ],
            bundle: include_bytes!(concat!("../fixtures/crypto/", $name, "/bundle.bin")),
            map: $map,
        }
    };
}
macro_rules! fmp4 {
    ($name:literal) => {
        case!(
            $name,
            "m4s",
            Some((
                include_bytes!(concat!("../fixtures/crypto/", $name, "/init.cbc")),
                include_bytes!(concat!("../fixtures/crypto/", $name, "/map-range.bin")),
                include_bytes!(concat!("../fixtures/media/", $name, "/init.fmp4"))
            ))
        )
    };
}
pub fn cases() -> [Case; 6] {
    [
        case!("ts_avc_regular", "ts", None),
        case!("ts_hevc_regular", "ts", None),
        case!("ts_aac_audio_only", "ts", None),
        fmp4!("fmp4_avc_regular"),
        fmp4!("fmp4_hevc_regular"),
        fmp4!("fmp4_aac_audio_only"),
    ]
}
pub fn prepared_cases() -> Vec<Case> {
    cases()
        .into_iter()
        .chain([
            case!("ts_avc_video_only", "ts", None),
            fmp4!("fmp4_avc_video_only"),
        ])
        .collect()
}
/// Shared clear and encrypted fixtures for resource and prepared execution tests.
#[allow(dead_code)]
pub fn input(
    name: &str,
    id: &str,
    encrypted: bool,
    ranges: bool,
) -> (hls_transmux::HlsInput, hls_transmux::KeyedInput) {
    let case = prepared_cases()
        .into_iter()
        .find(|case| case.name == name)
        .unwrap();
    let base = format!("https://media.test/{id}/{name}/");
    let suffix = if case.map.is_some() { "m4s" } else { "ts" };
    let clear_playlist = case
        .playlist
        .lines()
        .filter(|line| !line.starts_with("#EXT-X-KEY:"))
        .collect::<Vec<_>>()
        .join("\n")
        .replace("init.cbc", "init.fmp4")
        .replace("seg0.cbc", &format!("seg0.{suffix}"))
        .replace("seg1.cbc", &format!("seg1.{suffix}"))
        .replace("clear.bin", &format!("seg2.{suffix}"));
    let mut source = MemorySource::new()
        .text(format!("{base}clear.m3u8"), clear_playlist.clone())
        .segment(format!("{base}seg0.cbc"), case.encrypted[0])
        .segment(format!("{base}seg1.cbc"), case.encrypted[1])
        .segment(format!("{base}clear.bin"), case.clear[2])
        .segment(format!("{base}bundle.bin"), case.bundle);
    for (i, bytes) in case.clear.iter().enumerate() {
        source = source.segment(format!("{base}seg{i}.{suffix}"), *bytes);
    }
    if let Some((encrypted, bundle, clear)) = case.map {
        source = source
            .segment(format!("{base}init.cbc"), encrypted)
            .segment(format!("{base}map-range.bin"), bundle)
            .segment(format!("{base}init.fmp4"), clear);
    }
    let location = SourceLocation::Url(url::Url::parse(&format!("{base}clear.m3u8")).unwrap());
    let snapshot = parse_playlist_snapshot(
        &TextResource {
            location: location.clone(),
            content: if encrypted {
                if ranges { case.ranges } else { case.playlist }
            } else {
                &clear_playlist
            }
            .into(),
        },
        PlaylistContext::new(InputId::new(id).unwrap(), u64::MAX).with_revision(7),
    )
    .unwrap();
    let source = Arc::new(source);
    (
        hls_transmux::HlsInput::custom(source.clone(), location),
        hls_transmux::KeyedInput::new(snapshot, source),
    )
}
/// Same production corpus runs in native, Node/WASM and Chrome/WASM.
pub async fn run(provider: Arc<dyn KeyProvider>) -> serde_json::Value {
    let cases = cases();
    let mut report = Vec::new();
    for case in cases {
        for ranges in [false, true] {
            let base = format!("https://media.test/{}/", case.name);
            let mut source = MemorySource::new()
                .segment(format!("{base}seg0.cbc"), case.encrypted[0])
                .segment(format!("{base}seg1.cbc"), case.encrypted[1])
                .segment(format!("{base}clear.bin"), case.clear[2])
                .segment(format!("{base}bundle.bin"), case.bundle);
            if let Some((encrypted, bundle, _)) = case.map {
                source = source
                    .segment(format!("{base}init.cbc"), encrypted)
                    .segment(format!("{base}map-range.bin"), bundle);
            }
            let snapshot = parse_playlist_snapshot(
                &TextResource {
                    content: if ranges { case.ranges } else { case.playlist }.into(),
                    location: SourceLocation::Url(
                        url::Url::parse(&format!("{base}input.m3u8")).unwrap(),
                    ),
                },
                PlaylistContext::new(InputId::new("primary").unwrap(), u64::MAX).with_revision(7),
            )
            .unwrap();
            let keys = KeySession::new(
                format!("{}-{ranges}", case.name),
                "public-test",
                provider.clone(),
                Arc::new(Clock),
                KeySessionOptions::default(),
            )
            .unwrap();
            let session = ResourceSession::new(
                keys,
                ResourceOptions::default()
                    .with_encrypted_ranges(EncryptedRangePolicy::CompleteResources),
            )
            .unwrap();
            let source = Arc::new(source);
            let mut hash = Sha256::new();
            if let Some((_, _, clear)) = case.map {
                let output = session
                    .read(source.clone(), ResourceRequest::map(&snapshot, 0).unwrap())
                    .await
                    .unwrap();
                assert_eq!(output.bytes(), clear);
                assert_eq!(output.container(), ClearContainer::Fmp4Init);
                assert!(output.iv().is_some());
                hash.update(output.bytes());
            }
            let mut revisions = Vec::new();
            for i in 0..3 {
                let output = session
                    .read(
                        source.clone(),
                        ResourceRequest::media(&snapshot, i).unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(output.bytes(), case.clear[i]);
                if i == 0 {
                    assert_eq!(output.iv(), Some(sequence_iv(SEQUENCE)));
                }
                if i == 2 {
                    assert!(output.key_version().is_none() && output.iv().is_none());
                }
                revisions.push(output.resolve_revision());
                hash.update(output.bytes());
                assert_eq!(session.stats().reserved_bytes(), 0);
                assert_eq!(session.stats().resources(), 0);
            }
            assert_ne!(revisions[0], revisions[1]);
            report.push(serde_json::json!({"name":case.name,"ranges":ranges,"sha256":format!("{:x}",hash.finalize())}));
        }
    }
    serde_json::json!({"cases":report,"completeResourceDecryption":true,"originalSequenceIv":true,"mapIv":true,"sameUriKeyRotation":true,"clearSwitch":true})
}
