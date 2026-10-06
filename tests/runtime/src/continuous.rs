#[path = "../../support/continuous_runtime.rs"]
mod suite;
use crate::sample_corpus;
#[cfg(not(target_arch = "wasm32"))]
pub fn native_suite() -> String {
    futures::executor::block_on(suite::run(std::sync::Arc::new(sample_corpus::Provider)))
        .to_string()
}
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(js_name=run_continuous)]
pub async fn run(provider: js_sys::Function) -> String {
    suite::run(std::sync::Arc::new(crate::samples::bridge::Provider(
        provider,
    )))
    .await
    .to_string()
}
#[cfg(test)]
mod tests {
    #[test]
    fn continuous_profiles() {
        super::native_suite();
    }
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(js_name=profile_continuous)]
pub async fn profile(count: u32) -> String {
    assert!([8, 64, 256].contains(&count));
    crate::continuous_profile::run_counts(&[u64::from(count)])
        .await
        .to_string()
}
