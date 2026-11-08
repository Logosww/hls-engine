//! Rebind only filesystem identities in a frozen initial checkpoint.
//! Engine configuration, input/resource evidence and execution state stay frozen.
use hls_engine::EngineCheckpoint;
use sha2::{Digest, Sha256};
pub fn at_destination(frozen: &[u8], destination: &EngineCheckpoint) -> EngineCheckpoint {
    let current = destination.to_bytes();
    assert_eq!(
        &frozen[..40],
        &current[..40],
        "schema and Strict configuration digest must be unchanged"
    );
    fn offset(b: &[u8]) -> usize {
        assert_eq!(u64::from_be_bytes(b[72..80].try_into().unwrap()), 0);
        assert_eq!(u32::from_be_bytes(b[80..84].try_into().unwrap()), 1);
        100 + u64::from_be_bytes(b[92..100].try_into().unwrap()) as usize
    }
    let old = offset(frozen);
    let new = offset(&current);
    let mut bytes = frozen.to_vec();
    bytes[old..old + 32].copy_from_slice(&current[new..new + 32]);
    // Initial checkpoints have no publication, sealed file or completed children.
    assert!(bytes[old + 32..old + 45].iter().all(|b| *b == 0));
    if &bytes[..8] == b"HLSECP03" {
        bytes[old + 46..old + 78].copy_from_slice(&current[new + 46..new + 78]);
    }
    let end = bytes.len() - 32;
    let hash = Sha256::digest(&bytes[..end]);
    bytes[end..].copy_from_slice(&hash);
    EngineCheckpoint::from_bytes(&bytes).unwrap()
}
