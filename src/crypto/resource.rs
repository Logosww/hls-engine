//! Bounded clear/AES-128 resource preparation. No media output API is enabled here.
#![doc = include_str!("../../docs/aes-resources.md")]
use super::key::{
    KeyError, KeyErrorKind, KeyResource, KeyResourceKind, KeySession, KeyVersion,
    RequestCancellation, ResolvedKey,
};
use crate::{
    Source, SourceSessionOptions,
    playlist::{KeyReference, PlaylistRejection, PlaylistSnapshot, SegmentDescriptor},
};
use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
use std::{
    fmt,
    sync::{Arc, Mutex},
};
use zeroize::{Zeroize, Zeroizing};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResourceErrorKind {
    InvalidOptions,
    InvalidIndex,
    UnsupportedPlaylist,
    UnconfirmedRange,
    InvalidRange,
    MissingMap,
    InvalidIv,
    ResourceTooLarge,
    BudgetExceeded,
    Read,
    Key,
    InvalidCiphertextLength,
    Decrypt,
    AuthenticationFailed,
    MediaValidation,
    Cancelled,
    CounterOverflow,
}
/// Safe default diagnostics; raw source/media causes require explicit access.
pub struct ResourceError {
    kind: ResourceErrorKind,
    resource: Option<Arc<KeyResource>>,
    key: Option<Box<KeyError>>,
    cause: Option<crate::Error>,
    rejection: Option<PlaylistRejection>,
    selected_reference: Option<Arc<KeyReference>>,
    key_version: Option<KeyVersion>,
    resolve_revision: Option<u64>,
}
impl ResourceError {
    pub(crate) fn new(kind: ResourceErrorKind) -> Self {
        Self {
            kind,
            resource: None,
            key: None,
            cause: None,
            rejection: None,
            selected_reference: None,
            key_version: None,
            resolve_revision: None,
        }
    }
    fn at(mut self, request: &ResourceRequest) -> Self {
        self.resource = Some(Arc::new(request.resource.clone()));
        self
    }
    fn with_key(mut self, key: &ResolvedKey) -> Self {
        self.selected_reference = Some(Arc::new(key.reference().clone()));
        self.key_version = Some(key.version().clone());
        self.resolve_revision = Some(key.resolve_revision());
        self
    }
    pub fn key_reference(&self) -> Option<&KeyReference> {
        self.selected_reference
            .as_deref()
            .or_else(|| self.key.as_ref().and_then(|k| k.reference()))
    }
    pub fn key_version(&self) -> Option<&KeyVersion> {
        self.key_version.as_ref()
    }
    pub fn resolve_revision(&self) -> Option<u64> {
        self.resolve_revision
    }
    pub fn kind(&self) -> ResourceErrorKind {
        self.kind
    }
    pub fn resource(&self) -> Option<&KeyResource> {
        self.resource.as_deref()
    }
    pub fn key_error(&self) -> Option<&KeyError> {
        self.key.as_deref()
    }
    pub fn raw_cause(&self) -> Option<&crate::Error> {
        self.cause.as_ref()
    }
    pub fn playlist_rejection(&self) -> Option<PlaylistRejection> {
        self.rejection
    }
    fn from_key(key: KeyError) -> Self {
        Self {
            kind: if key.kind() == KeyErrorKind::Cancelled {
                ResourceErrorKind::Cancelled
            } else {
                ResourceErrorKind::Key
            },
            key: Some(Box::new(key)),
            ..Self::new(ResourceErrorKind::Key)
        }
    }
    fn from_cause(kind: ResourceErrorKind, cause: crate::Error) -> Self {
        Self {
            kind: if matches!(cause, crate::Error::Cancelled) {
                ResourceErrorKind::Cancelled
            } else {
                kind
            },
            cause: Some(cause),
            ..Self::new(kind)
        }
    }
}
impl fmt::Debug for ResourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResourceError")
            .field("kind", &self.kind)
            .field("resource", &self.resource)
            .field("key", &self.key)
            .field("rejection", &self.rejection)
            .field("key_reference", &self.key_reference())
            .field("key_version", &self.key_version)
            .field("resolve_revision", &self.resolve_revision)
            .finish_non_exhaustive()
    }
}
impl fmt::Display for ResourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "resource preparation {:?}", self.kind)
    }
}
impl std::error::Error for ResourceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.key.as_deref().map(|e| e as _)
    }
}
pub type ResourceResult<T> = Result<T, ResourceError>;

