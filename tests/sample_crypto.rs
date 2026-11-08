#[path = "support/sample_crypto.rs"]
mod suite;
#[tokio::test]
async fn external_packager_clear_samples_and_timelines_match() {
    assert_eq!(
        suite::run(std::sync::Arc::new(suite::Provider)).await["cases"]
            .as_array()
            .unwrap()
            .len(),
        21
    );
}

use hls_engine::{
    crypto::{key::*, resource::*, sample::*},
    *,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
struct Counting(AtomicUsize);
impl KeyProvider for Counting {
    fn resolve(&self, r: KeyRequest) -> KeyFuture<KeyResolution> {
        self.0.fetch_add(1, Ordering::SeqCst);
        suite::Provider.resolve(r)
    }
}
fn replace_box(bytes: &mut [u8], name: &[u8; 4], replacement: &[u8; 4]) {
    let moof = bytes.windows(4).position(|b| b == b"moof").unwrap();
    let size = u32::from_be_bytes(bytes[moof - 4..moof].try_into().unwrap()) as usize;
    let pos = bytes[moof..moof - 4 + size]
        .windows(4)
        .position(|b| b == name)
        .unwrap()
        + moof;
    bytes[pos..pos + 4].copy_from_slice(replacement);
}
#[tokio::test]
async fn external_auxiliary_only_and_inline_only_match() {
    let case = suite::cases()
        .into_iter()
        .find(|c| c.name == "fmp4_avc_cenc")
        .unwrap();
    let source = case.files.iter().find(|(n, _)| *n == "seg1.m4s").unwrap().1;
    let clear = suite::session(
        suite::input("fmp4_avc_clear", "primary"),
        Arc::new(suite::Provider),
        EngineOptions::default(),
    )
    .into_bytes(8 * 1024 * 1024, OutputFormat::Mp4)
    .await
    .unwrap()
    .0;
    for auxiliary in [true, false] {
        let mut bytes = source.to_vec();
        if auxiliary {
            replace_box(&mut bytes, b"senc", b"free")
        } else {
            replace_box(&mut bytes, b"saiz", b"free");
            replace_box(&mut bytes, b"saio", b"free")
        }
        let input = suite::input_modified(
            &case,
            "primary",
            case.playlist.into(),
            Some(("seg1.m4s", bytes)),
        );
        let output = suite::session(input, Arc::new(suite::Provider), EngineOptions::default())
            .into_bytes(8 * 1024 * 1024, OutputFormat::Mp4)
            .await
            .unwrap()
            .0;
        assert_eq!(suite::canonical(output), suite::canonical(clear.clone()));
    }
}
#[tokio::test]
async fn sample_budget_and_scheme_mismatch_do_not_request_keys() {
    for name in ["fmp4_avc_cenc", "ts_avc_sample"] {
        let provider = Arc::new(Counting(AtomicUsize::new(0)));
        let error = suite::session(
            suite::input(name, "primary"),
            provider.clone(),
            EngineOptions::default()
                .with_resources(ResourceOptions::default().with_sample_limit(1)),
        )
        .into_bytes(8 * 1024 * 1024, OutputFormat::Mp4)
        .await
        .err()
        .unwrap();
        assert_eq!(
            error.sample_error().unwrap().kind(),
            SampleErrorKind::BudgetExceeded
        );
        assert_eq!(provider.0.load(Ordering::SeqCst), 0);
    }
    let case = suite::cases()
        .into_iter()
        .find(|c| c.name == "fmp4_avc_cenc")
        .unwrap();
    let provider = Arc::new(Counting(AtomicUsize::new(0)));
    let input = suite::input_modified(
        &case,
        "primary",
        case.playlist.replace("SAMPLE-AES-CTR", "SAMPLE-AES"),
        None,
    );
    let error = suite::session(input, provider.clone(), EngineOptions::default())
        .into_bytes(8 * 1024 * 1024, OutputFormat::Mp4)
        .await
        .err()
        .unwrap();
    let sample = error.sample_error().unwrap();
    assert_eq!(sample.kind(), SampleErrorKind::InvalidMetadata);
    assert_eq!(sample.track_id(), Some(1));
    assert_eq!(sample.sample_index(), Some(0));
    assert_eq!(provider.0.load(Ordering::SeqCst), 0);
}

struct PendingProvider {
    calls: AtomicUsize,
    aborts: AtomicUsize,
}
impl KeyProvider for PendingProvider {
    fn resolve(&self, _: KeyRequest) -> KeyFuture<KeyResolution> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::pending())
    }
    fn abort(&self, _: &KeyRequest) {
        self.aborts.fetch_add(1, Ordering::SeqCst);
    }
}
#[tokio::test]
async fn dropping_engine_sample_key_wait_aborts_provider() {
    use futures_util::{FutureExt, task::noop_waker};
    use std::task::{Context, Poll};
    for name in ["fmp4_avc_cenc", "ts_avc_sample"] {
        let provider = Arc::new(PendingProvider {
            calls: AtomicUsize::new(0),
            aborts: AtomicUsize::new(0),
        });
        let mut future = Box::pin(
            suite::session(
                suite::input(name, "primary"),
                provider.clone(),
                EngineOptions::default(),
            )
            .into_bytes(8 * 1024 * 1024, OutputFormat::FragmentedMp4),
        );

        assert!(matches!(
            future.poll_unpin(&mut Context::from_waker(&noop_waker())),
            Poll::Pending
        ));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        drop(future);
        assert_eq!(provider.aborts.load(Ordering::SeqCst), 1);
    }
}
