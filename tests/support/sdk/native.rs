#![allow(dead_code)]
#[path = "../../../../rust/keyed.rs"]
mod wire;
use hls_transmux::crypto::key::KeyFuture;
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};
struct Host {
    files: HashMap<String, String>,
    root: std::path::PathBuf,
}
impl wire::Host for Host {
    fn now(&self) -> u64 {
        0
    }
    fn abort(&self, _: String) {}
    fn read(&self, request: String) -> wire::ReadFuture {
        let r: Value = serde_json::from_str(&request).unwrap();
        let bytes = std::fs::read(self.root.join(&self.files[r["url"].as_str().unwrap()]));
        Box::pin(async move {
            let bytes = bytes?;
            Ok(if let Some(offset) = r["offset"].as_str() {
                let start: usize = offset.parse().unwrap();
                let length: usize = r["length"].as_str().unwrap().parse().unwrap();
                bytes[start..start + length].to_vec()
            } else {
                bytes
            })
        })
    }
    fn resolve(&self, request: String) -> KeyFuture<wire::Reply> {
        let r: Value = serde_json::from_str(&request).unwrap();
        if r["method"] == "SAMPLE-AES-CTR" { assert_eq!(r["kid"], "00112233445566778899aabbccddeeff"); }
        let hex = if r["resourceKind"] == "media" && r["originalSequence"] == "9007199254740994" {
            "603deb1015ca71be2b73aef0857d7781"
        } else {
            "2b7e151628aed2a6abf7158809cf4f3c"
        };
        let key = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        Box::pin(async move {
            wire::Reply {
                status: "available".into(),
                key,
                version: None,
                ttl: None,
            }
        })
    }
}
fn canonical(bytes: &mut [u8]) {
    let mut offset = 0;
    while offset + 8 <= bytes.len() {
        let n = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        assert!(n >= 8 && offset + n <= bytes.len());
        let kind: [u8; 4] = bytes[offset + 4..offset + 8].try_into().unwrap();
        let body = &mut bytes[offset + 8..offset + n];
        match &kind {
            b"moov" | b"trak" | b"mdia" => canonical(body),
            b"mvhd" | b"tkhd" | b"mdhd" => {
                let end = if body[0] == 1 { 20 } else { 12 };
                body[4..end].fill(0)
            }
            _ => {}
        }
        offset += n;
    }
    assert_eq!(offset, bytes.len());
}
#[tokio::test]
async fn sdk_timeline_host_contract() {
    use sha2::{Digest, Sha256};
    let root = std::path::PathBuf::from(std::env::var("HLS_TIMELINE_ROOT").unwrap());
    let cases: Value =
        serde_json::from_slice(&std::fs::read(root.join("target/sdk-compat/cases.json")).unwrap())
            .unwrap();
    let mut results = Vec::new();
    for case in cases.as_array().unwrap() {
        eprintln!("timeline SDK case: {}", case["name"]);
        let request: wire::Request = serde_json::from_value(case["request"].clone()).unwrap();
        let selection: wire::timeline::Selection =
            serde_json::from_value(case["selection"].clone()).unwrap();
        let host = Arc::new(Host {
            files: serde_json::from_value(case["files"].clone()).unwrap(),
            root: root.clone(),
        });
        let session = wire::timeline::prepare_timeline(&request, host, selection.options())
            .await
            .unwrap();
        let (mut outputs, report) = if request.mode == "stream" {
            let mut bytes = Vec::new();
            let report = session.write_to(&mut bytes).await.unwrap();
            (vec![bytes], report)
        } else {
            session.into_mp4_outputs().await.unwrap()
        };
        assert_eq!(outputs.len(), case["outputs"].as_u64().unwrap() as usize);
        let hashes: Vec<_> = outputs
            .iter_mut()
            .map(|bytes| {
                canonical(bytes);
                format!("{:x}", Sha256::digest(bytes))
            })
            .collect();
        results.push(json!({"name":case["name"],"hashes":hashes,"report":report}));
    }
    std::fs::write(
        root.join("target/sdk-compat/native-timeline.json"),
        serde_json::to_vec_pretty(&results).unwrap(),
    )
    .unwrap();
}
