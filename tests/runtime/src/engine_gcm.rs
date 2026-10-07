#[path = "../../support/engine_gcm.rs"]
mod shared;

#[cfg(not(target_arch = "wasm32"))]
pub fn native_suite() -> String {
    futures::executor::block_on(shared::suite()).to_string()
}
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub async fn engine_gcm_suite() -> String {
    shared::suite().await.to_string()
}

#[cfg(target_arch = "wasm32")]
mod bridge {
    use super::shared;
    use hls_engine::{crypto::key::*, *};
    use std::{
        future::Future,
        pin::Pin,
        sync::Arc,
        task::{Context, Poll},
    };
    use wasm_bindgen::{JsCast, prelude::*};
    use wasm_bindgen_futures::JsFuture;
    struct Provider {
        resolve: js_sys::Function,
        abort: js_sys::Function,
    }
    impl KeyProvider for Provider {
        fn resolve(&self, request: KeyRequest) -> KeyFuture<KeyResolution> {
            let uri = match request.reference().location().location() {
                SourceLocation::Url(url) => url.as_str(),
                _ => "",
            };
            let result = self.resolve.call1(
                &JsValue::NULL,
                &serde_json::json!({
                    "method":request.reference().method().as_str(),"uri":uri,
                    "sequence":request.resource().slot().sequence().to_string()
                })
                .to_string()
                .into(),
            );
            Box::pin(async move {
                let value = match result {
                    Ok(value) => JsFuture::from(js_sys::Promise::resolve(&value)).await,
                    Err(error) => Err(error),
                };
                if let Ok(value) = value
                    && value.is_instance_of::<js_sys::Uint8Array>()
                    && let Ok(key) =
                        SecretKey::aes256(value.unchecked_into::<js_sys::Uint8Array>().to_vec())
                {
                    return KeyResolution::Available(
                        AvailableKey::aes256_gcm(key).with_version("promise-v1"),
                    );
                }
                KeyResolution::Failure(ProviderFailure::new(
                    ProviderFailureKind::InvalidResponse,
                    Arc::new(std::io::Error::other(
                        "GCM provider bridge rejected response",
                    )),
                ))
            })
        }
        fn abort(&self, _: &KeyRequest) {
            let _ = self.abort.call0(&JsValue::NULL);
        }
    }
    struct Writer {
        write: js_sys::Function,
        pending: Option<(JsFuture, usize)>,
    }
    impl tokio::io::AsyncWrite for Writer {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            data: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            if self.pending.is_none() {
                match self
                    .write
                    .call1(&JsValue::NULL, &js_sys::Uint8Array::from(data))
                {
                    Ok(value) => {
                        self.pending =
                            Some((JsFuture::from(js_sys::Promise::resolve(&value)), data.len()))
                    }
                    Err(_) => {
                        return Poll::Ready(Err(std::io::Error::other("write callback failed")));
                    }
                }
            }
            let (future, len) = self.pending.as_mut().unwrap();
            match Pin::new(future).poll(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(result) => {
                    let len = *len;
                    self.pending = None;
                    Poll::Ready(
                        result
                            .map(|_| len)
                            .map_err(|_| std::io::Error::other("write Promise failed")),
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
    #[wasm_bindgen]
    pub async fn engine_gcm_stream(
        resolve: js_sys::Function,
        write: js_sys::Function,
        abort: js_sys::Function,
        cancel: js_sys::Promise,
    ) -> String {
        let session = shared::rotation_with_provider(
            true,
            EngineOptions::default(),
            None,
            Arc::new(Provider { resolve, abort }),
        );
        session
            .handle()
            .accept_snapshot(&shared::input_id(), &shared::snapshot(true, true))
            .unwrap();
        let mut writer = Writer {
            write,
            pending: None,
        };
        tokio::select! { biased;
            _ = JsFuture::from(cancel) => serde_json::json!({"error":"Cancelled"}).to_string(),
            result = session.write_to(&mut writer) => match result {
                Ok(report) => serde_json::json!({"bytes":report.media().bytes_written().to_string()}).to_string(),
                Err(error) => serde_json::json!({"error":format!("{:?}",error.kind())}).to_string(),
            }
        }
    }
}
