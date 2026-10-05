//! Finite presentation ranges and immutable epoch mappings.
#![doc = include_str!("../../../docs/timeline-sessions.md")]
use super::*;
use crate::crypto::{key::KeySession, resource::*};
use crate::playlist::{InputId, SegmentSlot};
use std::{cmp::Ordering, future::Future, pin::Pin};

mod catalog;
mod engine;
mod model;
mod output;
#[cfg(feature = "serde")]
mod wire;
pub use model::*;
pub use output::{TimelineFileProvider, TimelineOutputRequest, TimelineWriterProvider};

/// An owned operation. Preparation validates manifests without changing legacy profiles.
pub struct TimelinePreparedTransmux {
    inputs: Vec<KeyedInput>,
    resources: Arc<ResourceSession>,
    options: TimelinePrepareOptions,
}

/// Prepare finite selected clear/AES-128 inputs for the timeline profile.
#[cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]
pub async fn prepare_hls_timeline(
    inputs: KeyedInputs,
    keys: KeySession,
    options: TimelinePrepareOptions,
) -> TimelineResult<TimelinePreparedTransmux> {
    options.check()?;
    let KeyedInputs { primary, audio } = inputs;
    let mut inputs = vec![primary];
    if let Some(audio) = audio {
        inputs.push(audio);
    }
    prepare_selected(inputs, keys, options)
}

// WASM operations intentionally retain non-Send host providers behind shared ownership.
#[cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]
fn prepare_selected(
    mut inputs: Vec<KeyedInput>,
    keys: KeySession,
    options: TimelinePrepareOptions,
) -> TimelineResult<TimelinePreparedTransmux> {
    if inputs.len() == 2
        && inputs[0].snapshot.context().input_id() == inputs[1].snapshot.context().input_id()
    {
        return Err(fail(TimelineErrorKind::InvalidOptions));
    }
    for input in &inputs {
        input
            .snapshot
            .validate_timeline_vod()
            .map_err(|_| fail(TimelineErrorKind::UnsupportedPlaylist))?;
    }
    for anchor in &options.anchors {
        if !inputs.iter().any(|input| {
            input.snapshot.context().input_id() == &anchor.input
                && input
                    .snapshot
                    .segments()
                    .iter()
                    .any(|segment| segment.slot().epoch() == anchor.epoch)
        }) {
            return Err(fail(TimelineErrorKind::InvalidOptions));
        }
    }
    for input in &inputs {
        for (index, segment) in input.snapshot.segments().iter().enumerate() {
            if segment.gap() {
                continue;
            }
            for map in [false, true] {
                if map && segment.map().is_none() {
                    continue;
                }
                let request = ResourceRequest::from_validated(&input.snapshot, index, map)
                    .map_err(resource_error)?;
                options
                    .resources
                    .preflight(&keys, &request)
                    .map_err(resource_error)?;
            }
        }
    }
    let resources =
        Arc::new(ResourceSession::new(keys, options.resources.clone()).map_err(resource_error)?);
    for input in &mut inputs {
        let settings = SourceSessionOptions {
            demand_driven: true,
            max_resource_bytes: Some(options.resources.max_resource_bytes()),
        };
        input.source = input
            .source
            .create_session_with_options(&settings)
            .unwrap_or_else(|| input.source.clone());
    }
    Ok(TimelinePreparedTransmux {
        inputs,
        resources,
        options,
    })
}

fn fail(kind: TimelineErrorKind) -> TimelineSessionError {
    TimelineSessionError {
        kind,
        slot: None,
        cause: None,
        resource: None,
        completed: Vec::new(),
    }
}
fn media_error(error: Error) -> TimelineSessionError {
    let kind = if matches!(error, Error::Cancelled) {
        TimelineErrorKind::Cancelled
    } else {
        TimelineErrorKind::Media
    };
    TimelineSessionError {
        cause: Some(error),
        ..fail(kind)
    }
}
fn resource_error(error: ResourceError) -> TimelineSessionError {
    let kind = if error.kind() == ResourceErrorKind::Cancelled {
        TimelineErrorKind::Cancelled
    } else {
        TimelineErrorKind::Resource
    };
    TimelineSessionError {
        resource: Some(Box::new(error)),
        ..fail(kind)
    }
}

