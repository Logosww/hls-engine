//! Local finite AES-128 export: cargo run --example keyed_demo -- input.m3u8 output.mp4
//! Key files are resolved from KEY URIs. HTTP/CDM authorization belongs to another provider.
#[cfg(all(not(target_arch = "wasm32"), feature = "default-source"))]
mod native {
    use hls_transmux::{crypto::key::*, playlist::*, *};
    use std::sync::Arc;
    use tokio::io::AsyncReadExt;
    struct Clock(std::time::Instant);
    impl KeyClock for Clock {
        fn now(&self) -> u64 {
            u64::try_from(self.0.elapsed().as_millis()).unwrap_or(u64::MAX)
        }
    }
    struct LocalKeys;
    impl KeyProvider for LocalKeys {
        fn resolve(&self, request: KeyRequest) -> KeyFuture<KeyResolution> {
            let location = request.reference().location().location().clone();
            Box::pin(async move {
                let SourceLocation::File(path) = location else {
                    return KeyResolution::Unavailable;
                };
                let read = async {
                    let file = tokio::fs::File::open(path).await?;
                    let mut bytes = zeroize::Zeroizing::new(Vec::new());
                    file.take(17).read_to_end(&mut bytes).await?;
                    Ok::<_, std::io::Error>(std::mem::take(&mut *bytes))
                };
                match read.await {
                    Ok(bytes) => match SecretKey::new(bytes) {
                        Ok(secret) => KeyResolution::Available(AvailableKey::aes128(secret)),
                        Err(error) => KeyResolution::Failure(ProviderFailure::new(
                            ProviderFailureKind::InvalidResponse,
                            Arc::new(error),
                        )),
                    },
                    Err(error) => KeyResolution::Failure(ProviderFailure::new(
                        ProviderFailureKind::Transport,
                        Arc::new(error),
                    )),
                }
            })
        }
    }
    pub async fn run() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let args = std::env::args().skip(1).collect::<Vec<_>>();
        if args.len() != 2 {
            return Err("usage: keyed_demo input.m3u8 output.mp4".into());
        }
        let location = SourceLocation::File(std::fs::canonicalize(&args[0])?);
        let source = Arc::new(ReqwestSource::new());
        let snapshot = parse_playlist_snapshot(
            &source.read_text(&location).await?,
            PlaylistContext::new(InputId::new("primary")?, 0),
        )?;
        let keys = KeySession::new(
            "local-export",
            "local-files",
            Arc::new(LocalKeys),
            Arc::new(Clock(std::time::Instant::now())),
            KeySessionOptions::default(),
        )?;
        let session = prepare_hls_with_keys(
            KeyedInputs::new(KeyedInput::new(snapshot, source)),
            keys,
            KeyedPrepareOptions::default(),
        )
        .await?;
        let query = session.capability_query(capabilities::KeyedOutput::NativeStreamingFile);
        assert!(capabilities::query_keyed_capability(&query).supported());
        let report = session
            .write_to_file(&args[1], FileOutputOptions::default())
            .await?;
        for input in report.inputs() {
            println!(
                "committed={} downloaded={} decrypted={}",
                input.committed_segments(),
                input.downloaded_bytes(),
                input.decrypted_bytes()
            );
        }
        Ok(())
    }
}
#[cfg(all(not(target_arch = "wasm32"), feature = "default-source"))]
#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    native::run().await
}
#[cfg(any(target_arch = "wasm32", not(feature = "default-source")))]
fn main() {
    eprintln!("keyed_demo requires a native target and default-source");
}
