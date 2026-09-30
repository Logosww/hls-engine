use hls_transmux::*;
use std::future::{Future, pending};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::AsyncWrite;

#[derive(Debug)]
struct Token(tokio::sync::watch::Sender<bool>);
impl Token {
    fn new() -> Arc<Self> {
        Arc::new(Self(tokio::sync::watch::channel(false).0))
    }
    fn trigger(&self) {
        self.0.send_replace(true);
    }
}
impl CancelToken for Token {
    fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        let mut rx = self.0.subscribe();
        Box::pin(async move {
            let _ = rx.wait_for(|v| *v).await;
        })
    }
}

#[derive(Debug)]
struct PendingSource {
    phase: &'static str,
    entered: Arc<tokio::sync::Notify>,
}
impl Source for PendingSource {
    fn read_text<'a>(
        &'a self,
        location: &'a SourceLocation,
    ) -> Pin<Box<dyn Future<Output = Result<TextResource>> + Send + 'a>> {
        Box::pin(async move {
            if self.phase == "playlist" {
                self.entered.notify_one();
                return pending().await;
            }
            if self.phase == "variant" {
                if matches!(location, SourceLocation::File(p) if p.ends_with("variant.m3u8")) {
                    self.entered.notify_one();
                    return pending().await;
                }
                return Ok(TextResource {
                    content: "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nvariant.m3u8\n".into(),
                    location: location.clone(),
                });
            }
            let map = if self.phase == "init" {
                "#EXT-X-MAP:URI=\"init.mp4\"\n"
            } else {
                ""
            };
            Ok(TextResource {
                content: format!(
                    "#EXTM3U\n#EXT-X-TARGETDURATION:10\n{map}#EXTINF:10,\nsegment.ts\n#EXT-X-ENDLIST\n"
                ),
                location: location.clone(),
            })
        })
    }
    fn read_bytes<'a>(
        &'a self,
        location: &'a SourceLocation,
        _: Option<&'a ByteRange>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>>> + Send + 'a>> {
        Box::pin(async move {
            let init = matches!(location, SourceLocation::File(p) if p.ends_with("init.mp4"));
            if self.phase == "media" || (self.phase == "init" && init) {
                self.entered.notify_one();
                return pending().await;
            }
            Ok(include_bytes!("fixtures/h264_aac_fhd.ts").to_vec())
        })
    }
}
fn input(phase: &'static str, entered: Arc<tokio::sync::Notify>) -> HlsInput {
    HlsInput::custom(
        Arc::new(PendingSource { phase, entered }),
        SourceLocation::File("playlist.m3u8".into()),
    )
}

#[tokio::test]
async fn pending_playlist_variant_init_and_media_cancel_within_one_second() {
    for phase in ["playlist", "variant", "init", "media"] {
        let entered = Arc::new(tokio::sync::Notify::new());
        let token = Token::new();
        let work = tokio::spawn(transmux_hls_to_mp4_bytes(
            input(phase, entered.clone()),
            TransmuxOptions {
                variant: Some(VariantSelection::Index(0)),
                cancel: Some(token.clone()),
                ..Default::default()
            },
        ));
        tokio::time::timeout(Duration::from_secs(1), entered.notified())
            .await
            .unwrap();
        token.trigger();
        let result = tokio::time::timeout(Duration::from_secs(1), work)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(result, Err(Error::Cancelled)), "{phase}");
    }
}

#[tokio::test]
async fn already_cancelled_does_not_start_reading() {
    let token = Token::new();
    token.trigger();
    let result = transmux_hls_to_mp4_bytes(
        input("playlist", Arc::new(tokio::sync::Notify::new())),
        TransmuxOptions {
            cancel: Some(token),
            ..Default::default()
        },
    )
    .await;
    assert!(matches!(result, Err(Error::Cancelled)));
}

struct Sink {
    mode: &'static str,
    entered: Arc<tokio::sync::Notify>,
    written: usize,
}
impl AsyncWrite for Sink {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if self.mode == "write" {
            self.entered.notify_one();
            return Poll::Pending;
        }
        if self.mode == "short" && self.written > 128 {
            return Poll::Ready(Err(std::io::Error::other("injected short write failure")));
        }
        let n = data.len().min(64);
        self.written += n;
        Poll::Ready(Ok(n))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.entered.notify_one();
        if self.mode == "flush-error" {
            Poll::Ready(Err(std::io::Error::other("injected flush failure")))
        } else {
            Poll::Pending
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.poll_flush(cx)
    }
}

#[tokio::test]
async fn blocked_sink_write_and_flush_cancel_without_checkpoint() {
    for mode in ["write", "flush"] {
        let entered = Arc::new(tokio::sync::Notify::new());
        let token = Token::new();
        let cancel = token.clone();
        let events = Arc::new(Mutex::new(Vec::new()));
        let saved = events.clone();
        let mut sink = Sink {
            mode,
            entered: entered.clone(),
            written: 0,
        };
        let work = tokio::spawn(async move {
            transmux_hls_to_writer_async(
                input("ready", Arc::new(tokio::sync::Notify::new())),
                &mut sink,
                TransmuxOptions {
                    output_format: OutputFormat::FragmentedMp4,
                    cancel: Some(token),
                    on_progress: Some(Arc::new(move |p| saved.lock().unwrap().push(p))),
                    ..Default::default()
                },
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(1), entered.notified())
            .await
            .unwrap();
        cancel.trigger();
        let result = tokio::time::timeout(Duration::from_secs(1), work)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(result, Err(Error::Cancelled)));
        assert!(events.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn short_write_and_flush_failure_do_not_publish_checkpoint() {
    for mode in ["short", "flush-error"] {
        let events = Arc::new(Mutex::new(Vec::new()));
        let saved = events.clone();
        let mut sink = Sink {
            mode,
            entered: Arc::new(tokio::sync::Notify::new()),
            written: 0,
        };
        let result = transmux_hls_to_writer_async(
            input("ready", Arc::new(tokio::sync::Notify::new())),
            &mut sink,
            TransmuxOptions {
                output_format: OutputFormat::FragmentedMp4,
                on_progress: Some(Arc::new(move |p| saved.lock().unwrap().push(p))),
                ..Default::default()
            },
        )
        .await;
        assert!(matches!(result, Err(Error::Io(_))));
        assert!(events.lock().unwrap().is_empty());
    }
}
