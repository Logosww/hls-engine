//! Test-only binding of the production parser and validated snapshot archive.
use hls_transmux::{
    SourceLocation, TextResource, parse_playlist_snapshot,
    playlist::{InputId, PlaylistContext, PlaylistSnapshot},
};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub fn parse(content: &str, base: &str, context: &str) -> Result<String, String> {
    let context: PlaylistContext = serde_json::from_str(context).map_err(|_| "invalid context")?;
    let location = SourceLocation::Url(url::Url::parse(base).map_err(|_| "invalid base")?);
    let snapshot = parse_playlist_snapshot(
        &TextResource {
            content: content.to_owned(),
            location,
        },
        context,
    )
    .map_err(|e| e.to_string())?;
    serde_json::to_string(&snapshot).map_err(|_| "invalid snapshot".to_owned())
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub fn roundtrip(archive: &str) -> Result<String, String> {
    let snapshot: PlaylistSnapshot =
        serde_json::from_str(archive).map_err(|_| "invalid snapshot")?;
    serde_json::to_string(&snapshot).map_err(|_| "invalid snapshot".to_owned())
}

pub fn fixture() -> String {
    let context = PlaylistContext::new(InputId::new("primary").unwrap(), u64::MAX)
        .with_revision(9_007_199_254_740_995);
    parse(
        include_str!("../playlist.m3u8"),
        "https://final.test/redirect/list.m3u8",
        &serde_json::to_string(&context).unwrap(),
    )
    .unwrap()
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub fn playlist_fixture_text() -> String {
    include_str!("../playlist.m3u8").to_owned()
}
