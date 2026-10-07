#[path = "../../../tests/support/timeline_contracts.rs"]
mod suite;
#[cfg(not(target_arch = "wasm32"))]
pub fn native_suite() -> String {
    futures::executor::block_on(suite::run(std::sync::Arc::new(
        suite::fixtures::corpus::Provider,
    )))
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
    use hls_engine::legacy::crypto::key::*;
    use std::sync::Arc;
    use wasm_bindgen::prelude::*;
    struct Provider {
        resolve: js_sys::Function,
    }
    impl KeyProvider for Provider {
        fn resolve(&self, request: KeyRequest) -> KeyFuture<KeyResolution> {
            let second = request.resource().kind() == KeyResourceKind::Media
                && request.resource().slot().sequence() == suite::fixtures::corpus::SEQUENCE + 1;
            let result = self
                .resolve
                .call1(&JsValue::NULL, &JsValue::from_bool(second));
            Box::pin(async move {
                let value = match result {
                    Ok(p) => {
                        wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(&p)).await
                    }
                    Err(e) => Err(e),
                };
                match value {
                    Ok(value) if value.is_instance_of::<js_sys::Uint8Array>() => {
                        KeyResolution::Available(AvailableKey::aes128(
                            SecretKey::new(value.unchecked_into::<js_sys::Uint8Array>().to_vec())
                                .unwrap(),
                        ))
                    }
                    _ => KeyResolution::Unavailable,
                }
            })
        }
    }
    #[wasm_bindgen(js_name=run_timeline)]
    pub async fn run(resolve: js_sys::Function) -> String {
        suite::run(Arc::new(Provider { resolve })).await.to_string()
    }
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub async fn profile_timeline() -> String {
    crate::timeline_profile::run().await.to_string()
}
