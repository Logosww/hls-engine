use futures_util::{FutureExt, task::noop_waker};
use hls_transmux::{
    SourceLocation, TextResource, crypto::key::*, parse_playlist_snapshot, playlist::*,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

#[derive(Default)]
struct Clock(AtomicU64);
impl KeyClock for Clock {
    fn now(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}
struct Provider {
    calls: Mutex<Vec<KeyRequest>>,
    aborts: Mutex<Vec<u64>>,
    replies: Mutex<Vec<tokio::sync::oneshot::Sender<KeyResolution>>>,
}
impl Provider {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(vec![]),
            aborts: Mutex::new(vec![]),
            replies: Mutex::new(vec![]),
        })
    }
    fn reply(&self, value: KeyResolution) {
        self.replies.lock().unwrap().remove(0).send(value).unwrap();
    }
    fn calls(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}
impl KeyProvider for Provider {
    fn resolve(&self, request: KeyRequest) -> KeyFuture<KeyResolution> {
        self.calls.lock().unwrap().push(request);
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.replies.lock().unwrap().push(tx);
        Box::pin(async move { rx.await.unwrap() })
    }
    fn abort(&self, request: &KeyRequest) {
        assert!(request.cancellation().is_cancelled());
        self.aborts.lock().unwrap().push(request.resolve_revision());
    }
}
fn snapshot(body: &str, input: &str, generation: u64, revision: u64) -> PlaylistSnapshot {
    parse_playlist_snapshot(&TextResource { location: SourceLocation::Url(url::Url::parse("https://user:password@host.test/list?token=SECRET").unwrap()), content: format!("#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:9007199254740993\n{body}\n#EXT-X-ENDLIST\n") }, PlaylistContext::new(InputId::new(input).unwrap(), generation).with_revision(revision)).unwrap()
}
fn playlist() -> PlaylistSnapshot {
    snapshot(
        "#EXT-X-KEY:METHOD=AES-128,URI=\"key?token=SECRET\"\n#EXTINF:4,\na.ts\n#EXTINF:4,\nb.ts\n#EXT-X-KEY:METHOD=AES-128,URI=\"key?token=SECRET\"\n#EXTINF:4,\nc.ts",
        "primary",
        0,
        0,
    )
}
fn resource() -> KeyResource {
    KeyResource::media(&playlist().segments()[0])
}
fn session(p: Arc<dyn KeyProvider>, clock: Arc<Clock>, options: KeySessionOptions) -> KeySession {
    KeySession::new("operation-secret", "auth-secret", p, clock, options).unwrap()
}
fn pending(waiter: &mut KeyWaiter) {
    assert!(
        waiter
            .poll_unpin(&mut std::task::Context::from_waker(&noop_waker()))
            .is_pending()
    );
}
fn available(version: Option<&str>, until: Option<u64>) -> KeyResolution {
    let mut value = AvailableKey::aes128(SecretKey::new(vec![0xab; 16]).unwrap());
    if let Some(v) = version {
        value = value.with_version(v);
    }
    if let Some(t) = until {
        value = value.with_valid_until(t);
    }
    KeyResolution::Available(value)
}
fn error(result: Result<KeyWaiter, KeyError>) -> KeyError {
    result.err().unwrap()
}

#[tokio::test]
async fn coalescing_waiter_drop_and_operation_lifecycle() {
    fn send_sync<T: Send + Sync>() {}
    fn send<T: Send>() {}
    send_sync::<KeySession>();
    send::<KeyWaiter>();
    send::<KeyRequest>();
    let p = Provider::new();
    let s = session(
        p.clone(),
        Arc::new(Clock::default()),
        KeySessionOptions::default(),
    );
    let mut a = s.try_resolve(resource()).unwrap();
    let mut b = s
        .try_resolve(KeyResource::media(&playlist().segments()[1]))
        .unwrap();
    pending(&mut a);
    pending(&mut b);
    assert_eq!(p.calls(), 1);
    assert_eq!(
        error(s.try_resolve(resource())).kind(),
        KeyErrorKind::BudgetExceeded
    );
    drop(a);
    assert!(p.aborts.lock().unwrap().is_empty());
    p.reply(available(None, None));
    let key = b.await.unwrap();
    assert_eq!(key.secret().expose(), &[0xab; 16]);
    assert!(matches!(key.version(), KeyVersion::Revision(1)));
    assert!(Arc::ptr_eq(
        &key,
        &s.try_resolve(resource()).unwrap().await.unwrap()
    ));
    assert_eq!(
        (
            s.stats().in_flight(),
            s.stats().cached(),
            s.stats().waiting_resources()
        ),
        (0, 1, 0)
    );
    s.cancel();
    s.cancel();
    assert_eq!(s.stats().cached(), 0);
    assert_eq!(
        error(s.try_resolve(resource())).kind(),
        KeyErrorKind::Cancelled
    );
}

