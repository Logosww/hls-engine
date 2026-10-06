//! Regression bindings for the production parser, keys, resources and prepared core.
#![cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]
pub mod continuous;
pub mod contracts;
pub mod keys;
pub mod playlist;
pub mod prepared;
pub mod resources;
pub mod samples;
pub mod timeline;

#[cfg(not(target_arch = "wasm32"))]
pub fn native_report() -> serde_json::Value {
    fn json(value: String) -> serde_json::Value {
        serde_json::from_str(&value).unwrap()
    }
    serde_json::json!({
        "playlist": json(playlist::fixture()),
        "keys": json(keys::native_suite()),
        "resources": json(resources::native_suite()),
        "prepared": json(prepared::native_suite()),
        "contracts": json(contracts::native_suite()),
        "timeline": json(timeline::native_suite()),
        "samples": json(samples::native_suite()),
        "continuous": json(continuous::native_suite()),
    })
}

#[path = "../../support/allocation.rs"]
pub mod allocation;
#[path = "../../support/timeline_profile.rs"]
pub mod timeline_profile;

#[path = "../../support/timeline_budget.rs"]
pub mod timeline_budget;

pub(crate) use samples::suite as sample_corpus;
#[path = "../../support/sample_profile.rs"]
pub mod sample_profile;

#[path = "../../support/continuous_profile.rs"]
pub mod continuous_profile;
