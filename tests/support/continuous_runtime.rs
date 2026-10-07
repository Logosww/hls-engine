use super::sample_corpus as sample;
use hls_engine::legacy::{crypto::key::*, playlist::*, *};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};

pub async fn run(provider: Arc<dyn KeyProvider>) -> serde_json::Value {
    let mut results = Vec::new();
    for case in sample::cases() {
        for (name, format) in [
            ("fragmented", OutputFormat::FragmentedMp4),
            ("classic", OutputFormat::Mp4),
        ] {
            let id = InputId::new("primary").unwrap();
            let mut source = MemorySource::new();
            for (file, bytes) in case.files {
                source = source.segment(format!("https://continuous.test/{file}"), *bytes);
            }
            let holder = Arc::new(Mutex::new(None::<ContinuousHandle>));
            let callback = holder.clone();
            let options = ContinuousOptions::default().with_on_event(Arc::new(move |event| {
                if matches!(event, ContinuousEvent::Committed { .. }) {
                    callback.lock().unwrap().as_ref().unwrap().stop();
                }
            }));
            let session = ContinuousSession::new(
                ContinuousInputs::new(ContinuousInput::new(id.clone(), Arc::new(source))),
                sample::keys(provider.clone()),
                options,
            )
            .unwrap();
            let handle = session.handle();
            *holder.lock().unwrap() = Some(handle.clone());
            let text = case
                .playlist
                .lines()
                .filter(|l| {
                    !l.starts_with("#EXT-X-ENDLIST") && !l.starts_with("#EXT-X-PLAYLIST-TYPE")
                })
                .collect::<Vec<_>>()
                .join("\n");
            let snapshot = parse_playlist_snapshot(
                &TextResource {
                    location: SourceLocation::Url(
                        url::Url::parse("https://continuous.test/input.m3u8").unwrap(),
                    ),
                    content: text,
                },
                PlaylistContext::new(id.clone(), 9_007_199_254_740_993)
                    .with_revision(9_007_199_254_740_994),
            )
            .unwrap();
            handle.accept_snapshot(&id, &snapshot).unwrap();
            let (bytes, report) = session.into_bytes(8 * 1024 * 1024, format).await.unwrap();
            assert_eq!(report.end_reason(), ContinuousEndReason::Stop);
            assert_eq!(
                report.inputs()[0].committed(),
                snapshot.segments().len() as u64
            );
            #[cfg(not(target_arch = "wasm32"))]
            if let Ok(dir) = std::env::var("HLS_CONTINUOUS_OUTPUT") {
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(
                    std::path::Path::new(&dir).join(format!("{}-{name}.mp4", case.name)),
                    &bytes,
                )
                .unwrap();
            }
            let mut wire = serde_json::to_value(&report).unwrap();
            // Allocation sizes depend on pointer width; semantic reports must not.
            wire.as_object_mut().unwrap().remove("peaks");
            let hash = format!("{:x}", Sha256::digest(sample::canonical(bytes)));
            results.push(serde_json::json!({"name":format!("{}-{name}",case.name),"hash":hash,"report":wire}));
        }
    }
    serde_json::json!(results)
}
