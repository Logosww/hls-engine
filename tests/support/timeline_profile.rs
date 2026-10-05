use crate::timeline_budget as benchmark;
use std::{cell::RefCell, rc::Rc};
#[cfg(not(target_arch = "wasm32"))]
fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
        * 1000.0
}
#[cfg(target_arch = "wasm32")]
fn now() -> f64 {
    js_sys::Date::now()
}
#[allow(dead_code)]
pub async fn run() -> serde_json::Value {
    let measurements = Rc::new(RefCell::new(Vec::new()));
    let values = measurements.clone();
    let state = Rc::new(RefCell::new((0usize, 0usize, 0.0, None)));
    let observer = Rc::new(move |event: &str, segments: usize, mode: &str| {
        if event == "start" {
            let (live, total) = crate::allocation::reset();
            *state.borrow_mut() = (live, total, now(), None);
        } else if event == "first-write" {
            let mut state = state.borrow_mut();
            state.3 = Some(now() - state.2);
        } else {
            let (live, peak, total) = crate::allocation::snapshot();
            let state = state.borrow();
            values.borrow_mut().push(serde_json::json!({"segments":segments,"mode":mode,
                "peak_increment_bytes":peak.saturating_sub(state.0),"retained_increment_bytes":live.saturating_sub(state.0),
                "allocated_bytes":total.wrapping_sub(state.1),"first_write_ms":state.3,"elapsed_ms":now()-state.2}));
        }
    });
    let cases = benchmark::run_observed(Some(observer)).await;
    serde_json::json!({"cases":cases,"measurements":*measurements.borrow()})
}
