#[path = "../../support/aac_fixture.rs"]
mod fixture;
use hls_engine::{playlist::*, *};
use std::{sync::Arc, time::Duration};
async fn suite() -> serde_json::Value {
    let mut results = vec![];
    for limit in [0, 40, 44, 50] {
        let mut times: Vec<_> = (0..15).map(|i| i * 1920).collect();
        times.extend((0..20).map(|i| 24840 + i * 1920));
        let source =
            MemorySource::new().segment("https://aac.test/segment", fixture::segment(&times));
        let id = InputId::new("audio").unwrap();
        let opts = if limit == 0 {
            EngineOptions::default()
        } else {
            EngineOptions::default().with_aac_timestamp_policy(AacTimestampPolicy::RepairOverlaps {
                max_shift: Duration::from_millis(limit),
            })
        };
        let session = EngineSession::new(
            EngineInputs::new(
                EngineInput::new(id.clone(), Arc::new(source)),
                EmbeddedAudio::Keep,
            ),
            crate::sample_corpus::keys(Arc::new(crate::sample_corpus::Provider)),
            opts,
        )
        .unwrap();
        let snapshot = parse_playlist_snapshot(
            &TextResource {
                location: SourceLocation::Url(url::Url::parse("https://aac.test/list").unwrap()),
                content: "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\nsegment\n#EXT-X-ENDLIST\n"
                    .into(),
            },
            PlaylistContext::new(id.clone(), 0),
        )
        .unwrap();
        session.handle().accept_snapshot(&id, &snapshot).unwrap();
        match session
            .into_bytes(1 << 20, OutputFormat::FragmentedMp4)
            .await
        {
            Ok((bytes, report)) => {
                assert!(limit >= 44);
                let summary = &report.media().aac_repairs().unwrap()[0];
                assert_eq!(summary.corrected_frames, 20);
                assert_eq!(summary.max_shift, MediaTime::new(11, 250).unwrap());
                assert_eq!(report.tracks()[0].sample_count(), 35);
                use sha2::{Digest, Sha256};
                results.push(serde_json::json!({"limit":limit,"summary":summary,"hash":format!("{:x}",Sha256::digest(crate::sample_corpus::canonical(bytes)))}));
            }
            Err(e) => {
                assert_eq!(
                    e.kind(),
                    if limit == 0 {
                        EngineErrorKind::AacTimestampOverlap
                    } else {
                        EngineErrorKind::AacTimestampRepairLimitExceeded
                    }
                );
                if limit == 0 {
                    let d = e.aac_overlap_error().unwrap();
                    assert_eq!(d.slot.sequence(), 0);
                    assert_eq!(d.original, MediaTime::new(24840, 90000).unwrap());
                    assert_eq!(d.overlap, MediaTime::new(11, 250).unwrap());
                } else {
                    assert!(e.aac_overlap_error().is_none());
                }
                results.push(serde_json::json!({"limit":limit,"error":format!("{:?}",e.kind()),"overlap":e.aac_overlap_error()}));
            }
        }
    }
    serde_json::json!(results)
}
#[cfg(not(target_arch = "wasm32"))]
pub fn native_suite() -> String {
    futures::executor::block_on(suite()).to_string()
}
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub async fn aac_suite() -> String {
    suite().await.to_string()
}
#[cfg(test)]
#[test]
fn aac_contract() {
    native_suite();
}
