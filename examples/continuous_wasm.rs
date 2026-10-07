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
pub struct Recorder {
    session: RefCell<Option<ContinuousSession>>,
    handle: ContinuousHandle,
}
#[wasm_bindgen]
impl Recorder {
    #[wasm_bindgen(constructor)]
    #[allow(clippy::arc_with_non_send_sync)]
    pub fn new(resources: js_sys::Object) -> std::result::Result<Recorder, JsValue> {
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
        let session = ContinuousSession::new(
            ContinuousInputs::new(ContinuousInput::new(
                InputId::new("primary").unwrap(),
                Arc::new(source),
            )),
            keys,
            ContinuousOptions::default(),
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
        text: String,
        url: String,
        revision: String,
    ) -> std::result::Result<(), JsValue> {
        let revision = revision
            .parse::<u64>()
            .map_err(|_| js("revision must be a decimal u64 string"))?;
        let id = InputId::new("primary").unwrap();
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
            .accept_when_ready(&id, &snapshot)
            .await
            .map_err(js)?;
        Ok(())
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
