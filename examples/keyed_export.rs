#![cfg(not(target_arch = "wasm32"))]
//! Export real keyed outputs for independent ffprobe/FFmpeg inspection.
#[path = "../tests/support/keyed_corpus.rs"]
#[allow(dead_code)]
mod suite;
use hls_transmux::*;
use std::{path::PathBuf, sync::Arc};
fn main() {
    let directory = PathBuf::from(std::env::args().nth(1).expect("output directory"));
    std::fs::create_dir_all(&directory).unwrap();
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let mut configurations = suite::corpus::prepared_cases().into_iter().map(|case|(case.name,None)).collect::<Vec<_>>();
        for video in ["ts_avc_regular","ts_hevc_regular","fmp4_avc_regular","fmp4_hevc_regular"] {
            for audio in ["ts_aac_audio_only","fmp4_aac_audio_only"] {configurations.push((video,Some(audio)));}
        }
        let mut manifest=Vec::new();
        for (index,(video,audio)) in configurations.into_iter().enumerate() {
            let (old,_)=suite::pair(video,audio,[true,true],true);
            let (bytes,_)=prepare_hls(old,PrepareOptions::default()).await.unwrap().into_mp4_bytes().await.unwrap();
            let reference=format!("{index}-clear.mp4"); std::fs::write(directory.join(&reference),bytes).unwrap();
            for (mode,format,backend) in [
                ("classic",OutputFormat::Mp4,FinalizeBackend::Native),
                ("fragmented",OutputFormat::FragmentedMp4,FinalizeBackend::Native),
                ("native",OutputFormat::StreamingMp4,FinalizeBackend::Native),
                #[cfg(feature="ffmpeg-finalize")]
                ("ffmpeg",OutputFormat::StreamingMp4,FinalizeBackend::Ffmpeg),
            ] {
                let (_,keyed)=suite::pair(video,audio,[true,true],true);
                let target=format!("{index}-{mode}.mp4");
                prepare_hls_with_keys(keyed,suite::keys(Arc::new(suite::corpus::Provider)),suite::options()).await.unwrap()
                    .write_to_file(directory.join(&target),FileOutputOptions::default().with_format(format).with_finalize_backend(backend)).await.unwrap();
                manifest.push(serde_json::json!({"primary":video,"audio":audio,"mode":mode,"reference":reference,"output":target}));
            }
        }
        std::fs::write(directory.join("manifest.json"),serde_json::to_string_pretty(&manifest).unwrap()).unwrap();
    });
}
