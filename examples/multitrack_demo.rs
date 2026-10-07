//! Fixed video, two selected audio tracks and typed cues, native classic output.
//! cargo run --example multitrack_demo -- video.m3u8 en.m3u8 ja.m3u8 output.mp4
#[cfg(all(not(target_arch = "wasm32"), feature = "default-source"))]
mod native {
    use hls_transmux::{crypto::key::*, playlist::*, *};
    use std::sync::Arc;
    struct Keys;
    impl KeyProvider for Keys {
        fn resolve(&self, _: KeyRequest) -> KeyFuture<KeyResolution> {
            Box::pin(async { KeyResolution::Unavailable })
        }
    }
    struct Clock;
    impl KeyClock for Clock {
        fn now(&self) -> u64 {
            0
        }
    }
    struct Wait;
    impl ContinuousWait for Wait {
        fn wait(
            &self,
            d: std::time::Duration,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
            Box::pin(tokio::time::sleep(d))
        }
    }
    pub async fn run() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let args: Vec<_> = std::env::args().skip(1).collect();
        if args.len() != 4 {
            return Err(
                "usage: multitrack_demo video.m3u8 english.m3u8 japanese.m3u8 output.mp4".into(),
            );
        }
        let source = Arc::new(ReqwestSource::new());
        let id = |s| InputId::new(s).unwrap();
        let keys = KeySession::new(
            "multi-demo",
            "clear",
            Arc::new(Keys),
            Arc::new(Clock),
            KeySessionOptions::default(),
        )?;
        let inputs = MultiTrackInputs::new(
            ContinuousInput::new(id("primary"), source.clone()),
            EmbeddedAudio::Exclude,
        )
        .with_audio(
            ContinuousInput::new(id("en"), source.clone()),
            TrackMetadata::new("en", "English").with_default(true),
        )
        .with_audio(
            ContinuousInput::new(id("ja"), source.clone()),
            TrackMetadata::new("ja", "Japanese"),
        )
        .with_subtitle(SubtitleTrack::new(
            id("cc"),
            id("primary"),
            TrackMetadata::new("en", "Captions"),
        ));
        let s = MultiTrackSession::new(
            inputs,
            keys,
            ContinuousOptions::default()
                .with_mode(ContinuousMode::Vod)
                .with_waiter(Arc::new(Wait), std::time::Duration::from_secs(30)),
        )?;
        let h = s.handle();
        for (name, path) in ["primary", "en", "ja"].iter().zip(&args) {
            let text = source
                .read_text(&SourceLocation::File(std::fs::canonicalize(path)?))
                .await?;
            h.accept_snapshot(
                &id(name),
                &parse_playlist_snapshot(&text, PlaylistContext::new(id(name), 0))?,
            )?;
        }
        // Cue times are on the bound input's source clock. Real applications
        // supply cue batches from their subtitle parser, with generation/epoch.
        let cc = h.subtitle_track_id(&id("cc")).unwrap();
        h.accept_cues(
            cc,
            &[SubtitleCue::new(
                0,
                0,
                MediaTime::new(0, 1000)?,
                MediaTime::new(1000, 1000)?,
                "Example caption",
            )
            .with_settings("align:start")],
        )?;
        h.end_subtitles(cc)?;
        let report = s
            .write_to_file(
                &args[3],
                FileOutputOptions::default().with_format(OutputFormat::StreamingMp4),
            )
            .await?;
        println!(
            "{} tracks, {} bytes",
            report.tracks().len(),
            report.media().bytes_written()
        );
        Ok(())
    }
}
#[cfg(all(not(target_arch = "wasm32"), feature = "default-source"))]
#[tokio::main(flavor = "current_thread")]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    native::run().await
}
#[cfg(any(target_arch = "wasm32", not(feature = "default-source")))]
fn main() {}
