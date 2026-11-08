#![doc = include_str!("../README.md")]

mod cancel;
pub mod capabilities;
mod codecs;
pub mod crypto;
mod error;
mod isobmff;
mod mp4;
mod mpeg_ts;
pub mod playlist;
mod raw_sample;
mod resume;
mod source;
mod types;

pub use cancel::CancelToken;
pub(crate) use error::Result;
pub use error::{Error, Result as SourceResult};
pub use resume::CheckpointDurability;
pub use source::{
    ByteRange, MemorySource, Source, SourceLocation, SourceSessionOptions, TextResource,
};
#[cfg(feature = "default-source")]
pub use source::{HttpRequestPolicy, ReqwestSource};
pub use types::{Codec, TrackInfo, TrackType, TransmuxReport};
mod engine;
mod state_codec;
pub use engine::*;

pub use playlist::parse_playlist_snapshot;
