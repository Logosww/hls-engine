//! Export a host-selected media playlist through Engine (no master selection).
#[cfg(feature = "default-source")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use hls_engine::{crypto::key::*, playlist::*, *};
    use std::{sync::Arc, time::Duration};
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
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        return Err("engine_export <selected-playlist> <output> [--fragmented|--batch|--streaming] [--aac-shift-ms N]".into());
    }
    let location = if args[0].starts_with("http") {
        SourceLocation::Url(url::Url::parse(&args[0])?)
    } else {
        SourceLocation::File(std::fs::canonicalize(&args[0])?)
    };
    let mut policy = HttpRequestPolicy::default();
    policy.request_timeout = Some(Duration::from_secs(120));
    policy.max_retries = 0;
    let source = Arc::new(ReqwestSource::new().with_request_policy(policy));
    let text = source.read_text(&location).await?;
    let id = InputId::new("primary")?;
    let snapshot = parse_playlist_snapshot(&text, PlaylistContext::new(id.clone(), 0))?;
    let mut options = EngineOptions::default();
    if let Some(i) = args.iter().position(|s| s == "--aac-shift-ms") {
        let ms = args.get(i + 1).ok_or("missing AAC limit")?.parse()?;
        options = options.with_aac_timestamp_policy(AacTimestampPolicy::RepairOverlaps {
            max_shift: Duration::from_millis(ms),
        });
    }
    options = options.with_on_event(Arc::new(|e| {
        if let EngineEvent::AacTimestampRepaired {
            slot,
            corrected_frames,
            max_shift,
        } = e
        {
            eprintln!(
                "AAC segment={} frames={} max={}/{}",
                slot.sequence(),
                corrected_frames,
                max_shift.ticks(),
                max_shift.timescale()
            );
        }
    }));
    let keys = KeySession::new(
        "export",
        "clear",
        Arc::new(NoKeys),
        Arc::new(Clock),
        KeySessionOptions::default(),
    )?;
    let session = EngineSession::new(
        EngineInputs::new(EngineInput::new(id.clone(), source), EmbeddedAudio::Keep),
        keys,
        options,
    )?;
    session.handle().accept_snapshot(&id, &snapshot)?;
    session.handle().end_input(&id)?;
    let format = if args.iter().any(|s| s == "--fragmented") {
        OutputFormat::FragmentedMp4
    } else if args.iter().any(|s| s == "--batch") {
        OutputFormat::Mp4
    } else {
        OutputFormat::StreamingMp4
    };
    match session
        .write_to_file(&args[1], FileOutputOptions::default().with_format(format))
        .await
    {
        Ok(report) => {
            println!("{report:?}");
            Ok(())
        }
        Err(e) => {
            eprintln!(
                "{e:?}; cause={:?}; repair={:?}; overlap={:?}; resource={:?}",
                e.raw_cause(),
                e.aac_timestamp_error(),
                e.aac_overlap_error(),
                e.resource_error().map(|r| (r.kind(), r.raw_cause()))
            );
            Err(Box::new(e) as Box<dyn std::error::Error>)
        }
    }
}
#[cfg(not(feature = "default-source"))]
fn main() {
    eprintln!("engine_export requires default-source");
}
