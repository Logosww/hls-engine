//! Independent Shaka media, shared by native and actual WASM tests.
use hls_transmux::{crypto::key::*, playlist::*, *};
use sha2::{Digest, Sha256};
use std::sync::Arc;
pub const KEY: [u8; 16] = [
    0x2b, 0x7e, 0x15, 0x16, 0x28, 0xae, 0xd2, 0xa6, 0xab, 0xf7, 0x15, 0x88, 0x09, 0xcf, 0x4f, 0x3c,
];
pub struct Clock;
impl KeyClock for Clock {
    fn now(&self) -> u64 {
        0
    }
}
pub struct Provider;
impl KeyProvider for Provider {
    fn resolve(&self, r: KeyRequest) -> KeyFuture<KeyResolution> {
        Box::pin(async move {
            let rotated = matches!(r.reference().location().location(),SourceLocation::Url(url) if url.path().ends_with("/packed-rotated-key"));
            let secret = SecretKey::new(if rotated {
                vec![
                    0x60, 0x3d, 0xeb, 0x10, 0x15, 0xca, 0x71, 0xbe, 0x2b, 0x73, 0xae, 0xf0, 0x85,
                    0x7d, 0x77, 0x81,
                ]
            } else {
                KEY.to_vec()
            })
            .unwrap();
            let mut key = match r.reference().method() {
                EncryptionMethod::SampleAes => AvailableKey::sample_aes(secret),
                EncryptionMethod::SampleAesCtr => AvailableKey::sample_aes_ctr(secret),
                _ => AvailableKey::aes128(secret),
            };
            if let Some(kid) = r.resource().kid() {
                key = key.with_kid(kid);
            }
            KeyResolution::Available(key.with_version("fixture"))
        })
    }
}
pub fn keys(p: Arc<dyn KeyProvider>) -> KeySession {
    KeySession::new(
        "sample-test",
        "fixture",
        p,
        Arc::new(Clock),
        KeySessionOptions::default(),
    )
    .unwrap()
}
pub struct Case {
    pub name: &'static str,
    pub playlist: &'static str,
    pub files: &'static [(&'static str, &'static [u8])],
}
macro_rules! case {
    ($name:literal,[$($file:literal),*])=>{Case{name:$name,playlist:include_str!(concat!("../fixtures/sample_crypto/",$name,"/input.m3u8")),files:&[$(($file,include_bytes!(concat!("../fixtures/sample_crypto/",$name,"/",$file)))),*]}};
}
pub fn cases() -> Vec<Case> {
    vec![
        case!(
            "fmp4_aac_cbcs",
            ["init.mp4", "seg1.m4s", "seg2.m4s", "seg3.m4s", "seg4.m4s"]
        ),
        case!(
            "fmp4_aac_cenc",
            ["init.mp4", "seg1.m4s", "seg2.m4s", "seg3.m4s", "seg4.m4s"]
        ),
        case!(
            "fmp4_aac_clear",
            ["init.mp4", "seg1.m4s", "seg2.m4s", "seg3.m4s", "seg4.m4s"]
        ),
        case!(
            "fmp4_avc_cbcs",
            ["init.mp4", "seg1.m4s", "seg2.m4s", "seg3.m4s"]
        ),
        case!(
            "fmp4_avc_cenc",
            ["init.mp4", "seg1.m4s", "seg2.m4s", "seg3.m4s"]
        ),
        case!(
            "fmp4_avc_clear",
            ["init.mp4", "seg1.m4s", "seg2.m4s", "seg3.m4s"]
        ),
        case!("fmp4_hevc_cbcs", ["init.mp4", "seg1.m4s", "seg2.m4s"]),
        case!("fmp4_hevc_cenc", ["init.mp4", "seg1.m4s", "seg2.m4s"]),
        case!("fmp4_hevc_clear", ["init.mp4", "seg1.m4s", "seg2.m4s"]),
        case!(
            "fmp4_nal1_cenc",
            ["init.mp4", "seg1.m4s", "seg2.m4s", "seg3.m4s"]
        ),
        case!(
            "fmp4_nal1_clear",
            ["init.mp4", "seg1.m4s", "seg2.m4s", "seg3.m4s"]
        ),
        case!(
            "fmp4_nal2_cenc",
            ["init.mp4", "seg1.m4s", "seg2.m4s", "seg3.m4s"]
        ),
        case!(
            "fmp4_nal2_clear",
            ["init.mp4", "seg1.m4s", "seg2.m4s", "seg3.m4s"]
        ),
        case!("ts_aac_clear", ["seg1.ts", "seg2.ts", "seg3.ts", "seg4.ts"]),
        case!(
            "ts_aac_sample",
            ["seg1.ts", "seg2.ts", "seg3.ts", "seg4.ts"]
        ),
        case!("ts_avc_clear", ["seg1.ts", "seg2.ts", "seg3.ts"]),
        case!("ts_avc_sample", ["seg1.ts", "seg2.ts", "seg3.ts"]),
    ]
}
pub fn canonical(mut bytes: Vec<u8>) -> Vec<u8> {
    fn walk(bytes: &mut [u8]) {
        let mut offset = 0;
        while offset + 8 <= bytes.len() {
            let size = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
            assert!(size >= 8 && offset + size <= bytes.len());
            let kind: [u8; 4] = bytes[offset + 4..offset + 8].try_into().unwrap();
            let content = &mut bytes[offset + 8..offset + size];
            match &kind {
                b"moov" | b"trak" | b"mdia" => walk(content),
                b"mvhd" | b"tkhd" | b"mdhd" => {
                    let end = if content[0] == 1 { 20 } else { 12 };
                    content[4..end].fill(0);
                }
                _ => {}
            }
            offset += size;
        }
        assert_eq!(offset, bytes.len());
    }
    walk(&mut bytes);
    bytes
}
pub fn input(name: &str, id: &str) -> KeyedInput {
    let case = cases().into_iter().find(|c| c.name == name).unwrap();
    input_modified(&case, id, case.playlist.to_string(), None)
}
pub fn input_modified(
    case: &Case,
    id: &str,
    playlist: String,
    override_file: Option<(&str, Vec<u8>)>,
) -> KeyedInput {
    let base = format!("https://sample.test/{id}/{}/", case.name);
    let mut source = MemorySource::new();
    for (name, bytes) in case.files {
        source = source.segment(
            format!("{base}{name}"),
            override_file
                .as_ref()
                .filter(|(n, _)| n == name)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| bytes.to_vec()),
        );
    }
    let snapshot = parse_playlist_snapshot(
        &TextResource {
            location: SourceLocation::Url(url::Url::parse(&format!("{base}input.m3u8")).unwrap()),
            content: playlist,
        },
        PlaylistContext::new(InputId::new(id).unwrap(), 0),
    )
    .unwrap();
    KeyedInput::new(snapshot, Arc::new(source))
}
pub async fn run(provider: Arc<dyn KeyProvider>) -> serde_json::Value {
    let mut reports = Vec::new();
    for name in [
        "fmp4_avc_cenc",
        "fmp4_avc_cbcs",
        "fmp4_hevc_cenc",
        "fmp4_hevc_cbcs",
        "fmp4_aac_cenc",
        "fmp4_aac_cbcs",
        "ts_avc_sample",
        "ts_aac_sample",
        "fmp4_nal1_cenc",
        "fmp4_nal2_cenc",
    ] {
        let clear = format!("{}_clear", name.rsplit_once('_').unwrap().0);
        for timeline in [false, true] {
            let mut outputs = Vec::new();
            for case in [&clear, name] {
                let inputs = KeyedInputs::new(input(case, "primary"));
                let output = if timeline {
                    let options = TimelinePrepareOptions::default().with_range(
                        PresentationRange::new(
                            MediaTime::new(250, 1000).unwrap(),
                            MediaTime::new(3100, 1000).unwrap(),
                        )
                        .unwrap(),
                    );
                    prepare_hls_timeline(inputs, keys(provider.clone()), options)
                        .await
                        .unwrap()
                        .into_mp4_bytes()
                        .await
                        .map(|v| v.0)
                        .unwrap_or_else(|e| panic!("{case}: {e:?}"))
                } else {
                    prepare_hls_with_keys(
                        inputs,
                        keys(provider.clone()),
                        KeyedPrepareOptions::default(),
                    )
                    .await
                    .unwrap_or_else(|e| panic!("prepare {case}: {e:?} {:?}", e.sample_error()))
                    .into_mp4_bytes()
                    .await
                    .map(|v| v.0)
                    .unwrap_or_else(|e| panic!("{case}: {e:?} {:?}", e.sample_error()))
                };
                outputs.push(canonical(output));
            }
            assert!(
                outputs[0] == outputs[1],
                "sample output differs for {name}, timeline={timeline}; lengths {} {}",
                outputs[0].len(),
                outputs[1].len()
            );
            #[cfg(not(target_arch = "wasm32"))]
            if let Ok(directory) = std::env::var("HLS_SAMPLE_OUTPUT") {
                std::fs::create_dir_all(&directory).unwrap();
                std::fs::write(format!("{directory}/{name}-{timeline}.mp4"), &outputs[1]).unwrap();
            }
            reports.push(serde_json::json!({"name":name,"timeline":timeline,"sha256":format!("{:x}",Sha256::digest(&outputs[1]))}));
        }
    }
    // Exercise fragment-local multi-KID resolution through every runtime's real provider.
    let case = cases()
        .into_iter()
        .find(|c| c.name == "fmp4_avc_cenc")
        .unwrap();
    let original = case
        .files
        .iter()
        .find(|(name, _)| *name == "seg1.m4s")
        .unwrap()
        .1;
    let changed = with_fragment_kid(original, 0x42);
    let modified = input_modified(
        &case,
        "primary",
        case.playlist.into(),
        Some(("seg1.m4s", changed)),
    );
    let bytes = prepare_hls_with_keys(
        KeyedInputs::new(modified),
        keys(provider.clone()),
        KeyedPrepareOptions::default(),
    )
    .await
    .unwrap()
    .into_mp4_bytes()
    .await
    .unwrap()
    .0;
    let hash = format!("{:x}", Sha256::digest(canonical(bytes)));
    assert_eq!(hash, reports[0]["sha256"].as_str().unwrap());
    reports
        .push(serde_json::json!({"name":"fragment-kid-rotation","timeline":false,"sha256":hash}));
    serde_json::json!({"cases":reports})
}

