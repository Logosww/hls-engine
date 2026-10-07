#![doc = include_str!("../README.md")]

mod cancel;
pub mod capabilities;
mod codecs;
pub mod crypto;
mod error;
#[cfg(feature = "ffmpeg-finalize")]
mod ffmpeg_finalize;
mod hls;
mod isobmff;
mod mp4;
mod mpeg_ts;
pub mod playlist;
mod raw_sample;
mod resume;
mod source;
mod transmux;
mod types;

/// Compatibility surface for v0.x callers and schema-v1 file tasks.
/// Historical wire formats and digest domains remain unchanged.
pub mod legacy {
    pub use crate::cancel::CancelToken;
    pub use crate::error::{Error, Result};
    pub use crate::playlist::parse_playlist_snapshot;
    pub use crate::resume::{
        CHECKPOINT_SCHEMA_VERSION, CheckpointDurability, TransmuxResumeState, TransmuxStage,
    };
    pub use crate::source::{
        ByteRange, HlsInput, MemorySource, Source, SourceLocation, TextResource,
    };
    #[cfg(feature = "default-source")]
    pub use crate::source::{HttpRequestPolicy, ReqwestSource};
    pub use crate::transmux::{
        FinalizeBackend, OutputFormat, TransmuxEvent, TransmuxOptions, TransmuxPhase,
        TransmuxProgress, TransmuxRuntimeOptions, VariantSelection, transmux_hls_to_mp4_async,
        transmux_hls_to_mp4_async_with_runtime, transmux_hls_to_mp4_bytes,
        transmux_hls_to_mp4_bytes_with_runtime, transmux_hls_to_writer_async,
        transmux_hls_to_writer_async_with_runtime,
    };
    pub use crate::types::{Codec, TrackInfo, TrackType, TransmuxReport};

    #[cfg(not(target_arch = "wasm32"))]
    pub use crate::transmux::{
        finalize_partial_mp4_async, finalize_partial_mp4_async_with_runtime,
    };

    pub use crate::source::SourceSessionOptions;
    pub use crate::transmux::session::*;

    pub use crate::{capabilities, crypto, playlist};
}
pub(crate) use legacy::*;
mod engine;
mod state_codec;
pub use engine::*;
