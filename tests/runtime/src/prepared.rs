use super::contracts::suite::fixtures as suite;
#[cfg(not(target_arch = "wasm32"))]
pub fn native_suite() -> String {
    futures::executor::block_on(suite::run(std::sync::Arc::new(suite::corpus::Provider)))
        .to_string()
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
    use super::suite;
    use hls_engine::legacy::{crypto::key::*, *};
    use std::{
        cell::RefCell,
        sync::{Arc, Weak},
    };
    use wasm_bindgen::prelude::*;
    thread_local! {static CURRENT:RefCell<Weak<Cancel>>=const{RefCell::new(Weak::new())};}
    #[derive(Debug)]
    struct Cancel(tokio::sync::watch::Sender<bool>);
    impl CancelToken for Cancel {
        fn is_cancelled(&self) -> bool {
            *self.0.borrow()
        }
        fn cancelled(
            &self,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
            let mut rx = self.0.subscribe();
            Box::pin(async move {
                rx.wait_for(|v| *v).await.unwrap();
            })
        }
    }
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
                && request.resource().slot().sequence() == suite::corpus::SEQUENCE + 1;
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
    #[wasm_bindgen(js_name = cancel_prepared)]
    pub fn cancel_active() {
        CURRENT.with(|v| {
            if let Some(s) = v.borrow().upgrade() {
                s.0.send_replace(true);
            }
        });
    }
    #[wasm_bindgen(js_name = run_prepared)]
    pub async fn run_suite(resolve: js_sys::Function, abort: js_sys::Function) -> String {
        let provider = Arc::new(Provider { resolve, abort });
        let report = suite::run(provider.clone()).await;
        let keys = KeySession::new(
            "cancel",
            "test",
            provider,
            Arc::new(suite::corpus::Clock),
            KeySessionOptions::default(),
        )
        .unwrap();
        let cancel = Arc::new(Cancel(tokio::sync::watch::channel(false).0));
        CURRENT.with(|v| *v.borrow_mut() = Arc::downgrade(&cancel));
        let (_, inputs) = suite::pair("ts_avc_regular", None, [true, false], false);
        let result =
            prepare_hls_with_keys(inputs, keys, suite::options().with_cancel(cancel)).await;
        assert_eq!(
            result.err().unwrap().kind(),
            KeyedSessionErrorKind::Cancelled
        );
        report.to_string()
    }
}
