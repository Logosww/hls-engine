use hls_transmux::{
    crypto::{key::*, resource::*},
    *,
};
use sha2::{Digest, Sha256};
use std::sync::Arc;
#[path = "crypto_corpus.rs"]
#[allow(dead_code)]
pub mod corpus;

// A caller-owned, non-Send sink that yields between short writes on every runtime.
struct LocalWriter {
    bytes: Vec<u8>,
    pending: bool,
    owner: std::rc::Rc<()>,
}
impl LocalWriter {
    fn new() -> Self {
        Self {
            bytes: vec![],
            pending: false,
            owner: std::rc::Rc::new(()),
        }
    }
}
impl tokio::io::AsyncWrite for LocalWriter {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.pending = !self.pending;
        if self.pending {
            cx.waker().wake_by_ref();
            return std::task::Poll::Pending;
        }
        let n = bytes.len().min(4093);
        self.bytes.extend_from_slice(&bytes[..n]);
        std::task::Poll::Ready(Ok(n))
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        assert_eq!(std::rc::Rc::strong_count(&self.owner), 1);
        std::task::Poll::Ready(Ok(()))
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        panic!("caller owns close")
    }
}
pub fn keys(provider: Arc<dyn KeyProvider>) -> KeySession {
    KeySession::new(
        "prepared",
        "test",
        provider,
        Arc::new(corpus::Clock),
        KeySessionOptions::default(),
    )
    .unwrap()
}
pub fn options() -> KeyedPrepareOptions {
    KeyedPrepareOptions::default().with_resources(
        ResourceOptions::default().with_encrypted_ranges(EncryptedRangePolicy::CompleteResources),
    )
}
pub fn pair(
    v: &str,
    a: Option<&str>,
    encrypted: [bool; 2],
    ranges: bool,
) -> (HlsInputs, KeyedInputs) {
    let (legacy, keyed) = corpus::input(v, "primary", encrypted[0], ranges);
    let (mut legacy, mut keyed) = (HlsInputs::new(legacy), KeyedInputs::new(keyed));
    if let Some(audio) = a {
        let (old, new) = corpus::input(audio, "audio", encrypted[1], ranges);
        legacy = legacy.with_audio(old);
        keyed = keyed.with_audio(new);
    }
    (legacy, keyed)
}
/// Only wall-clock creation/modification fields differ across calls and runtimes.
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
/// Byte-for-byte output equivalence includes samples, DTS/PTS, configs, edits and track selection.
pub async fn run(provider: Arc<dyn KeyProvider>) -> serde_json::Value {
    let mut configurations = Vec::new();
    for case in corpus::prepared_cases() {
        for encrypted in [false, true] {
            for ranges in [false, true] {
                configurations.push((case.name, None, [encrypted, false], ranges));
            }
        }
    }
    for v in [
        "ts_avc_regular",
        "ts_hevc_regular",
        "fmp4_avc_regular",
        "fmp4_hevc_regular",
    ] {
        for a in ["ts_aac_audio_only", "fmp4_aac_audio_only"] {
            for primary in [false, true] {
                for audio in [false, true] {
                    for ranges in [false, true] {
                        configurations.push((v, Some(a), [primary, audio], ranges));
                    }
                }
            }
        }
    }
    let mut reports = Vec::new();
    for (v, a, encrypted, ranges) in configurations {
        let (old, new) = pair(v, a, encrypted, ranges);
        let clear = prepare_hls(old, PrepareOptions::default()).await.unwrap();
        let keyed = prepare_hls_with_keys(new, keys(provider.clone()), options())
            .await
            .unwrap();
        assert_eq!(keyed.info().tracks(), clear.info().tracks());
        assert_eq!(keyed.info().timeline(), clear.info().timeline());
        let (expected, report) = clear.into_mp4_bytes().await.unwrap();
        let (actual, keyed_report) = keyed.into_mp4_bytes().await.unwrap();
        assert!(
            canonical(actual.clone()) == canonical(expected),
            "{v}/{a:?}/{encrypted:?}/{ranges}"
        );
        assert_eq!(keyed_report.media().tracks, report.media().tracks);
        assert_eq!(keyed_report.timeline(), report.timeline());
        for input in keyed_report.inputs() {
            assert_eq!(input.processed_segments(), 3);
            assert_eq!(input.downloaded_segments(), 3);
        }
        let (old, new) = pair(v, a, encrypted, ranges);
        let mut expected_fragmented = Vec::new();
        let mut actual_fragmented = LocalWriter::new();
        prepare_hls(old, PrepareOptions::default().with_write_mfra(true))
            .await
            .unwrap()
            .write_to(&mut expected_fragmented)
            .await
            .unwrap();
        prepare_hls_with_keys(new, keys(provider.clone()), options().with_write_mfra(true))
            .await
            .unwrap()
            .write_to(&mut actual_fragmented)
            .await
            .unwrap();
        assert!(canonical(actual_fragmented.bytes.clone()) == canonical(expected_fragmented));
        reports.push(
            serde_json::json!({"primary":v,"audio":a,"encrypted":encrypted,"ranges":ranges,
            "mp4":format!("{:x}", Sha256::digest(canonical(actual))),
            "fragmented":format!("{:x}", Sha256::digest(canonical(actual_fragmented.bytes)))}),
        );
    }
    serde_json::json!({"cases": reports, "sharedCore":true, "originalSequenceIv":true, "replacementAudio":true})
}
