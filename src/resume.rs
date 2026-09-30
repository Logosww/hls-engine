use crate::{Error, OutputFormat, Result};
use sha2::{Digest, Sha256};

/// Current checkpoint wire schema. v0.2 checkpoints cannot be migrated implicitly.
pub const CHECKPOINT_SCHEMA_VERSION: u32 = 1;

/// Lifecycle of a recoverable file task.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TransmuxStage {
    #[default]
    Downloading,
    Finalizing,
    Completed,
}

/// File commit guarantee before a checkpoint callback is invoked.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CheckpointDurability {
    /// Flush userspace buffers. Does not promise persistence across power loss.
    #[default]
    Flush,
    /// Flush and synchronize the output file before publishing a checkpoint.
    /// Callers must separately persist checkpoints atomically and durably.
    SyncAll,
}

/// Versioned checkpoint. Persist atomically after progress callbacks. Recovery
/// validates the committed prefix before truncating any uncommitted tail.
/// Default values are intentionally invalid, useful only for constructing tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct TransmuxResumeState {
    /// Required schema version; missing versions deserialize as legacy version 0
    /// and are explicitly rejected by recovery.
    #[cfg_attr(feature = "serde", serde(default))]
    pub schema_version: u32,
    pub stage: TransmuxStage,
    pub completed_segments: usize,
    pub total_segments: usize,
    pub bytes_written: u64,
    pub next_sequence: u32,
    pub global_base_dts_90k: u64,
    /// SHA-256 of the resolved media manifest in canonical field order.
    pub input_digest: [u8; 32],
    /// SHA-256 of the codec configuration, excluding wall-clock timestamps.
    pub init_digest: [u8; 32],
    pub output_format: OutputFormat,
    pub write_mfra: bool,
    /// Duration accumulated at the committed fragment boundary (milliseconds).
    pub duration_ms: u64,
}

impl TransmuxResumeState {
    /// Rejects unsupported schemas and inconsistent lifecycle/counter fields.
    /// File and input identity are additionally validated by recovery APIs.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != CHECKPOINT_SCHEMA_VERSION {
            return Err(Error::invalid(
                "unsupported checkpoint schema; v0.2 checkpoints require restarting with v0.3",
            ));
        }
        if self.total_segments == 0
            || self.completed_segments == 0
            || self.completed_segments > self.total_segments
            || self.next_sequence as u64 != self.completed_segments as u64 + 1
            || self.input_digest == [0; 32]
            || self.init_digest == [0; 32]
            || (self.output_format == OutputFormat::StreamingMp4
                && self.completed_segments == self.total_segments
                && self.stage == TransmuxStage::Downloading)
            || self.output_format == OutputFormat::Mp4
            || (self.stage != TransmuxStage::Downloading
                && self.completed_segments != self.total_segments)
        {
            return Err(Error::invalid("inconsistent checkpoint state"));
        }
        Ok(())
    }
}

pub(crate) fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

pub(crate) fn input_digest(
    playlist: &crate::hls::MediaPlaylist,
    location: &crate::SourceLocation,
) -> Result<[u8; 32]> {
    // Length-prefixed fields avoid ambiguous concatenation. Encoded numbers use
    // big endian; no Debug formatting or map iteration participates in identity.
    let mut hash = Sha256::new();
    fn field(hash: &mut Sha256, bytes: &[u8]) {
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
    }
    fn loc(hash: &mut Sha256, location: &crate::SourceLocation) {
        match location {
            crate::SourceLocation::Url(url) => {
                field(hash, b"url");
                field(hash, url.as_str().as_bytes());
            }
            crate::SourceLocation::File(path) => {
                field(hash, b"file");
                field(hash, path.as_os_str().as_encoded_bytes());
            }
        }
    }
    fn range(hash: &mut Sha256, value: Option<crate::ByteRange>) {
        hash.update([u8::from(value.is_some())]);
        if let Some(v) = value {
            hash.update(v.offset.to_be_bytes());
            hash.update(v.length.to_be_bytes());
        }
    }
    field(&mut hash, b"hls-transmux-manifest-v1");
    loc(&mut hash, location);
    hash.update(playlist.media_sequence.to_be_bytes());
    hash.update(
        playlist
            .target_duration
            .unwrap_or(0.0)
            .to_bits()
            .to_be_bytes(),
    );
    hash.update((playlist.segments.len() as u64).to_be_bytes());
    for s in &playlist.segments {
        loc(&mut hash, &location.resolve(&s.uri)?);
        hash.update(s.sequence_number.to_be_bytes());
        hash.update(s.duration_seconds.to_bits().to_be_bytes());
        hash.update(s.start_seconds.to_bits().to_be_bytes());
        range(&mut hash, s.byte_range);
        hash.update([u8::from(s.init_segment.is_some())]);
        if let Some(init) = &s.init_segment {
            loc(&mut hash, &location.resolve(&init.uri)?);
            range(&mut hash, init.byte_range);
        }
    }
    Ok(hash.finalize().into())
}
