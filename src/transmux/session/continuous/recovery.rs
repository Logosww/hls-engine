//! Native committed-prefix checkpoints. Persistence remains caller-owned.
use super::*;
use crate::state_codec::{Reader, StateCodec};

pub const ENGINE_CHECKPOINT_SCHEMA_VERSION: u32 = 2;
const MAX_CHECKPOINT_BYTES: usize = 64 * 1024 * 1024;

/// A versioned snapshot of a durable file prefix. Contains no keys, URLs or samples.
/// Use `to_bytes`/`from_bytes` for lossless transport without the serde feature.
#[derive(Clone)]
pub struct EngineCheckpoint {
    pub(super) configuration: [u8; 32],
    pub(super) prefix: [u8; 32],
    pub(super) bytes: u64,
    pub(super) sequence: u32,
    pub(super) output: u64,
    pub(super) state: Vec<u8>,
    pub(super) destination: [u8; 32],
    pub(super) finalizing: bool,
    pub(super) completed: bool,
    pub(super) classic: bool,
    pub(super) publication: Option<(String, u64, [u8; 32])>,
    pub(super) sealed: bool,
    pub(super) outputs: Vec<(u64, [u8; 32])>,
    pub(super) synced: bool,
}
impl std::fmt::Debug for EngineCheckpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineCheckpoint")
            .field("schema", &2)
            .field("bytes", &self.bytes)
            .field("output", &self.output)
            .finish_non_exhaustive()
    }
}
impl EngineCheckpoint {
    pub fn schema_version(&self) -> u32 {
        ENGINE_CHECKPOINT_SCHEMA_VERSION
    }
    pub fn bytes_written(&self) -> u64 {
        self.bytes
    }
    pub fn output_index(&self) -> u64 {
        self.output
    }
    pub fn is_completed(&self) -> bool {
        self.completed
    }
    pub fn is_finalizing(&self) -> bool {
        self.finalizing
    }
    /// The current child output is ready for publication at a split boundary.
    pub fn is_sealed(&self) -> bool {
        self.sealed
    }
    pub fn completed_outputs(&self) -> usize {
        self.outputs.len()
    }
    pub fn completed_output(&self, index: usize) -> Option<(u64, &[u8; 32])> {
        self.outputs.get(index).map(|(bytes, hash)| (*bytes, hash))
    }
    pub fn durability(&self) -> CheckpointDurability {
        if self.synced {
            CheckpointDurability::SyncAll
        } else {
            CheckpointDurability::Flush
        }
    }
    pub fn output_format(&self) -> OutputFormat {
        if self.classic {
            OutputFormat::StreamingMp4
        } else {
            OutputFormat::FragmentedMp4
        }
    }
    pub fn next_sequence(&self) -> u32 {
        self.sequence
    }
    pub fn configuration_id(&self) -> &[u8; 32] {
        &self.configuration
    }
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = b"HLSECP02".to_vec();
        self.configuration.put(&mut bytes);
        self.prefix.put(&mut bytes);
        self.bytes.put(&mut bytes);
        self.sequence.put(&mut bytes);
        self.output.put(&mut bytes);
        self.state.put(&mut bytes);
        self.destination.put(&mut bytes);
        self.finalizing.put(&mut bytes);
        self.completed.put(&mut bytes);
        self.classic.put(&mut bytes);
        self.publication.put(&mut bytes);
        self.sealed.put(&mut bytes);
        self.outputs.put(&mut bytes);
        self.synced.put(&mut bytes);
        bytes.extend_from_slice(&crate::resume::digest(&bytes));
        bytes
    }
    pub fn from_bytes(bytes: &[u8]) -> ContinuousResult<Self> {
        let bad = || fail(ContinuousErrorKind::ResumeCorruption);
        if bytes.len() > MAX_CHECKPOINT_BYTES || bytes.len() < 132 || &bytes[..8] != b"HLSECP02" {
            return Err(bad());
        }
        let end = bytes.len() - 32;
        if crate::resume::digest(&bytes[..end]) != bytes[end..] {
            return Err(bad());
        }
        let mut r = Reader(&bytes[8..end]);
        let value = (|| -> crate::state_codec::DecodeResult<Self> {
            Ok(Self {
                configuration: StateCodec::get(&mut r)?,
                prefix: StateCodec::get(&mut r)?,
                bytes: StateCodec::get(&mut r)?,
                sequence: StateCodec::get(&mut r)?,
                output: StateCodec::get(&mut r)?,
                state: {
                    let n = usize::get(&mut r)?;
                    r.take(n)?.to_vec()
                },
                destination: StateCodec::get(&mut r)?,
                finalizing: StateCodec::get(&mut r)?,
                completed: StateCodec::get(&mut r)?,
                classic: StateCodec::get(&mut r)?,
                publication: StateCodec::get(&mut r)?,
                sealed: StateCodec::get(&mut r)?,
                outputs: StateCodec::get(&mut r)?,
                synced: StateCodec::get(&mut r)?,
            })
        })()
        .map_err(|_| bad())?;
        if !r.0.is_empty()
            || (value.bytes == 0 && (value.sealed || value.finalizing || value.sequence != 1))
            || value.sequence == 0
            || value.state.is_empty()
            || (value.completed && !value.finalizing)
            || (value.sealed && value.finalizing)
            || (value.completed && value.classic && value.publication.is_none())
            || (value.publication.is_some()
                && (!(value.finalizing || value.sealed) || !value.classic))
            || value.outputs.len() as u64 != value.output
            || value.outputs.iter().any(|(size, _)| *size == 0)
            || value
                .publication
                .as_ref()
                .is_some_and(|(name, size, _)| name.is_empty() || *size == 0)
        {
            return Err(bad());
        }
        Ok(value)
    }
}
#[cfg(feature = "serde")]
impl serde::Serialize for EngineCheckpoint {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut out = s.serialize_struct("EngineCheckpoint", 2)?;
        out.serialize_field("schema_version", &2u32)?;
        let hex: String = self.to_bytes().iter().map(|b| format!("{b:02x}")).collect();
        out.serialize_field("archive", &hex)?;
        out.end()
    }
}
#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for EngineCheckpoint {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        use serde::de::Error as _;
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            schema_version: u32,
            archive: String,
        }
        let wire = Wire::deserialize(d)?;
        if wire.schema_version != 2
            || wire.archive.len() > 2 * MAX_CHECKPOINT_BYTES
            || !wire.archive.len().is_multiple_of(2)
        {
            return Err(D::Error::custom("invalid engine checkpoint envelope"));
        }
        let hex = wire.archive.as_bytes();
        let nibble = |v: u8| match v {
            b'0'..=b'9' => Some(v - b'0'),
            b'a'..=b'f' => Some(v - b'a' + 10),
            _ => None,
        };
        let bytes = hex
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| Some(nibble(p[0])? * 16 + nibble(p[1])?))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| D::Error::custom("invalid checkpoint archive"))?;
        Self::from_bytes(&bytes).map_err(D::Error::custom)
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub type CheckpointCallback = dyn Fn(EngineCheckpoint) -> ContinuousResult<()> + Send + Sync;
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
pub struct RecoveryOptions {
    pub(super) durability: CheckpointDurability,
    pub(super) callback: Arc<CheckpointCallback>,
    pub(super) format: OutputFormat,
}
#[cfg(not(target_arch = "wasm32"))]
impl RecoveryOptions {
    pub fn output_format(&self) -> OutputFormat {
        self.format
    }
    pub fn durability(&self) -> CheckpointDurability {
        self.durability
    }
    pub fn new(callback: Arc<CheckpointCallback>) -> Self {
        Self {
            durability: CheckpointDurability::Flush,
            callback,
            format: OutputFormat::FragmentedMp4,
        }
    }
    pub fn with_output_format(mut self, format: OutputFormat) -> Self {
        self.format = format;
        self
    }
    pub fn with_durability(mut self, durability: CheckpointDurability) -> Self {
        self.durability = durability;
        self
    }
}
