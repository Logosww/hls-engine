//! A finite selected input uses the same EngineSession as open input.
use hls_engine::{crypto::key::*, playlist::*, *};
use std::sync::Arc;
struct Clock;
impl KeyClock for Clock {
    fn now(&self) -> u64 {
        0
    }
}
struct NoKeys;
impl KeyProvider for NoKeys {
    fn resolve(&self, _: KeyRequest) -> KeyFuture<KeyResolution> {
        Box::pin(async { KeyResolution::Unavailable })
    }
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = InputId::new("primary")?;
    let base = url::Url::parse("https://example.test/list.m3u8")?;
    let snapshot = parse_playlist_snapshot(
        &TextResource {
            location: SourceLocation::Url(base),
            content: "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nseg0.ts\n#EXT-X-ENDLIST\n"
                .into(),
        },
        PlaylistContext::new(input.clone(), 0),
    )?;
    let source = MemorySource::new().segment(
        "https://example.test/seg0.ts",
        include_bytes!("../tests/fixtures/media/ts_avc_regular/seg0.ts"),
    );
    let keys = KeySession::new(
        "recording",
        "authorization",
        Arc::new(NoKeys),
        Arc::new(Clock),
        KeySessionOptions::default(),
    )?;
    let session = EngineSession::new(
        EngineInputs::new(
            EngineInput::new(input.clone(), Arc::new(source)),
            EmbeddedAudio::Keep,
        ),
        keys,
        EngineOptions::default(),
    )?;
    session.handle().accept_snapshot(&input, &snapshot)?;
    session.handle().end_input(&input)?;
    let (bytes, report) = session
        .into_bytes(8 * 1024 * 1024, OutputFormat::FragmentedMp4)
        .await?;
    println!("{} bytes, {} tracks", bytes.len(), report.tracks().len());
    Ok(())
}
