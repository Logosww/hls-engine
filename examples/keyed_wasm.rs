#![cfg(target_arch = "wasm32")]
//! Host supplies bounded resource buffers and asynchronous key resolution; no HTTP credentials enter reports.
use hls_transmux::{crypto::key::*, playlist::*, *};
use std::sync::Arc;
use wasm_bindgen::prelude::*;
struct Clock(std::sync::atomic::AtomicU64);
impl KeyClock for Clock {
    fn now(&self) -> u64 {
        {
            let now = js_sys::Date::now().max(0.0) as u64;
            self.0
                .fetch_max(now, std::sync::atomic::Ordering::SeqCst)
                .max(now)
        }
    }
}
struct Provider(js_sys::Function);
impl KeyProvider for Provider {
    fn resolve(&self, request: KeyRequest) -> KeyFuture<KeyResolution> {
        // URI is explicitly passed to the trusted provider, never to progress/error output.
        let location = match request.reference().location().location() {
            SourceLocation::Url(url) => url.as_str().to_owned(),
            _ => return Box::pin(async { KeyResolution::Unavailable }),
        };
        let wire=serde_json::json!({"uri":location,"sequence":request.resource().slot().sequence().to_string(),"revision":request.resolve_revision().to_string(),"kind":format!("{:?}",request.resource().kind())}).to_string();
        let promise = self.0.call1(&JsValue::NULL, &wire.into());
        Box::pin(async move {
            match promise {
                Ok(value) => {
                    match wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(&value))
                        .await
                    {
                        Ok(value) if value.is_null() => KeyResolution::Unavailable,
                        Ok(value) if value.is_instance_of::<js_sys::Uint8Array>() => {
                            match SecretKey::new(js_sys::Uint8Array::new(&value).to_vec()) {
                                Ok(secret) => {
                                    KeyResolution::Available(AvailableKey::aes128(secret))
                                }
                                Err(_) => failure(),
                            }
                        }
                        _ => failure(),
                    }
                }
                Err(_) => failure(),
            }
        })
    }
}
fn failure() -> KeyResolution {
    KeyResolution::Failure(ProviderFailure::new(
        ProviderFailureKind::InvalidResponse,
        Arc::new(std::io::Error::other("host key response rejected")),
    ))
}
/// resources maps absolute URL strings to Uint8Array. Provider returns Promise<Uint8Array|null>.
/// The caller owns fetching/buffering, authorization, retry, sink close, and JS buffer cleanup.
#[wasm_bindgen]
// Host Promise functions are intentionally local; the library API uses Arc on both targets.
#[allow(clippy::arc_with_non_send_sync)]
pub async fn transmux(
    playlist: String,
    playlist_url: String,
    resources: js_sys::Object,
    resolve: js_sys::Function,
    on_event: js_sys::Function,
) -> std::result::Result<js_sys::Uint8Array, JsValue> {
    let mut source = MemorySource::new();
    for entry in js_sys::Object::entries(&resources).iter() {
        let entry = js_sys::Array::from(&entry);
        let url = entry
            .get(0)
            .as_string()
            .ok_or_else(|| JsValue::from_str("invalid resource URL"))?;
        let value = entry.get(1);
        if !value.is_instance_of::<js_sys::Uint8Array>() {
            return Err(JsValue::from_str("resource must be Uint8Array"));
        }
        source = source.segment(url, js_sys::Uint8Array::new(&value).to_vec());
    }
    let location = SourceLocation::Url(
        url::Url::parse(&playlist_url).map_err(|_| JsValue::from_str("invalid playlist URL"))?,
    );
    let snapshot = parse_playlist_snapshot(
        &TextResource {
            content: playlist,
            location,
        },
        PlaylistContext::new(InputId::new("primary").unwrap(), 0),
    )
    .map_err(|e| JsValue::from_str(&e.to_string()))?;
    let keys = KeySession::new(
        "example",
        "host-provider",
        Arc::new(Provider(resolve)),
        Arc::new(Clock(std::sync::atomic::AtomicU64::new(0))),
        KeySessionOptions::default(),
    )
    .map_err(|e| JsValue::from_str(&e.to_string()))?;
    let options=KeyedPrepareOptions::default().with_on_event(Arc::new(move |event| {
        let p=&event.inputs()[0];let wire=serde_json::json!({"phase":format!("{:?}",event.phase()),"downloadedBytes":p.downloaded_bytes().to_string(),"decryptedBytes":p.decrypted_bytes().to_string(),"committed":p.committed_segments().to_string()}).to_string();
        // Observation failures do not own/cancel the media operation.
        let _=on_event.call1(&JsValue::NULL,&wire.into());
    }));
    let prepared = prepare_hls_with_keys(
        KeyedInputs::new(KeyedInput::new(snapshot, Arc::new(source))),
        keys,
        options,
    )
    .await
    .map_err(|e| JsValue::from_str(&e.to_string()))?;
    let (bytes, _) = prepared
        .into_mp4_bytes()
        .await
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    Ok(js_sys::Uint8Array::from(bytes.as_slice()))
}
