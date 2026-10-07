//! cargo run --example prepared_demo -- primary.m3u8 audio.m3u8 output.mp4 [bytes|fragmented|native|ffmpeg]
use hls_engine::legacy::*;
#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        return Err("expected primary audio output [mode]".into());
    }
    let inputs = HlsInputs::new(HlsInput::Path(args[0].clone().into()))
        .with_audio(HlsInput::Path(args[1].clone().into()));
    let session = prepare_hls(inputs, PrepareOptions::default()).await?;
    println!("{:?}", session.info());
    let mode = args.get(3).map(String::as_str).unwrap_or("native");
    if mode == "bytes" {
        let (bytes, report) = session.into_mp4_bytes().await?;
        tokio::fs::write(&args[2], bytes).await?;
        println!("{:?}", report);
    } else {
        let options = if mode == "fragmented" {
            FileOutputOptions::default().with_format(OutputFormat::FragmentedMp4)
        } else {
            FileOutputOptions::default()
        };
        #[cfg(feature = "ffmpeg-finalize")]
        let options = if mode == "ffmpeg" {
            options.with_finalize_backend(FinalizeBackend::Ffmpeg)
        } else {
            options
        };
        println!("{:?}", session.write_to_file(&args[2], options).await?);
    }
    Ok(())
}
