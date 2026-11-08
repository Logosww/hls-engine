//! Regression bindings for the production parser, keys, resources and Engine core.
#![cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]
pub mod aac;
pub mod continuous;
pub mod engine_gcm;
pub mod keys;
pub mod multitrack;
pub mod playlist;
pub mod resources;
pub mod samples;
pub mod subtitles;

#[cfg(not(target_arch = "wasm32"))]
pub fn native_report() -> serde_json::Value {
    fn json(value: String) -> serde_json::Value {
        serde_json::from_str(&value).unwrap()
    }
    serde_json::json!({
        "playlist": json(playlist::fixture()),
        "aac": json(aac::native_suite()),
        "engine_gcm": json(engine_gcm::native_suite()),
        "keys": json(keys::native_suite()),
        "resources": json(resources::native_suite()),
        "subtitles": json(subtitles::native_suite()),
        "samples": json(samples::native_suite()),
        "continuous": json(continuous::native_suite()),
        "multitrack": json(multitrack::native_suite()),
    })
}

#[path = "../../support/allocation.rs"]
pub mod allocation;

pub(crate) use samples::suite as sample_corpus;
#[path = "../../support/sample_profile.rs"]
pub mod sample_profile;

#[path = "../../support/continuous_profile.rs"]
pub mod continuous_profile;

#[path = "../../support/multitrack_profile.rs"]
pub mod multitrack_profile;
