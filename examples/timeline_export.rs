//! Export timeline output APIs for independent media regressions. The fixed key is fixture-only.
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
    let mut audio = None;
    let mut split = false;
    let mut options = TimelinePrepareOptions::default();
    let mut arguments = args[2..].iter();
    while let Some(argument) = arguments.next() {
        if argument == "--range-ms" {
            let start = arguments.next().expect("range start").parse().unwrap();
            let end = arguments.next().expect("range end").parse().unwrap();
            options = options.with_range(
                PresentationRange::new(
                    MediaTime::new(start, 1000).unwrap(),
                    MediaTime::new(end, 1000).unwrap(),
                )
                .unwrap(),
            );
        } else if argument == "--split" {
            split = true;
            options = options.with_change_policy(TimelineChangePolicy::Split);
        } else if argument == "--collapse" {
            options = options.with_gap_policy(GapPolicy::Collapse);
        } else {
            assert!(
                audio.is_none(),
                "only one replacement audio input is supported"
            );
            audio = Some(argument);
        }
    }
    std::fs::create_dir_all(&output)?;
    let modes: &[&str] = &[
        "bytes",
        "file",
        "stream",
        #[cfg(feature = "ffmpeg-finalize")]
        "file-ffmpeg",
    ];
    for &mode in modes {
        let mut inputs = KeyedInputs::new(load(&input, "primary")?);
        if let Some(audio) = audio {
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
        let session = prepare_hls_timeline(inputs, keys, options.clone())
            .await
            .unwrap();
        let path = output.join(format!("{mode}.mp4"));
        if split {
            match mode {
                "bytes" => {
                    let (parts, _) = session.into_mp4_outputs().await.unwrap();
                    for (index, bytes) in parts.into_iter().enumerate() {
                        std::fs::write(output.join(format!("{mode}-{index}.mp4")), bytes)?;
                    }
                }
                "file" | "file-ffmpeg" => {
                    let output_options = FileOutputOptions::default();
                    #[cfg(feature = "ffmpeg-finalize")]
                    let output_options = if mode == "file-ffmpeg" {
                        output_options.with_finalize_backend(FinalizeBackend::Ffmpeg)
                    } else {
                        output_options
                    };
                    session
                        .write_to_files(
                            &mut Files {
                                directory: output.clone(),
                                mode: mode.into(),
                            },
                            output_options,
                        )
                        .await
                        .unwrap();
                }
                _ => {
                    session
                        .write_to_outputs(&mut Writers {
                            directory: output.clone(),
                            mode: mode.into(),
                        })
                        .await
                        .unwrap();
                }
            }
            continue;
        }
        match mode {
            "bytes" => std::fs::write(path, session.into_mp4_bytes().await.unwrap().0)?,
            "file" | "file-ffmpeg" => {
                let output_options = FileOutputOptions::default();
                #[cfg(feature = "ffmpeg-finalize")]
                let output_options = if mode == "file-ffmpeg" {
                    output_options.with_finalize_backend(FinalizeBackend::Ffmpeg)
                } else {
                    output_options
                };
                session.write_to_file(path, output_options).await.unwrap();
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

struct Files {
    directory: PathBuf,
    mode: String,
}
impl TimelineFileProvider for Files {
    fn acquire<'a>(
        &'a mut self,
        request: TimelineOutputRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = TimelineResult<PathBuf>> + 'a>> {
        Box::pin(async move {
            Ok(self
                .directory
                .join(format!("{}-{}.mp4", self.mode, request.index())))
        })
    }
}
struct Writers {
    directory: PathBuf,
    mode: String,
}
impl TimelineWriterProvider for Writers {
    type Writer = tokio::fs::File;
    fn acquire<'a>(
        &'a mut self,
        request: TimelineOutputRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = TimelineResult<Self::Writer>> + 'a>>
    {
        Box::pin(async move {
            tokio::fs::File::create(self.directory.join(format!(
                "{}-{}.mp4",
                self.mode,
                request.index()
            )))
            .await
            .map_err(TimelineSessionError::output)
        })
    }
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
