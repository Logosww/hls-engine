#![cfg(target_arch = "wasm32")]
//! Small clear-fixture bridge. The bounded resource map belongs to this example;
//! production hosts use a demand-driven Source/Promise bridge (see SDK tests).
use hls_engine::legacy::{crypto::key::*, playlist::*, *};
use std::{
    cell::RefCell,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use wasm_bindgen::prelude::*;
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
fn js(error: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&error.to_string())
}
#[wasm_bindgen]
pub struct MultiRecorder {
    session: RefCell<Option<MultiTrackSession>>,
    handle: MultiTrackHandle,
}
#[wasm_bindgen]
impl MultiRecorder {
    #[wasm_bindgen(constructor)]
    #[allow(clippy::arc_with_non_send_sync)]
    pub fn new(
        resources: js_sys::Object,
        audio_ids: String,
        retain_embedded_audio: bool,
    ) -> std::result::Result<MultiRecorder, JsValue> {
        let mut source = MemorySource::new();
        let mut total = 0u64;
        for entry in js_sys::Object::entries(&resources).iter() {
            let entry = js_sys::Array::from(&entry);
            let url = entry.get(0).as_string().ok_or_else(|| js("invalid URL"))?;
            let bytes = entry.get(1);
            if !bytes.is_instance_of::<js_sys::Uint8Array>() {
                return Err(js("expected Uint8Array"));
            }
            let bytes = js_sys::Uint8Array::new(&bytes);
            total += u64::from(bytes.length());
            if total > 32 * 1024 * 1024 {
                return Err(js("fixture map exceeds 32 MiB"));
            }
            source = source.segment(url, bytes.to_vec());
        }
        let keys = KeySession::new(
            "wasm-demo",
            "clear",
            Arc::new(Keys),
            Arc::new(Clock),
            KeySessionOptions::default(),
        )
        .map_err(js)?;
        let source = Arc::new(source);
        let mut inputs = MultiTrackInputs::new(
            ContinuousInput::new(InputId::new("primary").unwrap(), source.clone()),
            if retain_embedded_audio {
                EmbeddedAudio::Keep
            } else {
                EmbeddedAudio::Exclude
            },
        );
        let ids: Vec<String> = serde_json::from_str(&audio_ids).map_err(js)?;
        for id in ids {
            inputs = inputs.with_audio(
                ContinuousInput::new(InputId::new(&id).map_err(js)?, source.clone()),
                TrackMetadata::new("und", id),
            );
        }
        inputs = inputs.with_subtitle(SubtitleTrack::new(
            InputId::new("cc").unwrap(),
            InputId::new("primary").unwrap(),
            TrackMetadata::new("en", "Captions"),
        ));
        let session = MultiTrackSession::new(
            inputs,
            keys,
            ContinuousOptions::default()
                .with_waiter(Arc::new(Wait), std::time::Duration::from_secs(30)),
        )
        .map_err(js)?;
        let handle = session.handle();
        Ok(Self {
            session: RefCell::new(Some(session)),
            handle,
        })
    }
    pub async fn accept(
        &self,
        input_id: String,
        text: String,
        url: String,
        revision: String,
    ) -> std::result::Result<(), JsValue> {
        let revision = revision
            .parse::<u64>()
            .map_err(|_| js("revision must be a decimal u64 string"))?;
        let id = InputId::new(input_id).map_err(js)?;
        let snapshot = parse_playlist_snapshot(
            &TextResource {
                location: SourceLocation::Url(
                    url::Url::parse(&url).map_err(|_| js("invalid URL"))?,
                ),
                content: text,
            },
            PlaylistContext::new(id.clone(), 0).with_revision(revision),
        )
        .map_err(js)?;
        self.handle
            .control()
            .accept_when_ready(&id, &snapshot)
            .await
            .map_err(js)?;
        Ok(())
    }
    pub fn cues(&self, batch: String) -> std::result::Result<(), JsValue> {
        let cues: Vec<SubtitleCue> = serde_json::from_str(&batch).map_err(js)?;
        let track = self
            .handle
            .subtitle_track_id(&InputId::new("cc").unwrap())
            .unwrap();
        self.handle.accept_cues(track, &cues).map_err(js)?;
        Ok(())
    }
    pub fn end_subtitles(&self) -> std::result::Result<(), JsValue> {
        self.handle
            .end_subtitles(
                self.handle
                    .subtitle_track_id(&InputId::new("cc").unwrap())
                    .unwrap(),
            )
            .map_err(js)
    }
    pub fn stop(&self) {
        self.handle.stop();
    }
    pub fn cancel(&self) {
        self.handle.cancel();
    }
    /// The returned Promise resolves after flush. JS owns sink close/abort.
    pub async fn run(&self, write: js_sys::Function) -> std::result::Result<String, JsValue> {
        let session = self
            .session
            .borrow_mut()
            .take()
            .ok_or_else(|| js("already started"))?;
        let report = session
            .write_to(&mut Writer {
                write,
                pending: None,
            })
            .await
            .map_err(js)?;
        serde_json::to_string(&report).map_err(js)
    }
}
struct Writer {
    write: js_sys::Function,
    pending: Option<(Pin<Box<wasm_bindgen_futures::JsFuture>>, usize)>,
}
impl tokio::io::AsyncWrite for Writer {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if self.pending.is_none() {
            let value = self
                .write
                .call1(&JsValue::NULL, &js_sys::Uint8Array::from(bytes));
            let promise = match value {
                Ok(v) => js_sys::Promise::resolve(&v),
                Err(_) => return Poll::Ready(Err(std::io::Error::other("sink rejected"))),
            };
            self.pending = Some((
                Box::pin(wasm_bindgen_futures::JsFuture::from(promise)),
                bytes.len(),
            ));
        }
        let (future, len) = self.pending.as_mut().unwrap();
        let len = *len;
        match future.as_mut().poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                self.pending = None;
                Poll::Ready(
                    result
                        .map(|_| len)
                        .map_err(|_| std::io::Error::other("sink rejected")),
                )
            }
        }
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Err(std::io::Error::other("caller owns close")))
    }
}

struct Wait;
impl ContinuousWait for Wait {
    fn wait(&self, duration: std::time::Duration) -> Pin<Box<dyn Future<Output = ()> + '_>> {
        Box::pin(async move {
            let promise = js_sys::Promise::new(&mut |resolve, reject| {
                let timer =
                    js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("setTimeout"))
                        .and_then(|v| v.dyn_into::<js_sys::Function>());
                match timer.and_then(|timer| {
                    timer.call2(
                        &JsValue::NULL,
                        &resolve,
                        &JsValue::from_f64(duration.as_millis() as f64),
                    )
                }) {
                    Ok(_) => {}
                    Err(e) => {
                        let _ = reject.call1(&JsValue::NULL, &e);
                    }
                }
            });
            let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
        })
    }
}