/// A resource selected from a finite, preflight-validated P1 snapshot.
#[derive(Debug, Clone)]
pub struct ResourceRequest {
    packed: bool,
    resource: KeyResource,
    segment: SegmentDescriptor,
}
impl ResourceRequest {
    pub fn media(snapshot: &PlaylistSnapshot, index: usize) -> ResourceResult<Self> {
        Self::from_snapshot(snapshot, index, false)
    }
    pub fn map(snapshot: &PlaylistSnapshot, index: usize) -> ResourceResult<Self> {
        Self::from_snapshot(snapshot, index, true)
    }
    fn from_snapshot(snapshot: &PlaylistSnapshot, index: usize, map: bool) -> ResourceResult<Self> {
        snapshot
            .validate_finite_vod()
            .map_err(|rejection| ResourceError {
                rejection: Some(rejection),
                ..ResourceError::new(ResourceErrorKind::UnsupportedPlaylist)
            })?;
        Self::from_validated(snapshot, index, map)
    }
    // The prepared entry validates each immutable snapshot once before any I/O.
    pub(crate) fn from_validated(
        snapshot: &PlaylistSnapshot,
        index: usize,
        map: bool,
    ) -> ResourceResult<Self> {
        let segment = snapshot
            .segments()
            .get(index)
            .ok_or_else(|| ResourceError::new(ResourceErrorKind::InvalidIndex))?
            .clone();
        Self::from_descriptor(segment, map)
    }
    /// Internal admission has already validated and reconciled this descriptor.
    pub(crate) fn from_descriptor(segment: SegmentDescriptor, map: bool) -> ResourceResult<Self> {
        let resource = if map {
            KeyResource::map(&segment)
                .ok_or_else(|| ResourceError::new(ResourceErrorKind::MissingMap))?
        } else {
            KeyResource::media(&segment)
        };
        Ok(Self {
            resource,
            segment,
            packed: false,
        })
    }
    pub(crate) fn with_packed(mut self, enabled: bool) -> Self {
        self.packed = enabled;
        self
    }
    pub fn resource(&self) -> &KeyResource {
        &self.resource
    }
    pub fn segment(&self) -> &SegmentDescriptor {
        &self.segment
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EncryptedRangePolicy {
    /// Encryption boundaries cannot be inferred from block alignment.
    Reject,
    /// Caller attests each range is one independently padded, complete HLS resource.
    CompleteResources,
}
#[derive(Debug, Clone)]
pub struct ResourceOptions {
    max_resource_bytes: u64,
    max_samples: usize,
    max_waiting_bytes: u64,
    max_resources: usize,
    ranges: EncryptedRangePolicy,
    gcm: bool,
}
impl Default for ResourceOptions {
    fn default() -> Self {
        Self {
            max_resource_bytes: 16 * 1024 * 1024,
            max_samples: 65_536,
            max_waiting_bytes: 32 * 1024 * 1024,
            max_resources: 2,
            ranges: EncryptedRangePolicy::Reject,
            gcm: false,
        }
    }
}
impl ResourceOptions {
    /// Enable only the fixed HLS draft-22 authenticated resource profile.
    /// Requires the `experimental-gcm` Cargo feature as well.
    pub fn with_experimental_gcm(mut self, enabled: bool) -> Self {
        self.gcm = enabled;
        self
    }
    pub fn experimental_gcm(&self) -> bool {
        self.gcm && cfg!(feature = "experimental-gcm")
    }

    /// Maximum raw samples retained while processing one resource.
    pub fn with_sample_limit(mut self, samples: usize) -> Self {
        self.max_samples = samples;
        self
    }
    pub fn with_limits(
        mut self,
        resource_bytes: u64,
        waiting_bytes: u64,
        resources: usize,
    ) -> Self {
        self.max_resource_bytes = resource_bytes;
        self.max_waiting_bytes = waiting_bytes;
        self.max_resources = resources;
        self
    }
    pub fn with_encrypted_ranges(mut self, policy: EncryptedRangePolicy) -> Self {
        self.ranges = policy;
        self
    }
    pub(crate) fn validate(&self) -> ResourceResult<()> {
        if self.max_samples == 0
            || self.max_resource_bytes == 0
            || self.max_waiting_bytes < self.max_resource_bytes
            || self.max_resources == 0
            || usize::try_from(self.max_resource_bytes).is_err()
        {
            return Err(ResourceError::new(ResourceErrorKind::InvalidOptions));
        }
        Ok(())
    }
    pub fn encrypted_ranges(&self) -> EncryptedRangePolicy {
        self.ranges
    }
    pub(crate) fn preflight(
        &self,
        keys: &KeySession,
        request: &ResourceRequest,
    ) -> ResourceResult<()> {
        (|| {
            self.request_size(request)?;
            if request.segment.map().is_none()
                && request
                    .resource
                    .keys()
                    .candidates()
                    .first()
                    .is_some_and(|k| {
                        matches!(k.method(), crate::playlist::EncryptionMethod::SampleAesCtr)
                    })
            {
                return Err(ResourceError::new(ResourceErrorKind::UnsupportedPlaylist));
            }
            keys.validate_resource(&request.resource)
                .map_err(ResourceError::from_key)
        })()
        .map_err(|e| e.at(request))
    }
    fn request_size(&self, request: &ResourceRequest) -> ResourceResult<u64> {
        if gcm_method(request) && !self.experimental_gcm() {
            return Err(ResourceError::new(ResourceErrorKind::UnsupportedPlaylist));
        }
        let encrypted = !request.resource.keys().is_clear();
        let range = request.resource.range().map(|r| r.byte_range());
        if encrypted && range.is_some() && self.ranges != EncryptedRangePolicy::CompleteResources {
            return Err(ResourceError::new(ResourceErrorKind::UnconfirmedRange));
        }
        let reserved = range.map_or(self.max_resource_bytes, |r| r.length);
        if reserved == 0 || range.is_some_and(|r| r.offset.checked_add(r.length).is_none()) {
            return Err(ResourceError::new(ResourceErrorKind::InvalidRange));
        }
        if reserved > self.max_resource_bytes {
            return Err(ResourceError::new(ResourceErrorKind::ResourceTooLarge));
        }
        if encrypted
            && !sample_method(request)
            && !gcm_method(request)
            && range.is_some_and(|r| r.length % 16 != 0)
        {
            return Err(ResourceError::new(
                ResourceErrorKind::InvalidCiphertextLength,
            ));
        }
        Ok(reserved)
    }
    pub fn max_resource_bytes(&self) -> u64 {
        self.max_resource_bytes
    }
    pub fn max_waiting_bytes(&self) -> u64 {
        self.max_waiting_bytes
    }
    pub fn max_resources(&self) -> usize {
        self.max_resources
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClearContainer {
    PackedAac,
    TransportStream,
    Fmp4Init,
    Fmp4Media,
}
/// Resource bytes never appear in Debug or serialization. Owned bytes are wiped on drop.
pub struct ClearResource {
    bytes: Zeroizing<Vec<u8>>,
    request: ResourceRequest,
    container: ClearContainer,
    key_reference: Option<KeyReference>,
    key_version: Option<KeyVersion>,
    resolve_revision: Option<u64>,
    iv: Option<[u8; 16]>,
}
impl ClearResource {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn request(&self) -> &ResourceRequest {
        &self.request
    }
    pub fn container(&self) -> ClearContainer {
        self.container
    }
    pub fn key_reference(&self) -> Option<&KeyReference> {
        self.key_reference.as_ref()
    }
    pub fn key_version(&self) -> Option<&KeyVersion> {
        self.key_version.as_ref()
    }
    pub fn resolve_revision(&self) -> Option<u64> {
        self.resolve_revision
    }
    pub fn iv(&self) -> Option<[u8; 16]> {
        self.iv
    }
}
impl fmt::Debug for ClearResource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClearResource")
            .field("request", &self.request)
            .field("container", &self.container)
            .field("bytes", &self.bytes.len())
            .field("resolve_revision", &self.resolve_revision)
            .finish_non_exhaustive()
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceStats {
    resources: usize,
    reserved_bytes: u64,
    peak_raw_sample_bytes: usize,
    peak_replay_sample_bytes: usize,
}
impl ResourceStats {
    /// Peak retained raw sample payload capacity in one resource (not total RSS).
    pub fn peak_raw_sample_bytes(&self) -> usize {
        self.peak_raw_sample_bytes
    }
    /// Peak retained decrypted replay payload capacity in one resource.
    pub fn peak_replay_sample_bytes(&self) -> usize {
        self.peak_replay_sample_bytes
    }

    pub fn resources(&self) -> usize {
        self.resources
    }
    pub fn reserved_bytes(&self) -> u64 {
        self.reserved_bytes
    }
}
pub(crate) struct Permit {
    state: Arc<Mutex<ResourceStats>>,
    bytes: u64,
}
impl Drop for Permit {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap();
        state.resources -= 1;
        state.reserved_bytes -= self.bytes;
    }
}
// Only isolated source sessions can be stopped, never an unrelated shared source.
struct SourceLease {
    source: Arc<dyn Source>,
    isolated: bool,
}
impl Drop for SourceLease {
    fn drop(&mut self) {
        if self.isolated {
            self.source.stop_session();
        }
    }
}
#[derive(Clone, Copy)]
pub(crate) enum ResourceStage {
    Downloaded,
    Decrypted,
    Ready,
    MapReused,
}
pub(crate) struct ResourceObservation<'a> {
    pub resource: &'a KeyResource,
    pub stage: ResourceStage,
    pub bytes: u64,
}
#[cfg(not(target_arch = "wasm32"))]
type ResourceObserver = Arc<dyn Fn(ResourceObservation<'_>) -> ResourceResult<()> + Send + Sync>;
#[cfg(target_arch = "wasm32")]
type ResourceObserver = Arc<dyn Fn(ResourceObservation<'_>) -> ResourceResult<()>>;
/// Owns key lifecycle and resource admission for a single operation, shared by its inputs.
pub struct ResourceSession {
    keys: KeySession,
    options: ResourceOptions,
    state: Arc<Mutex<ResourceStats>>,
    cancel: RequestCancellation,
    observer: Option<ResourceObserver>,
    recovery: Mutex<Option<RecoveryKeys>>,
}
impl ResourceSession {
    pub fn new(mut keys: KeySession, options: ResourceOptions) -> ResourceResult<Self> {
        options.validate()?;
        keys.enable_gcm(options.experimental_gcm());
        Ok(Self {
            keys,
            options,
            state: Arc::new(Mutex::new(ResourceStats {
                resources: 0,
                reserved_bytes: 0,
                peak_raw_sample_bytes: 0,
                peak_replay_sample_bytes: 0,
            })),
            cancel: RequestCancellation::new(),
            observer: None,
            recovery: Mutex::new(None),
        })
    }
    pub(crate) fn with_observer(mut self, observer: ResourceObserver) -> Self {
        self.observer = Some(observer);
        self
    }
    fn observe(
        &self,
        request: &ResourceRequest,
        stage: ResourceStage,
        bytes: u64,
    ) -> ResourceResult<()> {
        self.check()?;
        if let Some(observer) = &self.observer {
            observer(ResourceObservation {
                resource: &request.resource,
                stage,
                bytes,
            })?;
        }
        self.check()
    }
    pub fn stats(&self) -> ResourceStats {
        *self.state.lock().unwrap()
    }
    pub fn invalidate_keys(&self) -> ResourceResult<()> {
        self.keys.invalidate().map_err(ResourceError::from_key)
    }
    pub fn cancel(&self) {
        self.cancel.cancel();
        self.keys.cancel();
    }
    fn check(&self) -> ResourceResult<()> {
        if self.cancel.is_cancelled() {
            Err(ResourceError::new(ResourceErrorKind::Cancelled))
        } else {
            Ok(())
        }
    }
    fn reserve(&self, bytes: u64) -> ResourceResult<Permit> {
        let mut state = self.state.lock().unwrap();
        let total = state
            .reserved_bytes
            .checked_add(bytes)
            .ok_or_else(|| ResourceError::new(ResourceErrorKind::BudgetExceeded))?;
        if state.resources >= self.options.max_resources || total > self.options.max_waiting_bytes {
            return Err(ResourceError::new(ResourceErrorKind::BudgetExceeded));
        }
        state.resources += 1;
        state.reserved_bytes = total;
        Ok(Permit {
            state: self.state.clone(),
            bytes,
        })
    }
    /// Demand-driven read: admission failure never invokes source/provider I/O.
    /// Dropping this future releases buffers, key waiter and source lease.
    pub async fn read(
        &self,
        source: Arc<dyn Source>,
        request: ResourceRequest,
    ) -> ResourceResult<ClearResource> {
        if sample_method(&request) {
            return Err(ResourceError::new(ResourceErrorKind::UnsupportedPlaylist).at(&request));
        }
        self.read_inner(source, &request, None)
            .await
            .map_err(|e| e.at(&request))
    }
    /// One MAP per input; compare full declaration context and the current resolution,
    /// never just its URI. Revalidation also observes TTL, eviction and invalidation.
    pub(crate) async fn read_map_cached(
        &self,
        source: Arc<dyn Source>,
        request: ResourceRequest,
        cached: &mut Option<ClearResource>,
    ) -> ResourceResult<()> {
        let result = async {
            self.check()?;
            let same_context = cached.as_ref().is_some_and(|old|
                old.request.segment.map() == request.segment.map()
                && old.request.resource.slot().input_id() == request.resource.slot().input_id()
                && old.request.resource.slot().generation() == request.resource.slot().generation()
                && old.request.resource.slot().epoch() == request.resource.slot().epoch());
            let mut resolved = None;
            if same_context {
                if request.resource.keys().is_clear() { return self.observe(&request, ResourceStage::MapReused, 0); }
                let waiter = self.keys.try_resolve(request.resource.clone()).map_err(ResourceError::from_key)?;
                let key = tokio::select! {
                    biased;
                    _ = self.cancel.cancelled() => return Err(ResourceError::new(ResourceErrorKind::Cancelled)),
                    result = waiter => result.map_err(ResourceError::from_key)?,
                };
                if !self.keys.key_is_valid(&key) {
                    return Err(ResourceError::from_key(KeyError::new(KeyErrorKind::Expired)));
                }
                self.record_recovery_key(&request.resource, &key).map_err(ResourceError::from_key)?;
                let old = cached.as_ref().unwrap();
                if old.key_reference.as_ref() == Some(key.reference())
                    && old.key_version.as_ref() == Some(key.version())
                    && old.resolve_revision == Some(key.resolve_revision()) {
                    return self.observe(&request, ResourceStage::MapReused, 0);
                }
                resolved = Some(key);
            }
            *cached = None;
            *cached = Some(self.read_inner(source, &request, resolved).await?);
            Ok(())
        }.await;
        result.map_err(|e: ResourceError| e.at(&request))
    }
    async fn read_inner(
        &self,
        source: Arc<dyn Source>,
        request: &ResourceRequest,
        resolved: Option<Arc<ResolvedKey>>,
    ) -> ResourceResult<ClearResource> {
        self.check()?;
        let encrypted = !request.resource.keys().is_clear();
        let range = request.resource.range().map(|r| r.byte_range());
        let reserved = self.options.request_size(request)?;
        let _permit = self.reserve(reserved)?;
        // Reserve a key waiter before reading, but do not poll the provider until bytes arrive.
        let waiter = if encrypted && resolved.is_none() {
            Some(
                self.keys
                    .try_resolve(request.resource.clone())
                    .map_err(ResourceError::from_key)?,
            )
        } else {
            None
        };
        let options = SourceSessionOptions {
            demand_driven: true,
            max_resource_bytes: Some(reserved),
        };
        let isolated = source.create_session_with_options(&options);
        let lease = SourceLease {
            isolated: isolated.is_some(),
            source: isolated.unwrap_or(source),
        };
        self.check()?;
        let data=tokio::select! {
            biased;
            _=self.cancel.cancelled()=>return Err(ResourceError::new(ResourceErrorKind::Cancelled)),
            data=async { lease.source.read_bytes(request.resource.location().location(),range.as_ref()).await }=>data,
        }.map_err(|e|ResourceError::from_cause(if crate::source::is_resource_limit(&e) { ResourceErrorKind::ResourceTooLarge } else { ResourceErrorKind::Read },e))?;
        let mut bytes = Zeroizing::new(data);
        self.check()?;
        if bytes.len() as u64 > reserved {
            return Err(ResourceError::new(ResourceErrorKind::ResourceTooLarge));
        }
        self.observe(request, ResourceStage::Downloaded, bytes.len() as u64)?;
        if range.is_some_and(|r| bytes.len() as u64 != r.length) {
            return Err(ResourceError::new(ResourceErrorKind::InvalidRange));
        }
        if encrypted && !gcm_method(request) && (bytes.is_empty() || bytes.len() % 16 != 0) {
            return Err(ResourceError::new(
                ResourceErrorKind::InvalidCiphertextLength,
            ));
        }
        let key = if let Some(key) = resolved {
            Some(key)
        } else if let Some(waiter) = waiter {
            let key = tokio::select! {
                biased;
                _=self.cancel.cancelled()=>return Err(ResourceError::new(ResourceErrorKind::Cancelled)),
                result=waiter=>result.map_err(ResourceError::from_key)?,
            };
            if !self.keys.key_is_valid(&key) {
                return Err(ResourceError::from_key(KeyError::new(
                    KeyErrorKind::Expired,
                )));
            }
            Some(key)
        } else {
            None
        };
        self.check()?;
        if key.as_ref().is_some_and(|key| !self.keys.key_is_valid(key)) {
            return Err(ResourceError::from_key(KeyError::new(
                KeyErrorKind::Expired,
            )));
        }
        if let Some(key) = &key {
            self.record_recovery_key(&request.resource, key)
                .map_err(ResourceError::from_key)?;
        }
        let iv = key
            .as_ref()
            .map(|key| match key.reference().explicit_iv() {
                _ if gcm_method(request) => bytes
                    .get(..16)
                    .and_then(|v| v.try_into().ok())
                    .ok_or_else(|| ResourceError::new(ResourceErrorKind::InvalidCiphertextLength)),
                Some(iv) => Ok(iv),
                None if request.resource.kind() == KeyResourceKind::Media => {
                    Ok(sequence_iv(request.resource.slot().sequence()))
                }
                None => Err(ResourceError::new(ResourceErrorKind::InvalidIv)),
            })
            .transpose()?;
        if let (Some(key), Some(iv)) = (&key, iv) {
            if gcm_method(request) {
                decrypt_gcm(&mut bytes, key.secret().expose(), &iv).map_err(|e| e.with_key(key))?;
            } else {
                decrypt(&mut bytes, key.secret().expose(), &iv).map_err(|e| e.with_key(key))?;
            }
            self.observe(request, ResourceStage::Decrypted, bytes.len() as u64)?;
        }
        self.check()?;
        let container = validate_container(&bytes, request).map_err(|e| {
            let error = ResourceError::from_cause(ResourceErrorKind::MediaValidation, e);
            if let Some(key) = &key {
                error.with_key(key)
            } else {
                error
            }
        })?;
        self.check()?;
        self.observe(request, ResourceStage::Ready, bytes.len() as u64)?;
        Ok(ClearResource {
            bytes,
            request: request.clone(),
            container,
            key_reference: key.as_ref().map(|k| k.reference().clone()),
            key_version: key.as_ref().map(|k| k.version().clone()),
            resolve_revision: key.as_ref().map(|k| k.resolve_revision()),
            iv,
        })
    }
}
impl Drop for ResourceSession {
    fn drop(&mut self) {
        self.cancel();
    }
}
/// HLS sequence IV: original u64 media sequence left-padded to 128 big-endian bits.
pub fn sequence_iv(sequence: u64) -> [u8; 16] {
    let mut iv = [0; 16];
    iv[8..].copy_from_slice(&sequence.to_be_bytes());
    iv
}
fn decrypt(bytes: &mut Vec<u8>, key: &[u8], iv: &[u8; 16]) -> ResourceResult<()> {
    if bytes.is_empty() || !bytes.len().is_multiple_of(16) {
        return Err(ResourceError::new(
            ResourceErrorKind::InvalidCiphertextLength,
        ));
    }
    let cipher = cbc::Decryptor::<aes::Aes128>::new_from_slices(key, iv)
        .map_err(|_| ResourceError::new(ResourceErrorKind::Decrypt))?;
    let length = cipher
        .decrypt_padded_mut::<Pkcs7>(bytes)
        .map_err(|_| ResourceError::new(ResourceErrorKind::Decrypt))?
        .len();
    bytes[length..].zeroize();
    bytes.truncate(length);
    Ok(())
}
fn validate_container(bytes: &[u8], request: &ResourceRequest) -> crate::Result<ClearContainer> {
    if request.packed && bytes.starts_with(b"ID3") {
        if request.segment.map().is_some() || request.resource.kind() == KeyResourceKind::Map {
            return Err(crate::Error::unsupported("Packed AAC cannot use MAP"));
        }
        crate::raw_sample::packed::validate(bytes)?;
        return Ok(ClearContainer::PackedAac);
    }
    if request.resource.kind() == KeyResourceKind::Map {
        crate::isobmff::validate_resource_envelope(bytes, true)?;
        return Ok(ClearContainer::Fmp4Init);
    }
    if request.segment.map().is_some() {
        crate::isobmff::validate_resource_envelope(bytes, false)?;
        return Ok(ClearContainer::Fmp4Media);
    }
    if bytes.is_empty() || !bytes.len().is_multiple_of(188) {
        return Err(crate::Error::bitstream("invalid TS resource size"));
    }
    let mut pat = false;
    for packet in bytes.as_chunks::<188>().0 {
        if packet[0] != 0x47
            || packet[1] & 0x80 != 0
            || packet[3] & 0xc0 != 0
            || packet[3] & 0x30 == 0
        {
            return Err(crate::Error::bitstream("invalid or scrambled TS packet"));
        }
        let control = (packet[3] >> 4) & 3;
        if (control == 2 && packet[4] != 183) || (control == 3 && packet[4] > 182) {
            return Err(crate::Error::bitstream("invalid TS adaptation size"));
        }
        pat |= packet[1] & 0x1f == 0 && packet[2] == 0 && packet[1] & 0x40 != 0;
    }
    if !pat {
        return Err(crate::Error::bitstream("TS resource missing PAT"));
    }
    Ok(ClearContainer::TransportStream)
}

/// Internal resource envelope: sample-protected bytes are never returned as ClearResource.
pub(crate) enum EncodedResource {
    Clear(Box<ClearResource>),
    Samples {
        bytes: Zeroizing<Vec<u8>>,
        request: Box<ResourceRequest>,
    },
}
impl EncodedResource {
    pub(crate) fn bytes(&self) -> &[u8] {
        match self {
            Self::Clear(v) => v.bytes(),
            Self::Samples { bytes, .. } => bytes,
        }
    }
    fn request(&self) -> &ResourceRequest {
        match self {
            Self::Clear(v) => v.request(),
            Self::Samples { request, .. } => request,
        }
    }
}
fn sample_method(request: &ResourceRequest) -> bool {
    request
        .resource
        .keys()
        .candidates()
        .first()
        .is_some_and(|k| {
            matches!(
                k.method(),
                crate::playlist::EncryptionMethod::SampleAes
                    | crate::playlist::EncryptionMethod::SampleAesCtr
            )
        })
}
impl ResourceSession {
    pub(crate) fn observe_sample_buffers(&self, raw: usize, replay: usize) {
        let mut stats = self.state.lock().unwrap();
        stats.peak_raw_sample_bytes = stats.peak_raw_sample_bytes.max(raw);
        stats.peak_replay_sample_bytes = stats.peak_replay_sample_bytes.max(replay);
    }
    pub(crate) fn reserve_samples(&self, bytes: usize) -> ResourceResult<Permit> {
        self.check()?;
        self.reserve(bytes as u64)
    }
    pub(crate) fn sample_limits(&self) -> (usize, usize) {
        (
            self.options.max_samples,
            self.options.max_waiting_bytes as usize,
        )
    }
    pub(crate) async fn resolve_sample_key(
        &self,
        resource: KeyResource,
    ) -> Result<Arc<ResolvedKey>, KeyError> {
        let waiter = self.keys.try_resolve(resource.clone())?;
        let key = tokio::select! {biased; _=self.cancel.cancelled()=>return Err(KeyError::new(KeyErrorKind::Cancelled)),result=waiter=>result?};
        if !self.keys.key_is_valid(&key) {
            return Err(KeyError::new(KeyErrorKind::Expired));
        }
        self.record_recovery_key(&resource, &key)?;
        Ok(key)
    }
    pub(crate) async fn read_encoded(
        &self,
        source: Arc<dyn Source>,
        request: ResourceRequest,
    ) -> ResourceResult<EncodedResource> {
        if !sample_method(&request) {
            return self
                .read(source, request)
                .await
                .map(|v| EncodedResource::Clear(Box::new(v)));
        }
        let result=async {
            self.check()?;
            let reserved=self.options.request_size(&request)?;let _permit=self.reserve(reserved)?;
            let settings=SourceSessionOptions{demand_driven:true,max_resource_bytes:Some(reserved)};
            let isolated=source.create_session_with_options(&settings);
            let lease=SourceLease{isolated:isolated.is_some(),source:isolated.unwrap_or(source)};
            let range=request.resource.range().map(|r|r.byte_range());
            let bytes=tokio::select!{biased;_=self.cancel.cancelled()=>return Err(ResourceError::new(ResourceErrorKind::Cancelled)),v=lease.source.read_bytes(request.resource.location().location(),range.as_ref())=>v}
                .map_err(|e|ResourceError::from_cause(if crate::source::is_resource_limit(&e){ResourceErrorKind::ResourceTooLarge}else{ResourceErrorKind::Read},e))?;
            let bytes=Zeroizing::new(bytes);
            self.check()?;
            if bytes.len() as u64 > reserved {return Err(ResourceError::new(ResourceErrorKind::ResourceTooLarge));}
            if range.is_some_and(|r|r.length!=bytes.len() as u64){return Err(ResourceError::new(ResourceErrorKind::InvalidRange));}
            self.observe(&request,ResourceStage::Downloaded,bytes.len() as u64)?;
            if request.resource.kind()==KeyResourceKind::Map || request.segment.map().is_some() {
                crate::isobmff::validate_sample_envelope(&bytes,request.resource.kind()==KeyResourceKind::Map)
            }else {validate_container(&bytes,&request).map(|_|())}
                .map_err(|e|ResourceError::from_cause(ResourceErrorKind::MediaValidation,e))?;
            self.observe(&request,ResourceStage::Ready,bytes.len() as u64)?;
            Ok(bytes)
        }.await.map_err(|e:ResourceError|e.at(&request))?;
        Ok(EncodedResource::Samples {
            bytes: result,
            request: Box::new(request),
        })
    }
    pub(crate) async fn read_encoded_map(
        &self,
        source: Arc<dyn Source>,
        request: ResourceRequest,
        cached: &mut Option<EncodedResource>,
    ) -> ResourceResult<()> {
        if sample_method(&request) {
            if cached.as_ref().is_some_and(|v| {
                matches!(v, EncodedResource::Samples { .. })
                    && v.request().segment.map() == request.segment.map()
                    && v.request().resource.slot().input_id() == request.resource.slot().input_id()
                    && v.request().resource.slot().generation()
                        == request.resource.slot().generation()
                    && v.request().resource.slot().epoch() == request.resource.slot().epoch()
            }) {
                return self.observe(&request, ResourceStage::MapReused, 0);
            }
            *cached = Some(self.read_encoded(source, request).await?);
            return Ok(());
        }
        let mut clear = match cached.take() {
            Some(EncodedResource::Clear(v)) => Some(*v),
            _ => None,
        };
        let result = self.read_map_cached(source, request, &mut clear).await;
        *cached = clear.map(|v| EncodedResource::Clear(Box::new(v)));
        result
    }
    pub(crate) fn sample_decrypted(
        &self,
        request: &ResourceRequest,
        bytes: u64,
    ) -> ResourceResult<()> {
        if sample_method(request) {
            self.observe(request, ResourceStage::Decrypted, bytes)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nist_cbc_blocks_and_independent_padded_vector() {
        fn wipes_on_drop<T: zeroize::ZeroizeOnDrop>() {}
        wipes_on_drop::<aes::Aes128>();
        wipes_on_drop::<cbc::Decryptor<aes::Aes128>>();
        use aes::cipher::block_padding::NoPadding;
        fn hex(s: &str) -> Vec<u8> {
            (0..s.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
                .collect()
        }
        let key = hex("2b7e151628aed2a6abf7158809cf4f3c");
        let iv = hex("000102030405060708090a0b0c0d0e0f");
        let mut ciphertext = hex(
            "7649abac8119b246cee98e9b12e9197d5086cb9b507219ee95db113a917678b273bed6b8e3c1743b7116e69e222295163ff1caa1681fac09120eca307586e1a7",
        );
        let clear = hex(
            "6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e5130c81c46a35ce411e5fbc1191a0a52eff69f2445df4f9b17ad2b417be66c3710",
        );
        assert_eq!(
            cbc::Decryptor::<aes::Aes128>::new_from_slices(&key, &iv)
                .unwrap()
                .decrypt_padded_mut::<NoPadding>(&mut ciphertext)
                .unwrap(),
            clear
        );
        let mut padded = include_bytes!("../../tests/fixtures/crypto/vector.cbc").to_vec();
        decrypt(&mut padded, &key, &iv.try_into().unwrap()).unwrap();
        assert_eq!(padded, clear);
        assert_eq!(
            sequence_iv(u64::MAX),
            [
                0, 0, 0, 0, 0, 0, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255
            ]
        );
    }
}

fn gcm_method(request: &ResourceRequest) -> bool {
    request
        .resource
        .keys()
        .candidates()
        .first()
        .is_some_and(|k| *k.method() == crate::playlist::EncryptionMethod::Aes256Gcm)
}
fn decrypt_gcm(bytes: &mut Vec<u8>, key: &[u8], iv: &[u8; 16]) -> ResourceResult<()> {
    #[cfg(feature = "experimental-gcm")]
    {
        use aes_gcm::{
            AesGcm,
            aead::{AeadInPlace, KeyInit, consts::U16},
        };
        if bytes.len() < 32 {
            return Err(ResourceError::new(
                ResourceErrorKind::InvalidCiphertextLength,
            ));
        }
        let end = bytes.len() - 16;
        let tag: [u8; 16] = bytes[end..].try_into().unwrap();
        let cipher = AesGcm::<aes::Aes256, U16>::new_from_slice(key)
            .map_err(|_| ResourceError::new(ResourceErrorKind::Decrypt))?;
        cipher
            .decrypt_in_place_detached(iv.into(), &[], &mut bytes[16..end], (&tag).into())
            .map_err(|_| ResourceError::new(ResourceErrorKind::AuthenticationFailed))?;
        bytes.copy_within(16..end, 0);
        let size = end - 16;
        bytes[size..].zeroize();
        bytes.truncate(size);
        Ok(())
    }
    #[cfg(not(feature = "experimental-gcm"))]
    {
        let _ = (bytes, key, iv);
        Err(ResourceError::new(ResourceErrorKind::UnsupportedPlaylist))
    }
}

#[derive(Clone)]
struct RecoveryKey {
    slot: crate::playlist::SegmentSlot,
    map: bool,
    reference: [u8; 32],
    kid: Option<[u8; 16]>,
    version: Option<[u8; 32]>,
}
crate::state_codec::state_struct!(RecoveryKey {
    slot,
    map,
    reference,
    kid,
    version
});
#[derive(Default)]
struct RecoveryKeys {
    current: Vec<RecoveryKey>,
    expected: Vec<RecoveryKey>,
}
impl ResourceSession {
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn enable_recovery(&self) {
        *self.recovery.lock().unwrap() = Some(RecoveryKeys::default());
    }
    pub(crate) fn recovery_enabled(&self) -> bool {
        self.recovery.lock().unwrap().is_some()
    }
    fn record_recovery_key(
        &self,
        resource: &KeyResource,
        key: &ResolvedKey,
    ) -> Result<(), KeyError> {
        let mut state = self.recovery.lock().unwrap();
        let Some(state) = state.as_mut() else {
            return Ok(());
        };
        let binding = RecoveryKey {
            slot: resource.slot().clone(),
            map: resource.kind() == KeyResourceKind::Map,
            reference: key.reference().checkpoint_identity(),
            kid: resource.kid(),
            version: match key.version() {
                KeyVersion::Provider(v) => Some(crate::resume::digest(v.as_bytes())),
                KeyVersion::Revision(_) => None,
            },
        };
        let same = |old: &&RecoveryKey| {
            old.slot == binding.slot && old.map == binding.map && old.kid == binding.kid
        };
        if let Some(expected) = state.expected.iter().find(same)
            && (binding.reference != expected.reference
                || binding.version.is_none()
                || binding.version != expected.version)
        {
            return Err(KeyError::new(KeyErrorKind::ResumeConflict));
        }
        if let Some(old) = state.current.iter_mut().find(|old| {
            old.slot == binding.slot && old.map == binding.map && old.kid == binding.kid
        }) {
            *old = binding;
        } else {
            if state.current.len() >= self.options.max_samples {
                return Err(KeyError::new(KeyErrorKind::BudgetExceeded));
            }
            state.current.push(binding);
        }
        Ok(())
    }
    pub(crate) fn committed_keys(&self, slot: &crate::playlist::SegmentSlot) {
        if let Some(state) = self.recovery.lock().unwrap().as_mut() {
            state.current.retain(|b| &b.slot != slot);
            state.expected.retain(|b| &b.slot != slot);
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn save_recovery_keys(&self) -> ResourceResult<Vec<u8>> {
        use crate::state_codec::StateCodec;
        let state = self.recovery.lock().unwrap();
        let mut bytes = Vec::new();
        if let Some(state) = state.as_ref() {
            if state.current.iter().any(|b| b.version.is_none()) {
                return Err(ResourceError::from_key(KeyError::new(
                    KeyErrorKind::ResumeConflict,
                )));
            }
            state.current.put(&mut bytes);
        } else {
            Vec::<RecoveryKey>::new().put(&mut bytes);
        }
        Ok(bytes)
    }
    pub(crate) fn restore_recovery_keys(&self, bytes: &[u8]) -> ResourceResult<()> {
        use crate::state_codec::{Reader, StateCodec};
        let bad = || ResourceError::from_key(KeyError::new(KeyErrorKind::ResumeConflict));
        let mut reader = Reader(bytes);
        let expected = Vec::<RecoveryKey>::get(&mut reader).map_err(|_| bad())?;
        if !reader.0.is_empty() || expected.iter().any(|b| b.version.is_none()) {
            return Err(bad());
        }
        *self.recovery.lock().unwrap() = Some(RecoveryKeys {
            expected,
            current: vec![],
        });
        Ok(())
    }
}

#[cfg(all(test, feature = "experimental-gcm"))]
mod gcm_vectors {
    use super::*;

    #[test]
    fn independent_non_block_vector_and_wrong_key() {
        let encoded = include_bytes!("../../tests/fixtures/gcm/vector.gcm");
        let iv: [u8; 16] = encoded[..16].try_into().unwrap();
        let key: Vec<_> = (0..32).collect();
        let mut clear = encoded.to_vec();
        decrypt_gcm(&mut clear, &key, &iv).unwrap();
        assert_eq!(clear, b"HLS draft-22 uses a sixteen-byte IV.");
        let mut bad_key = key;
        bad_key[0] ^= 1;
        assert_eq!(
            decrypt_gcm(&mut encoded.to_vec(), &bad_key, &iv)
                .unwrap_err()
                .kind(),
            ResourceErrorKind::AuthenticationFailed
        );
        for length in 0..32 {
            assert_eq!(
                decrypt_gcm(&mut encoded[..length].to_vec(), &bad_key, &iv)
                    .unwrap_err()
                    .kind(),
                ResourceErrorKind::InvalidCiphertextLength
            );
        }
    }
}
