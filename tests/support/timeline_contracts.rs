use crate::timeline_budget as budget;
use hls_transmux::{
    crypto::{key::*, resource::*},
    *,
};
use sha2::{Digest, Sha256};
use std::sync::Arc;
// Both runtime suites use the same fixture/provider types.
pub(crate) use crate::contracts::suite::fixtures;
pub async fn run(provider: Arc<dyn KeyProvider>) -> serde_json::Value {
    let mut cases = Vec::new();
    for name in [
        "ts_avc_regular",
        "fmp4_avc_regular",
        "ts_hevc_regular",
        "fmp4_hevc_regular",
        "ts_aac_audio_only",
    ] {
        for encrypted in [false, true] {
            let (_, inputs) = fixtures::pair(name, None, [encrypted, false], true);
            let options = TimelinePrepareOptions::default()
                .with_resources(
                    ResourceOptions::default()
                        .with_encrypted_ranges(EncryptedRangePolicy::CompleteResources),
                )
                .with_range(
                    PresentationRange::new(
                        MediaTime::new(250, 1000).unwrap(),
                        MediaTime::new(1400, 1000).unwrap(),
                    )
                    .unwrap(),
                );
            let (bytes, report) =
                prepare_hls_timeline(inputs, fixtures::keys(provider.clone()), options)
                    .await
                    .unwrap()
                    .into_mp4_bytes()
                    .await
                    .unwrap();
            cases.push(serde_json::json!({"name":name,"encrypted":encrypted,"sha256":format!("{:x}",Sha256::digest(fixtures::canonical(bytes))),"report":report}));
        }
    }
    serde_json::json!({"cases":cases, "budget": budget::run().await})
}
