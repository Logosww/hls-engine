use super::contracts::suite::fixtures::corpus;
#[cfg(not(target_arch = "wasm32"))]
pub fn native_suite() -> String {
    futures::executor::block_on(corpus::run(std::sync::Arc::new(corpus::Provider))).to_string()
}
#[cfg(test)]
mod tests {
    #[test]
    fn native_contracts() {
        super::native_suite();
    }
}

#[cfg(target_arch = "wasm32")]
mod bridge {
    use super::corpus;
    use hls_engine::legacy::{
        MemorySource, SourceLocation, TextResource,
        crypto::{key::*, resource::*},
        parse_playlist_snapshot,
        playlist::*,
    };
    use std::{
        cell::RefCell,
        sync::{Arc, Weak},
    };
    use wasm_bindgen::prelude::*;
    thread_local! {static CURRENT:RefCell<Weak<ResourceSession>>=const{RefCell::new(Weak::new())};}
    struct Provider {
        resolve: js_sys::Function,
        abort: js_sys::Function,
    }
    impl KeyProvider for Provider {
        fn resolve(&self, request: KeyRequest) -> KeyFuture<KeyResolution> {
            if request.cancellation().is_cancelled() {
                return Box::pin(async { KeyResolution::Unavailable });
            }
            let second = request.resource().kind() == KeyResourceKind::Media
                && request.resource().slot().sequence() == corpus::SEQUENCE + 1;
            let promise=self.resolve.call1(&JsValue::NULL,&serde_json::json!({"second":second,"operation":request.operation(),"revision":request.resolve_revision().to_string(),"sequence":request.resource().slot().sequence().to_string()}).to_string().into());
            Box::pin(async move {
                let result = match promise {
                    Ok(p) => {
                        wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(&p)).await
                    }
                    Err(e) => Err(e),
                };
                match result {
                    Ok(v) if v.is_instance_of::<js_sys::Uint8Array>() => {
                        match SecretKey::new(v.unchecked_into::<js_sys::Uint8Array>().to_vec()) {
                            Ok(key) => {
                                KeyResolution::Available(AvailableKey::aes128(key).with_version(
                                    format!("revision-{}", request.resolve_revision()),
                                ))
                            }
                            Err(_) => failure(),
                        }
                    }
                    _ => failure(),
                }
            })
        }
        fn abort(&self, request: &KeyRequest) {
            let _ = self
                .abort
                .call1(&JsValue::NULL, &request.operation().into());
        }
    }
    fn failure() -> KeyResolution {
        KeyResolution::Failure(ProviderFailure::new(
            ProviderFailureKind::InvalidResponse,
            Arc::new(std::io::Error::other("test bridge response rejected")),
        ))
    }
    #[wasm_bindgen(js_name = cancel_resource)]
    pub fn cancel_active() {
        CURRENT.with(|v| {
            if let Some(s) = v.borrow().upgrade() {
                s.cancel();
            }
        });
    }
    #[wasm_bindgen(js_name = run_resources)]
    pub async fn run_suite(resolve: js_sys::Function, abort: js_sys::Function) -> String {
        let provider = Arc::new(Provider { resolve, abort });
        let report = corpus::run(provider.clone()).await;
        let keys = KeySession::new(
            "cancel",
            "test",
            provider,
            Arc::new(corpus::Clock),
            KeySessionOptions::default(),
        )
        .unwrap();
        let session = Arc::new(ResourceSession::new(keys, ResourceOptions::default()).unwrap());
        CURRENT.with(|v| *v.borrow_mut() = Arc::downgrade(&session));
        let snapshot = parse_playlist_snapshot(
            &TextResource {
                content: include_str!("../../../tests/fixtures/crypto/ts_avc_regular/input.m3u8")
                    .into(),
                location: SourceLocation::Url(
                    url::Url::parse("https://media.test/input.m3u8").unwrap(),
                ),
            },
            PlaylistContext::new(InputId::new("p").unwrap(), 0),
        )
        .unwrap();
        let source = Arc::new(MemorySource::new().segment(
            "https://media.test/seg0.cbc",
            include_bytes!("../../../tests/fixtures/crypto/ts_avc_regular/seg0.cbc").to_vec(),
        ));
        let result = session
            .read(source, ResourceRequest::media(&snapshot, 0).unwrap())
            .await;
        assert_eq!(result.unwrap_err().kind(), ResourceErrorKind::Cancelled);
        assert_eq!(session.stats().reserved_bytes(), 0);
        report.to_string()
    }
}
