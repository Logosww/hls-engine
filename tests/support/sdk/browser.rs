// Appended inside the real SDK keyed browser module; reuses its Promise/lease host.
#[wasm_bindgen]
pub async fn timeline_browser(
    request: String,
    selection: String,
    read: Function,
    write: Function,
    resolve: Function,
    abort: Function,
    cancel: js_sys::Promise,
) -> std::result::Result<JsValue, JsValue> {
    let r: wire::Request =
        serde_json::from_str(&request).map_err(|_| JsValue::from_str("invalid request"))?;
    let selection: wire::timeline::Selection =
        serde_json::from_str(&selection).map_err(|_| JsValue::from_str("invalid selection"))?;
    let read = CallbackRegistration::new(read);
    let write = CallbackRegistration::new(write);
    let resolve = CallbackRegistration::new(resolve);
    let abort = CallbackRegistration::new(abort);
    let host = Arc::new(Host {
        read: read.0,
        resolve: resolve.0,
        abort: abort.0,
        start: monotonic_ms(),
    });
    let run = async {
        let session = match wire::timeline::prepare_timeline(&r, host, selection.options()).await {
            Ok(s) => s,
            Err(e) => return e,
        };
        if r.mode == "stream" {
            return match session
                .write_to(&mut DemandWriter {
                    id: write.0,
                    pending: None,
                })
                .await
            {
                Ok(report) => serde_json::json!({"report":report}),
                Err(e) => wire::timeline::timeline_failure(e),
            };
        }
        match session.into_mp4_outputs().await {
            Ok((outputs, report)) => {
                for (index, bytes) in outputs.iter().enumerate() {
                    if !matches!(
                        invoke_local(
                            write.0,
                            vec![
                                Uint8Array::from(bytes.as_slice()).into(),
                                JsValue::from_str(&index.to_string())
                            ]
                        )
                        .await,
                        Ok(Ok(_))
                    ) {
                        return wire::error("OUTPUT_WRITE_FAILED", "write");
                    }
                }
                serde_json::json!({"report":report})
            }
            Err(e) => wire::timeline::timeline_failure(e),
        }
    };
    let result = tokio::select! {biased; _=wasm_bindgen_futures::JsFuture::from(cancel)=>wire::error("ABORTED","cancelled"),result=run=>result};
    Ok(JsValue::from_str(&result.to_string()))
}