#[tokio::test]
async fn cancellation_aborts_last_waiter_and_drop_wakes_pending() {
    let p = Provider::new();
    let s = session(
        p.clone(),
        Arc::new(Clock::default()),
        KeySessionOptions::default(),
    );
    let mut a = s.try_resolve(resource()).unwrap();
    pending(&mut a);
    let request = p.calls.lock().unwrap()[0].clone();
    a.cancel();
    request.cancellation().cancelled().await;
    assert_eq!(*p.aborts.lock().unwrap(), [1]);
    assert_eq!(s.stats().in_flight(), 0);
    let mut a = s.try_resolve(resource()).unwrap();
    pending(&mut a);
    drop(s);
    assert_eq!(a.await.unwrap_err().kind(), KeyErrorKind::Cancelled);
    assert_eq!(*p.aborts.lock().unwrap(), [1, 2]);
    for tx in p.replies.lock().unwrap().drain(..) {
        assert!(tx.send(available(None, None)).is_err());
    }
}

#[tokio::test]
async fn ttl_invalidation_late_success_and_lru_are_version_aware() {
    let p = Provider::new();
    let clock = Arc::new(Clock::default());
    let s = session(
        p.clone(),
        clock.clone(),
        KeySessionOptions::default().with_limits(2, 2, 3),
    );
    let mut old = s.try_resolve(resource()).unwrap();
    pending(&mut old);
    s.invalidate().unwrap();
    let mut fresh = s.try_resolve(resource()).unwrap();
    pending(&mut fresh);
    // Complete NEW request first. Old result must not overwrite it.
    p.replies
        .lock()
        .unwrap()
        .remove(1)
        .send(available(Some("new"), Some(10)))
        .unwrap();
    let new = fresh.await.unwrap();
    p.reply(available(Some("old"), None));
    assert!(matches!(old.await.unwrap().version(), KeyVersion::Provider(v) if v == "old"));
    assert!(Arc::ptr_eq(
        &new,
        &s.try_resolve(resource()).unwrap().await.unwrap()
    ));
    clock.0.store(10, Ordering::SeqCst);
    let mut expired = s.try_resolve(resource()).unwrap();
    pending(&mut expired);
    assert_eq!(
        p.calls.lock().unwrap().last().unwrap().refresh_reason(),
        RefreshReason::Expired
    );
    p.reply(available(Some("refreshed"), Some(20)));
    expired.await.unwrap();
    let r2 = KeyResource::media(&playlist().segments()[2]);
    let mut second = s.try_resolve(r2.clone()).unwrap();
    pending(&mut second);
    p.reply(available(None, None));
    second.await.unwrap();
    // Touch first so second is evicted by third.
    s.try_resolve(resource()).unwrap().await.unwrap();
    let r3 = KeyResource::media(
        &snapshot(
            "#EXT-X-KEY:METHOD=AES-128,URI=\"third\"\n#EXTINF:4,\na.ts",
            "primary",
            0,
            0,
        )
        .segments()[0],
    );
    let mut third = s.try_resolve(r3).unwrap();
    pending(&mut third);
    p.reply(available(None, None));
    third.await.unwrap();
    assert_eq!(s.stats().cached(), 2);
    let mut evicted = s.try_resolve(r2).unwrap();
    pending(&mut evicted);
    assert_eq!(p.calls(), 6);
    p.reply(available(None, None));
    evicted.await.unwrap();
}

