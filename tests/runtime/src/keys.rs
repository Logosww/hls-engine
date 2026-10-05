//! Actual production key-session contracts shared by native, Node and Chrome.
use futures::{FutureExt, task::noop_waker};
use hls_transmux::{
    SourceLocation, TextResource, crypto::key::*, parse_playlist_snapshot, playlist::*,
};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

#[derive(Default)]
struct Clock(AtomicU64);
impl KeyClock for Clock {
    fn now(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}
fn resource(uri: &str) -> KeyResource {
    let text = TextResource {
        location: SourceLocation::Url(url::Url::parse("https://example.test/list").unwrap()),
        content: format!(
            "#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:9007199254740993\n#EXT-X-KEY:METHOD=AES-128,URI=\"{uri}\"\n#EXTINF:4,\na.ts\n#EXT-X-ENDLIST\n"
        ),
    };
    KeyResource::media(
        &parse_playlist_snapshot(
            &text,
            PlaylistContext::new(InputId::new("primary").unwrap(), u64::MAX)
                .with_revision(u64::MAX),
        )
        .unwrap()
        .segments()[0],
    )
}
fn pending(waiter: &mut KeyWaiter) {
    assert!(
        waiter
            .poll_unpin(&mut std::task::Context::from_waker(&noop_waker()))
            .is_pending()
    );
}
fn session(op: &str, p: Arc<dyn KeyProvider>, clock: Arc<Clock>) -> Arc<KeySession> {
    let s = Arc::new(
        KeySession::new(op, "opaque-auth", p, clock, KeySessionOptions::default()).unwrap(),
    );
    #[cfg(target_arch = "wasm32")]
    bridge::CURRENT.with(|v| *v.borrow_mut() = Arc::downgrade(&s));
    s
}
fn response(request: &KeyRequest, bytes: Vec<u8>) -> KeyResolution {
    match SecretKey::new(bytes) {
        Err(_) => KeyResolution::Failure(ProviderFailure::new(
            ProviderFailureKind::InvalidResponse,
            Arc::new(std::io::Error::other("invalid key")),
        )),
        Ok(key) => {
            let mut result =
                AvailableKey::aes128(key).with_version(format!("v{}", request.resolve_revision()));
            if request.operation() == "ttl" {
                result = result.with_valid_until(request.resolve_revision() * 10);
            }
            KeyResolution::Available(result)
        }
    }
}
pub async fn suite(provider: Arc<dyn KeyProvider>) -> String {
    let clock = Arc::new(Clock::default());
    let s = session("shared", provider.clone(), clock.clone());
    let mut a = s.try_resolve(resource("key")).unwrap();
    let b = s.try_resolve(resource("key")).unwrap();
    pending(&mut a);
    drop(a);
    let key = b.await.unwrap();
    assert_eq!(key.secret().expose(), &[0xab; 16]);
    assert!(Arc::ptr_eq(
        &key,
        &s.try_resolve(resource("key")).unwrap().await.unwrap()
    ));
    assert_eq!(s.stats().cached(), 1);
    for uri in ["reject", "throw", "invalid", "wrong-type"] {
        let e = s.try_resolve(resource(uri)).unwrap().await.unwrap_err();
        assert_eq!(e.kind(), KeyErrorKind::Provider);
        assert!(!format!("{e:?}").contains("RAW-CREDENTIALS"));
    }
    let e = s
        .try_resolve(resource("unavailable"))
        .unwrap()
        .await
        .unwrap_err();
    assert_eq!(e.kind(), KeyErrorKind::Unavailable);
    // Real expiry + refresh: provider and caller share the injected clock domain.
    let ttl = session("ttl", provider.clone(), clock.clone());
    let first = ttl.try_resolve(resource("key")).unwrap().await.unwrap();
    clock.0.store(10, Ordering::SeqCst);
    let second = ttl.try_resolve(resource("key")).unwrap().await.unwrap();
    assert_ne!(first.resolve_revision(), second.resolve_revision());
    clock.0.store(0, Ordering::SeqCst);
    let refresh = session("refresh", provider.clone(), clock.clone());
    let mut old = refresh.try_resolve(resource("key")).unwrap();
    pending(&mut old);
    refresh.invalidate().unwrap();
    let fresh = refresh.try_resolve(resource("key")).unwrap().await.unwrap();
    old.await.unwrap();
    assert!(Arc::ptr_eq(
        &fresh,
        &refresh.try_resolve(resource("key")).unwrap().await.unwrap()
    ));
    for op in ["cancel", "drop", "last", "reject-late"] {
        let s = session(op, provider.clone(), clock.clone());
        let mut a = s.try_resolve(resource("key")).unwrap();
        pending(&mut a);
        if op == "last" {
            a.cancel();
            assert_eq!(s.stats().in_flight(), 0);
        } else {
            if op != "drop" {
                s.cancel();
                s.cancel();
                assert_eq!(s.stats().cached(), 0);
            }
            drop(s);
            assert_eq!(a.await.unwrap_err().kind(), KeyErrorKind::Cancelled);
        }
    }
    // Exercise budget without even invoking a third provider and keep operations isolated.
    let budget = session("budget", provider.clone(), clock.clone());
    let a = budget.try_resolve(resource("a")).unwrap();
    let b = budget.try_resolve(resource("b")).unwrap();
    assert_eq!(
        budget.try_resolve(resource("c")).err().unwrap().kind(),
        KeyErrorKind::BudgetExceeded
    );
    let (a, b) = futures::join!(a, b);
    a.unwrap();
    b.unwrap();
    let isolated = session("isolated", provider, clock);
    s.cancel();
    isolated
        .try_resolve(resource("key"))
        .unwrap()
        .await
        .unwrap();
    serde_json::json!({"productionKeySession":true,"coalesced":true,"singleWaiterDrop":true,"expiry":true,"invalidation":true,"lateGeneration":true,"cancelDrop":true,"lastWaiterAbort":true,"typedRejection":true,"strictKeyBytes":true,"bounded":true,"operationIsolation":true}).to_string()
}

#[cfg(not(target_arch = "wasm32"))]
pub fn native_suite() -> String {
    struct Native;
    impl KeyProvider for Native {
        fn resolve(&self, request: KeyRequest) -> KeyFuture<KeyResolution> {
            Box::pin(async move {
                let mut yielded = false;
                futures::future::poll_fn(move |cx| {
                    if yielded {
                        std::task::Poll::Ready(())
                    } else {
                        yielded = true;
                        cx.waker().wake_by_ref();
                        std::task::Poll::Pending
                    }
                })
                .await;
                let uri = match request.reference().location().location() {
                    SourceLocation::Url(u) => u.path(),
                    _ => unreachable!(),
                };
                match uri {
                    "/reject" | "/throw" => KeyResolution::Failure(ProviderFailure::new(
                        ProviderFailureKind::Transport,
                        Arc::new(std::io::Error::other("RAW-CREDENTIALS")),
                    )),
                    "/invalid" | "/wrong-type" => response(&request, vec![]),
                    "/unavailable" => KeyResolution::Unavailable,
                    _ => response(&request, vec![0xab; 16]),
                }
            })
        }
    }
    futures::executor::block_on(suite(Arc::new(Native)))
}
#[cfg(test)]
mod tests {
    #[test]
    fn native_contracts() {
        println!("{}", super::native_suite());
    }
}

#[cfg(target_arch = "wasm32")]
mod bridge {
    use super::*;
    use std::{cell::RefCell, sync::Weak};
    use wasm_bindgen::prelude::*;
    thread_local! { pub(super) static CURRENT:RefCell<Weak<KeySession>>=const { RefCell::new(Weak::new()) }; }
    struct JsProvider {
        resolve: js_sys::Function,
        abort: js_sys::Function,
    }
    fn token(r: &KeyRequest) -> String {
        format!("{}:{}", r.operation(), r.resolve_revision())
    }
    impl KeyProvider for JsProvider {
        fn resolve(&self, r: KeyRequest) -> KeyFuture<KeyResolution> {
            let wire=serde_json::json!({"operation":r.operation(),"authScope":r.auth_scope(),"token":token(&r),"revision":r.resolve_revision().to_string(),"refreshGeneration":r.refresh_generation().to_string(),"sequence":r.resource().slot().sequence().to_string(),"generation":r.resource().slot().generation().to_string(),"declaration":r.reference().declaration().ordinal().to_string(),"snapshotRevision":r.reference().declaration().revision().to_string(),"uri":r.reference().location().diagnostic()}).to_string();
            let promise = self.resolve.call1(&JsValue::NULL, &wire.into());
            Box::pin(async move {
                let data = match promise {
                    Ok(v) => {
                        wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(&v)).await
                    }
                    Err(e) => Err(e),
                };
                match data {
                    Err(_) => KeyResolution::Failure(ProviderFailure::new(
                        ProviderFailureKind::Transport,
                        Arc::new(std::io::Error::other(
                            "provider rejected; raw JS value intentionally omitted by test adapter",
                        )),
                    )),
                    Ok(data) if data.is_null() => KeyResolution::Unavailable,
                    Ok(data) => {
                        // No Uint8Array constructor coercion; provider response must be exactly byte-shaped.
                        if !data.is_instance_of::<js_sys::Uint8Array>() {
                            return response(&r, vec![]);
                        }
                        response(&r, data.unchecked_into::<js_sys::Uint8Array>().to_vec())
                    }
                }
            })
        }
        fn abort(&self, r: &KeyRequest) {
            let _ = self.abort.call1(&JsValue::NULL, &token(r).into());
        }
    }
    #[wasm_bindgen]
    pub fn inspect() -> String {
        CURRENT.with(|v| {
            let Some(s) = v.borrow().upgrade() else {
                return "dropped".to_owned();
            };
            let stats = s.stats();
            format!("{}:{}", stats.in_flight(), stats.cached())
        })
    }
    #[wasm_bindgen(js_name = run_keys)]
    pub async fn run_suite(resolve: js_sys::Function, abort: js_sys::Function) -> String {
        suite(Arc::new(JsProvider { resolve, abort })).await
    }
}
