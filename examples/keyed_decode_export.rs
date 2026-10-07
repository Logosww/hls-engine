//! Export keyed output APIs, optionally with external audio, for media regressions.
use hls_engine::legacy::{crypto::key::*, playlist::*, *};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

struct Provider;
impl KeyProvider for Provider {
    fn resolve(&self, _: KeyRequest) -> KeyFuture<KeyResolution> {
        Box::pin(async {
            KeyResolution::Available(AvailableKey::aes128(
                SecretKey::new(vec![0x11; 16]).unwrap(),
            ))
        })
    }
}
struct Clock;
impl KeyClock for Clock {
    fn now(&self) -> u64 {
        0
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let input = PathBuf::from(&args[0]);
    let output = PathBuf::from(&args[1]);
    std::fs::create_dir_all(&output)?;
    let modes: &[&str] = if args.len() > 2 {
        &["bytes", "file", "stream", "stream-indexed"]
    } else {
        &["bytes", "file", "stream"]
    };
    for &mode in modes {
        let mut inputs = KeyedInputs::new(load(&input, "primary")?);
        if let Some(audio) = args.get(2) {
            inputs = inputs.with_audio(load(Path::new(audio), "audio")?);
        }
        let keys = KeySession::new(
            "decode-test",
            "isolated",
            Arc::new(Provider),
            Arc::new(Clock),
            KeySessionOptions::default(),
        )
        .unwrap();
        let session = prepare_hls_with_keys(
            inputs,
            keys,
            KeyedPrepareOptions::default().with_write_mfra(mode == "stream-indexed"),
        )
        .await
        .unwrap();
        let path = output.join(format!("{mode}.mp4"));
        match mode {
            "bytes" => std::fs::write(path, session.into_mp4_bytes().await.unwrap().0)?,
            "file" => {
                session
                    .write_to_file(path, FileOutputOptions::default())
                    .await
                    .unwrap();
            }
            _ => {
                let mut bytes = Vec::new();
                session.write_to(&mut bytes).await.unwrap();
                std::fs::write(path, bytes)?;
            }
        }
    }
    Ok(())
}

fn load(input: &Path, role: &str) -> Result<KeyedInput> {
    let text = std::fs::read_to_string(input.join("media.m3u8"))?;
    let mut source = MemorySource::new();
    for entry in std::fs::read_dir(input)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            source = source.segment(
                format!(
                    "https://fixture.invalid/{role}/{}",
                    entry.file_name().to_string_lossy()
                ),
                std::fs::read(entry.path())?,
            );
        }
    }
    let snapshot = parse_playlist_snapshot(
        &TextResource {
            content: text,
            location: SourceLocation::Url(
                format!("https://fixture.invalid/{role}/media.m3u8")
                    .parse()
                    .unwrap(),
            ),
        },
        PlaylistContext::new(InputId::new(role).unwrap(), 0),
    )
    .unwrap();
    Ok(KeyedInput::new(snapshot, Arc::new(source)))
}