#[tokio::test]
async fn expires_during_provider_wait_and_between_cache_admission_and_poll() {
    let p = Provider::new();
    let clock = Arc::new(Clock::default());
    let s = session(p.clone(), clock.clone(), KeySessionOptions::default());
    let mut a = s.try_resolve(resource()).unwrap();
    pending(&mut a);
    clock.0.store(10, Ordering::SeqCst);
    p.reply(available(None, Some(10)));
    assert_eq!(a.await.unwrap_err().kind(), KeyErrorKind::Expired);
    assert_eq!(s.stats().cached(), 0);
    let mut a = s.try_resolve(resource()).unwrap();
    pending(&mut a);
    p.reply(available(None, Some(20)));
    a.await.unwrap();
    let cached = s.try_resolve(resource()).unwrap();
    clock.0.store(20, Ordering::SeqCst);
    assert_eq!(cached.await.unwrap_err().kind(), KeyErrorKind::Expired);
}

#[tokio::test]
async fn candidate_preference_unavailable_and_terminal_failure() {
    let body = "#EXT-X-KEY:METHOD=AES-128,URI=\"unknown\",KEYFORMAT=\"unknown\"\n#EXT-X-KEY:METHOD=AES-128,URI=\"identity\"\n#EXT-X-KEY:METHOD=AES-128,URI=\"custom\",KEYFORMAT=\"custom\",KEYFORMATVERSIONS=\"1/2\"\n#EXTINF:4,\na.ts";
    let r = KeyResource::media(&snapshot(body, "input", 0, 0).segments()[0]);
    let p = Provider::new();
    let s = session(
        p.clone(),
        Arc::new(Clock::default()),
        KeySessionOptions::default().with_formats(vec![
            KeyFormatSupport::new("custom", vec![2]),
            KeyFormatSupport::new("identity", vec![1]),
        ]),
    );
    let mut a = s.try_resolve(r.clone()).unwrap();
    pending(&mut a);
    assert_eq!(p.calls.lock().unwrap()[0].reference().format(), "custom");
    p.reply(KeyResolution::Unavailable);
    pending(&mut a);
    assert_eq!(p.calls.lock().unwrap()[1].reference().format(), "identity");
    p.reply(available(None, None));
    assert_eq!(a.await.unwrap().reference().format(), "identity");
    s.invalidate().unwrap();
    let mut a = s.try_resolve(r.clone()).unwrap();
    pending(&mut a);
    p.reply(KeyResolution::Failure(ProviderFailure::new(
        ProviderFailureKind::Authorization,
        Arc::new(std::io::Error::other("RAW-CREDENTIALS")),
    )));
    let err = a.await.unwrap_err();
    assert_eq!(p.calls(), 3);
    assert_eq!(
        err.provider_failure().unwrap().kind(),
        ProviderFailureKind::Authorization
    );
    assert_eq!(
        err.provider_failure().unwrap().raw_cause().to_string(),
        "RAW-CREDENTIALS"
    );
    for text in [
        format!("{err:?}"),
        err.to_string(),
        format!("{:?}", p.calls.lock().unwrap()[0]),
    ] {
        for forbidden in [
            "RAW-CREDENTIALS",
            "SECRET",
            "auth-secret",
            "operation-secret",
            "password",
        ] {
            assert!(!text.contains(forbidden), "{text}");
        }
    }
    let mut a = s.try_resolve(r).unwrap();
    pending(&mut a);
    p.reply(KeyResolution::Unavailable);
    pending(&mut a);
    p.reply(KeyResolution::Unavailable);
    assert_eq!(a.await.unwrap_err().kind(), KeyErrorKind::Unavailable);
}

