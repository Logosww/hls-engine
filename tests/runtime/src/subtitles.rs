#[path = "../../support/subtitle_contract.rs"]
mod shared;
#[cfg(not(target_arch = "wasm32"))]
pub fn native_suite() -> String {
    futures::executor::block_on(shared::run()).to_string()
}
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub async fn subtitle_suite() -> String {
    shared::run().await.to_string()
}

#[cfg(target_arch = "wasm32")]
mod bridge {
    use super::shared;
    use hls_engine::{
        EngineOptions, OutputFormat, SubtitleCommit, SubtitleSink, SubtitleSinkFuture,
    };
    use std::sync::Arc;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_futures::JsFuture;
    struct Sink {
        write: js_sys::Function,
        close: js_sys::Function,
    }
    async fn wait(value: Result<JsValue, JsValue>) -> std::io::Result<()> {
        let value = value.map_err(|_| std::io::Error::other("sidecar callback"))?;
        JsFuture::from(js_sys::Promise::resolve(&value))
            .await
            .map(|_| ())
            .map_err(|_| std::io::Error::other("sidecar Promise"))
    }
    impl SubtitleSink for Sink {
        fn commit<'a>(&'a self, batch: &'a SubtitleCommit) -> SubtitleSinkFuture<'a> {
            let payload = serde_json::json!({"cues":batch.cues().iter().map(|c| serde_json::json!({
                "receipt":c.receipt().to_string(), "output":c.output_index().to_string(),
                "start":c.start().ticks().to_string(), "end":c.end().ticks().to_string(),
                "payload":c.cue().payload(), "identifier":c.cue().identifier()
            })).collect::<Vec<_>>(), "frontiers":batch.frontiers().iter().map(|f| f.end().ticks().to_string()).collect::<Vec<_>>()});
            Box::pin(wait(
                self.write
                    .call1(&JsValue::NULL, &payload.to_string().into()),
            ))
        }
        fn finish(&self) -> SubtitleSinkFuture<'_> {
            Box::pin(wait(self.close.call0(&JsValue::NULL)))
        }
    }
    #[wasm_bindgen]
    pub async fn subtitle_stream(
        write: js_sys::Function,
        close: js_sys::Function,
        cancel: js_sys::Promise,
    ) -> String {
        let session = shared::session(false, EngineOptions::default())
            .with_subtitle_sink(Arc::new(Sink { write, close }));
        shared::feed(&session);
        let handle = session.handle();
        let mut running =
            Box::pin(session.into_bytes(2 * 1024 * 1024, OutputFormat::FragmentedMp4));
        let result = tokio::select! { biased;
            _ = JsFuture::from(cancel) => { handle.cancel(); running.await },
            result = &mut running => result,
        };
        match result {
            Ok((_, report)) => {
                serde_json::json!({"bytes":report.media().bytes_written().to_string()}).to_string()
            }
            Err(error) => serde_json::json!({"error":format!("{:?}",error.kind())}).to_string(),
        }
    }
}
