use sha2::{Digest, Sha256};
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

pub(crate) fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
