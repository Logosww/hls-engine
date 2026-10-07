//! Cross-version executable built only by verify_legacy_checkpoint.py.
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
#[derive(Debug)]
struct Cancel(Arc<AtomicBool>);
impl old::CancelToken for Cancel {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    fn cancelled(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}
fn canonical(bytes: &mut [u8]) {
    let mut at = 0;
    while at < bytes.len() {
        let len = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let kind: [u8; 4] = bytes[at + 4..at + 8].try_into().unwrap();
        let body = &mut bytes[at + 8..at + len];
        match &kind {
            b"moov" | b"trak" | b"mdia" => canonical(body),
            b"mvhd" | b"mdhd" | b"tkhd" => {
                let end = if body[0] == 1 { 20 } else { 12 };
                body[4..end].fill(0);
            }
            _ => {}
        }
        at += len;
    }
}
#[tokio::main(flavor = "current_thread")]
async fn main() {
    use current::legacy as engine;
    let root = PathBuf::from(std::env::args_os().nth(1).unwrap());
    let input = root.join("input.m3u8");
    for classic in [false, true] {
        let suffix = if classic { "classic" } else { "fragmented" };
        let output = root.join(format!("resumed-{suffix}.mp4"));
        let expected_path = root.join(format!("old-{suffix}.mp4"));
        let format = if classic {
            old::OutputFormat::StreamingMp4
        } else {
            old::OutputFormat::FragmentedMp4
        };
        old::transmux_hls_to_mp4_async(
            old::HlsInput::Path(input.clone()),
            &expected_path,
            old::TransmuxOptions {
                output_format: format,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let captured = Arc::new(Mutex::new(None));
        let checkpoint = captured.clone();
        let error = old::transmux_hls_to_mp4_async(
            old::HlsInput::Path(input.clone()),
            &output,
            old::TransmuxOptions {
                output_format: format,
                checkpoint_durability: old::CheckpointDurability::SyncAll,
                cancel: Some(Arc::new(Cancel(cancelled))),
                on_progress: Some(Arc::new(move |p| {
                    if p.stage == old::TransmuxStage::Downloading && p.completed_segments == 1 {
                        *checkpoint.lock().unwrap() =
                            Some(serde_json::to_string(&p.resume).unwrap());
                        flag.store(true, Ordering::SeqCst);
                    }
                })),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, old::Error::Cancelled));
        let wire = captured.lock().unwrap().clone().unwrap();
        std::fs::write(root.join(format!("old-{suffix}.checkpoint.json")), &wire).unwrap();
        let checkpoint: engine::TransmuxResumeState = serde_json::from_str(&wire).unwrap();
        assert_eq!(checkpoint.schema_version, 1);
        engine::transmux_hls_to_mp4_async(
            engine::HlsInput::Path(input.clone()),
            &output,
            engine::TransmuxOptions {
                output_format: if classic {
                    engine::OutputFormat::StreamingMp4
                } else {
                    engine::OutputFormat::FragmentedMp4
                },
                resume: Some(checkpoint),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let mut expected = std::fs::read(expected_path).unwrap();
        let mut actual = std::fs::read(output).unwrap();
        canonical(&mut expected);
        canonical(&mut actual);
        assert_eq!(actual, expected);
    }
    println!("{{\"schema1OldArtifactResumed\":true,\"layouts\":2,\"oldOutputEqual\":true}}");
}