#[tokio::test]
async fn declarations_generations_epochs_inputs_and_auth_do_not_alias() {
    let p = Provider::new();
    let clock = Arc::new(Clock::default());
    let s = session(p.clone(), clock.clone(), KeySessionOptions::default());
    let body = "#EXT-X-KEY:METHOD=AES-128,URI=\"same\"\n#EXTINF:4,\na.ts";
    let mut resources = Vec::new();
    for (input, generation, revision) in [("a", 0, 0), ("b", 0, 0), ("a", 1, 0), ("a", 0, 1)] {
        resources.push(KeyResource::media(
            &snapshot(body, input, generation, revision).segments()[0],
        ));
    }
    resources.push(KeyResource::media(
        &snapshot(
            &format!("#EXT-X-DISCONTINUITY-SEQUENCE:1\n{body}"),
            "a",
            0,
            0,
        )
        .segments()[0],
    ));
    resources.push(KeyResource::media(
        &snapshot(&body.replace("same", "same?auth=other"), "a", 0, 0).segments()[0],
    ));
    for r in resources {
        let mut a = s.try_resolve(r).unwrap();
        pending(&mut a);
        p.reply(available(None, None));
        a.await.unwrap();
    }
    assert_eq!(p.calls(), 6);
    let other = KeySession::new(
        "other-op",
        "other-auth",
        p.clone(),
        clock,
        KeySessionOptions::default(),
    )
    .unwrap();
    let r = KeyResource::media(&snapshot(body, "a", 0, 0).segments()[0]);
    let mut a = other.try_resolve(r).unwrap();
    pending(&mut a);
    s.cancel();
    p.reply(available(None, None));
    a.await.unwrap();
    assert_eq!(p.calls(), 7);
}

#[tokio::test]
async fn bounded_pending_reclaims_unpolled_work_and_preserves_resource_errors() {
    let p = Provider::new();
    let s = session(
        p.clone(),
        Arc::new(Clock::default()),
        KeySessionOptions::default().with_limits(1, 8, 3),
    );
    let a = s.try_resolve(resource()).unwrap();
    let r = KeyResource::media(&playlist().segments()[2]);
    assert_eq!(
        error(s.try_resolve(r.clone())).kind(),
        KeyErrorKind::BudgetExceeded
    );
    drop(a);
    assert_eq!(p.calls(), 0);
    assert_eq!(s.stats().waiting_resources(), 0);
    let mut a = s.try_resolve(r).unwrap();
    pending(&mut a);
    let b = s
        .try_resolve(KeyResource::media(&playlist().segments()[2]))
        .unwrap();
    p.reply(KeyResolution::Unavailable);
    for e in [a.await.unwrap_err(), b.await.unwrap_err()] {
        assert_eq!(
            e.resource().unwrap().slot().sequence(),
            9_007_199_254_740_995
        );
    }
}

#[tokio::test]
async fn metadata_key_kid_and_options_validation() {
    assert_eq!(
        SecretKey::new(vec![1; 15]).unwrap_err().kind(),
        KeyErrorKind::InvalidKey
    );
    let p = Provider::new();
    let clock = Arc::new(Clock::default());
    assert_eq!(
        KeySession::new(
            "op",
            "scope",
            p.clone(),
            clock.clone(),
            KeySessionOptions::default().with_limits(0, 1, 1)
        )
        .err()
        .unwrap()
        .kind(),
        KeyErrorKind::InvalidOptions
    );
    let s = session(p.clone(), clock, KeySessionOptions::default());
    for (body, kind) in [
        (
            "#EXT-X-KEY:METHOD=AES-256-GCM,URI=\"k\"",
            KeyErrorKind::Unsupported,
        ),
        (
            "#EXT-X-KEY:METHOD=AES-128,URI=\"k\",KEYFORMAT=\"unknown\"",
            KeyErrorKind::Unsupported,
        ),
        (
            "#EXT-X-KEY:METHOD=AES-128,URI=\"k\"\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"k\",KEYFORMAT=\"custom\"",
            KeyErrorKind::ConflictingMetadata,
        ),
    ] {
        let r = KeyResource::media(
            &snapshot(&format!("{body}\n#EXTINF:4,\na.ts"), "a", 0, 0).segments()[0],
        );
        assert_eq!(error(s.try_resolve(r)).kind(), kind);
    }
    assert_eq!(p.calls(), 0);
    let mut a = s.try_resolve(resource().with_kid([1; 16])).unwrap();
    pending(&mut a);
    p.reply(available(None, None));
    assert_eq!(
        a.await.unwrap_err().kind(),
        KeyErrorKind::ConflictingMetadata
    );
    let mut a = s.try_resolve(resource().with_kid([1; 16])).unwrap();
    pending(&mut a);
    p.reply(KeyResolution::Available(
        AvailableKey::aes128(SecretKey::new(vec![1; 16]).unwrap()).with_kid([1; 16]),
    ));
    a.await.unwrap();
}

