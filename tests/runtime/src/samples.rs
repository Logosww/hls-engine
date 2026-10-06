#[path = "../../support/sample_crypto.rs"]
#[allow(dead_code)]
pub(crate) mod suite;
#[cfg(not(target_arch = "wasm32"))]
pub fn native_suite() -> String {
    futures::executor::block_on(suite::run(std::sync::Arc::new(suite::Provider))).to_string()
}
#[cfg(test)]
mod tests {
    #[test]
    fn independent_samples() {
        super::native_suite();
    }
}
#[cfg(target_arch = "wasm32")]
pub(crate) mod bridge {
    use super::suite;
    use hls_transmux::{crypto::key::*, playlist::EncryptionMethod};
    use wasm_bindgen::prelude::*;
    pub(crate) struct Provider(pub(crate) js_sys::Function);
    impl KeyProvider for Provider {
        fn resolve(&self, r: KeyRequest) -> KeyFuture<KeyResolution> {
            let kid = r.resource().kid();
            let method = r.reference().method().clone();
            let result = self.0.call2(
                &JsValue::NULL,
                &JsValue::from_str(method.as_str()),
                &kid.map(|k| {
                    JsValue::from_str(&k.iter().map(|b| format!("{b:02x}")).collect::<String>())
                })
                .unwrap_or(JsValue::NULL),
            );
            Box::pin(async move {
                let v = match result {
                    Ok(p) => {
                        wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(&p)).await
                    }
                    Err(e) => Err(e),
                };
                match v {
                    Ok(v) if v.is_instance_of::<js_sys::Uint8Array>() => {
                        let secret =
                            SecretKey::new(v.unchecked_into::<js_sys::Uint8Array>().to_vec())
                                .unwrap();
                        let mut key = match method {
                            EncryptionMethod::SampleAes => AvailableKey::sample_aes(secret),
                            EncryptionMethod::SampleAesCtr => AvailableKey::sample_aes_ctr(secret),
                            _ => AvailableKey::aes128(secret),
                        };
                        if let Some(kid) = kid {
                            key = key.with_kid(kid);
                        }
                        KeyResolution::Available(key.with_version("fixture"))
                    }
                    _ => KeyResolution::Unavailable,
                }
            })
        }
    }
    #[wasm_bindgen(js_name=profile_samples)]
    pub async fn profile(resolve: js_sys::Function) -> String {
        crate::sample_profile::run(std::sync::Arc::new(Provider(resolve)))
            .await
            .to_string()
    }
    #[wasm_bindgen(js_name=run_samples)]
    pub async fn run(resolve: js_sys::Function) -> String {
        suite::run(std::sync::Arc::new(Provider(resolve)))
            .await
            .to_string()
    }
}
