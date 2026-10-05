#[cfg(target_arch = "wasm32")]
#[path = "../../../examples/keyed_wasm.rs"]
mod keyed_wasm_example;
#[path = "../../../tests/support/keyed_contracts.rs"]
pub(crate) mod suite;
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
    use hls_transmux::crypto::key::*;
    use std::sync::Arc;
    use wasm_bindgen::prelude::*;
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
                && request.resource().slot().sequence() == suite::fixtures::corpus::SEQUENCE + 1;
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
    #[wasm_bindgen(js_name = run_contracts)]
    pub async fn run_suite(resolve: js_sys::Function, abort: js_sys::Function) -> String {
        let provider = Arc::new(Provider { resolve, abort });
        let report = suite::run(provider.clone()).await;
        report.to_string()
    }
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub async fn run_example(
    resolve: js_sys::Function,
    on_event: js_sys::Function,
) -> Result<js_sys::Uint8Array, wasm_bindgen::JsValue> {
    let source = js_sys::Object::new();
    for (name, bytes) in [
        (
            "seg0.cbc",
            &include_bytes!("../../../tests/fixtures/crypto/ts_avc_regular/seg0.cbc")[..],
        ),
        (
            "seg1.cbc",
            &include_bytes!("../../../tests/fixtures/crypto/ts_avc_regular/seg1.cbc")[..],
        ),
        (
            "clear.bin",
            &include_bytes!("../../../tests/fixtures/media/ts_avc_regular/seg2.ts")[..],
        ),
    ] {
        js_sys::Reflect::set(
            &source,
            &format!("https://media.test/{name}").into(),
            &js_sys::Uint8Array::from(bytes),
        )?;
    }
    keyed_wasm_example::transmux(
        include_str!("../../../tests/fixtures/crypto/ts_avc_regular/input.m3u8").into(),
        "https://media.test/list.m3u8".into(),
        source,
        resolve,
        on_event,
    )
    .await
}
