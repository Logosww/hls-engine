//! Poll a local clear playlist until ENDLIST, writing continuous fMP4.
//! cargo run --example continuous_demo -- input.m3u8 output.fmp4
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
    pub async fn run() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let args: Vec<_> = std::env::args().skip(1).collect();
        if args.len() != 2 {
            return Err("usage: continuous_demo input.m3u8 output.fmp4".into());
        }
        let location = SourceLocation::File(std::fs::canonicalize(&args[0])?);
        let source = Arc::new(ReqwestSource::new());
        let id = InputId::new("primary")?;
        let keys = KeySession::new(
            "demo",
            "clear",
            Arc::new(Keys),
            Arc::new(Clock),
            KeySessionOptions::default(),
        )?;
        let session = ContinuousSession::new(
            ContinuousInputs::new(ContinuousInput::new(id.clone(), source.clone())),
            keys,
            ContinuousOptions::default(),
        )?;
        let handle = session.handle();
        let producer = async {
            let mut revision = 0u64;
            loop {
                let text = source.read_text(&location).await?;
                let snapshot = parse_playlist_snapshot(
                    &text,
                    PlaylistContext::new(id.clone(), 0).with_revision(revision),
                )?;
                handle.accept_when_ready(&id, &snapshot).await?;
                if snapshot.end_list() {
                    break;
                }
                revision = revision.checked_add(1).ok_or("revision overflow")?;
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            Ok::<_, Box<dyn std::error::Error>>(())
        };
        let consumer = async {
            Ok::<_, Box<dyn std::error::Error>>(
                session
                    .write_to_file(
                        &args[1],
                        FileOutputOptions::default().with_format(OutputFormat::FragmentedMp4),
                    )
                    .await?,
            )
        };
        let (_, report) = tokio::try_join!(producer, consumer)?;
        println!(
            "{:?}: {} bytes",
            report.end_reason(),
            report.bytes_written()
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