fn cmp(a: MediaTime, b: MediaTime) -> TimelineResult<Ordering> {
    let (sa, sb) = (i128::from(a.timescale), i128::from(b.timescale));
    let whole = a.ticks.div_euclid(sa).cmp(&b.ticks.div_euclid(sb));
    if whole != Ordering::Equal {
        return Ok(whole);
    }
    Ok((a.ticks.rem_euclid(sa) * sb).cmp(&(b.ticks.rem_euclid(sb) * sa)))
}
fn add(a: MediaTime, b: MediaTime) -> TimelineResult<MediaTime> {
    combine(a, b, false)
}
fn sub(a: MediaTime, b: MediaTime) -> TimelineResult<MediaTime> {
    combine(a, b, true)
}
fn reduced(time: MediaTime) -> MediaTime {
    let mut a = time.timescale;
    let mut b = time.ticks.rem_euclid(i128::from(time.timescale)) as u32;
    while b != 0 {
        (a, b) = (b, a % b);
    }
    MediaTime {
        ticks: time.ticks / i128::from(a),
        timescale: time.timescale / a,
    }
}
fn combine(a: MediaTime, b: MediaTime, subtract: bool) -> TimelineResult<MediaTime> {
    let (a, b) = (reduced(a), reduced(b));
    fn gcd(mut a: u32, mut b: u32) -> u32 {
        while b != 0 {
            (a, b) = (b, a % b);
        }
        a
    }
    let common = gcd(a.timescale, b.timescale);
    let scale = (a.timescale / common)
        .checked_mul(b.timescale)
        .ok_or_else(|| fail(TimelineErrorKind::TimeOverflow))?;
    let left = a.ticks.checked_mul(i128::from(scale / a.timescale));
    let right = b.ticks.checked_mul(i128::from(scale / b.timescale));
    let ticks = left
        .zip(right)
        .and_then(|(a, b)| {
            if subtract {
                a.checked_sub(b)
            } else {
                a.checked_add(b)
            }
        })
        .ok_or_else(|| fail(TimelineErrorKind::TimeOverflow))?;
    Ok(reduced(MediaTime {
        ticks,
        timescale: scale,
    }))
}
fn rescale(value: MediaTime, scale: u32) -> TimelineResult<i128> {
    let value = reduced(value);
    let mut a = value.timescale;
    let mut b = scale;
    while b != 0 {
        (a, b) = (b, a % b);
    }
    let denominator = i128::from(value.timescale / a);
    let numerator = i128::from(scale / a);
    (value.ticks / denominator)
        .checked_mul(numerator)
        .and_then(|whole| whole.checked_add((value.ticks % denominator) * numerator / denominator))
        .ok_or_else(|| fail(TimelineErrorKind::TimeOverflow))
}
fn zero() -> MediaTime {
    MediaTime {
        ticks: 0,
        timescale: 1,
    }
}

impl Drop for TimelinePreparedTransmux {
    fn drop(&mut self) {
        for input in &self.inputs {
            input.source.stop_session();
        }
    }
}

#[cfg(test)]
mod arithmetic_tests {
    use super::*;
    #[test]
    fn extreme_times_compare_and_rescale_without_intermediate_overflow() {
        let a = MediaTime::new(i128::MAX, 90_000).unwrap();
        let b = MediaTime::new(i128::MAX - 1, 90_000).unwrap();
        assert_eq!(cmp(a, b).unwrap(), Ordering::Greater);
        assert_eq!(rescale(a, 90_000).unwrap(), i128::MAX);
        let negative = MediaTime::new(i128::MIN, 48_000).unwrap();
        assert_eq!(rescale(negative, 48_000).unwrap(), i128::MIN);
    }
}