/// Add a fragment-local seig mapping to an independently encrypted fragment.
/// Ciphertext and senc records are unchanged; the alternate KID uses the same test key.
pub fn with_fragment_kid(bytes: &[u8], kid: u8) -> Vec<u8> {
    fn bx(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        [(data.len() as u32 + 8).to_be_bytes().as_slice(), kind, data].concat()
    }
    let mut output = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let length = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        let kind = &bytes[offset + 4..offset + 8];
        let payload = &bytes[offset + 8..offset + length];
        if kind != b"moof" {
            output.extend_from_slice(&bytes[offset..offset + length]);
            offset += length;
            continue;
        }
        let mut moof = Vec::new();
        let mut pos = 0;
        while pos < payload.len() {
            let size = u32::from_be_bytes(payload[pos..pos + 4].try_into().unwrap()) as usize;
            if &payload[pos + 4..pos + 8] != b"traf" {
                moof.extend_from_slice(&payload[pos..pos + size]);
                pos += size;
                continue;
            }
            let mut traf = payload[pos + 8..pos + size].to_vec();
            let mut p = 0;
            let mut count = 0u32;
            let mut runs = Vec::new();
            while p < traf.len() {
                let n = u32::from_be_bytes(traf[p..p + 4].try_into().unwrap()) as usize;
                if &traf[p + 4..p + 8] == b"trun" {
                    count += u32::from_be_bytes(traf[p + 12..p + 16].try_into().unwrap());
                    runs.push(p);
                }
                p += n;
            }
            let mut sgpd = vec![1, 0, 0, 0];
            sgpd.extend_from_slice(b"seig");
            sgpd.extend_from_slice(&20u32.to_be_bytes());
            sgpd.extend_from_slice(&1u32.to_be_bytes());
            sgpd.extend_from_slice(&[0, 0, 1, 16]);
            sgpd.extend_from_slice(&[kid; 16]);
            let mut sbgp = vec![0; 4];
            sbgp.extend_from_slice(b"seig");
            sbgp.extend_from_slice(&1u32.to_be_bytes());
            sbgp.extend_from_slice(&count.to_be_bytes());
            sbgp.extend_from_slice(&0x10001u32.to_be_bytes());
            let extra = [bx(b"sgpd", &sgpd), bx(b"sbgp", &sbgp)].concat();
            for start in runs {
                let flags = u32::from_be_bytes(traf[start + 8..start + 12].try_into().unwrap());
                assert_ne!(flags & 1, 0);
                let v = i32::from_be_bytes(traf[start + 16..start + 20].try_into().unwrap());
                traf[start + 16..start + 20]
                    .copy_from_slice(&(v + extra.len() as i32).to_be_bytes());
            }
            traf.extend_from_slice(&extra);
            moof.extend_from_slice(&bx(b"traf", &traf));
            pos += size;
        }
        output.extend_from_slice(&bx(b"moof", &moof));
        offset += length;
    }
    output
}