#[tokio::test]
async fn provider_can_reenter_session_synchronously_and_on_abort() {
    struct Reentrant(Mutex<std::sync::Weak<KeySession>>);
    impl KeyProvider for Reentrant {
        fn resolve(&self, request: KeyRequest) -> KeyFuture<KeyResolution> {
            let s = self.0.lock().unwrap().upgrade().unwrap();
            assert_eq!(s.stats().in_flight(), 1);
            s.cancel();
            assert!(request.cancellation().is_cancelled());
            Box::pin(async { available(None, None) })
        }
        fn abort(&self, _: &KeyRequest) {
            let s = self.0.lock().unwrap().upgrade().unwrap();
            assert_eq!(s.stats().in_flight(), 0);
            s.cancel();
        }
    }
    let p = Arc::new(Reentrant(Mutex::new(std::sync::Weak::new())));
    let s = Arc::new(session(
        p.clone(),
        Arc::new(Clock::default()),
        KeySessionOptions::default(),
    ));
    *p.0.lock().unwrap() = Arc::downgrade(&s);
    assert_eq!(
        s.try_resolve(resource()).unwrap().await.unwrap_err().kind(),
        KeyErrorKind::Cancelled
    );
    assert_eq!(s.stats().cached(), 0);
}

#[tokio::test]
async fn operation_cancel_wakes_an_already_registered_executor() {
    let p = Provider::new();
    let s = session(
        p.clone(),
        Arc::new(Clock::default()),
        KeySessionOptions::default(),
    );
    let mut waiter = s.try_resolve(resource()).unwrap();
    let mut polls = 0;
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        futures_util::future::poll_fn(|cx| {
            polls += 1;
            let result = waiter.poll_unpin(cx);
            if result.is_pending() {
                s.cancel();
            }
            result
        }),
    )
    .await
    .unwrap();
    assert_eq!(result.unwrap_err().kind(), KeyErrorKind::Cancelled);
    assert_eq!(polls, 2);
    assert_eq!(p.aborts.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn shared_errors_keep_each_waiters_resource_and_maps_keep_frozen_keys() {
    let p = Provider::new();
    let s = session(
        p.clone(),
        Arc::new(Clock::default()),
        KeySessionOptions::default(),
    );
    let playlist = playlist();
    let mut a = s
        .try_resolve(KeyResource::media(&playlist.segments()[0]))
        .unwrap();
    let b = s
        .try_resolve(KeyResource::media(&playlist.segments()[1]))
        .unwrap();
    pending(&mut a);
    p.reply(KeyResolution::Failure(ProviderFailure::new(
        ProviderFailureKind::Transport,
        Arc::new(std::io::Error::other("transport")),
    )));
    let a = a.await.unwrap_err();
    let b = b.await.unwrap_err();
    assert_eq!(
        a.resource().unwrap().slot().sequence() + 1,
        b.resource().unwrap().slot().sequence()
    );
    assert_eq!(a.reference(), b.reference());
    assert!(a.reference().is_some());
    let playlist = snapshot(
        "#EXT-X-KEY:METHOD=AES-128,URI=\"old\",IV=0x01\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXT-X-KEY:METHOD=AES-128,URI=\"new\"\n#EXTINF:4,\na.m4s",
        "input",
        0,
        0,
    );
    let mut map = s
        .try_resolve(KeyResource::map(&playlist.segments()[0]).unwrap())
        .unwrap();
    pending(&mut map);
    {
        let calls = p.calls.lock().unwrap();
        let request = calls.last().unwrap();
        assert_eq!(request.resource().kind(), KeyResourceKind::Map);
        assert!(
            request
                .reference()
                .location()
                .diagnostic()
                .ends_with("/old")
        );
    }
    p.reply(available(None, None));
    map.await.unwrap();
}

#[test]
fn secret_and_provider_version_debug_are_redacted() {
    let value = AvailableKey::aes128(SecretKey::new(vec![171; 16]).unwrap())
        .with_version("SENSITIVE_VERSION");
    let debug = format!("{value:?}");
    assert!(!debug.contains("171"));
    assert!(!debug.contains("SENSITIVE_VERSION"));
    assert_eq!(
        format!("{:?}", KeyVersion::Provider("SENSITIVE_VERSION".into())),
        "KeyVersion(..)"
    );
}
