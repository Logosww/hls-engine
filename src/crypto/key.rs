//! Operation-scoped key resolution, without HTTP or runtime-owned tasks.
#![doc = include_str!("../../docs/key-sessions.md")]

use crate::playlist::{
    EncryptionMethod, KeyContext, KeyReference, ResourceLocation, ResourceRange, SegmentDescriptor,
    SegmentSlot,
};
use futures_util::{
    FutureExt,
    future::{AbortHandle, Abortable, Shared},
};
use std::{
    fmt,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, Weak},
    task::{Context, Poll},
};
use zeroize::Zeroizing;

/// Native providers/clocks are thread safe; browser implementations may own JS values.
#[cfg(not(target_arch = "wasm32"))]
pub trait ProviderBounds: Send + Sync {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + Sync + ?Sized> ProviderBounds for T {}
#[cfg(target_arch = "wasm32")]
pub trait ProviderBounds {}
#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> ProviderBounds for T {}
#[cfg(not(target_arch = "wasm32"))]
pub type KeyFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;
#[cfg(target_arch = "wasm32")]
pub type KeyFuture<T> = Pin<Box<dyn Future<Output = T> + 'static>>;
#[cfg(not(target_arch = "wasm32"))]
pub type ProviderCause = dyn std::error::Error + Send + Sync + 'static;
#[cfg(target_arch = "wasm32")]
pub type ProviderCause = dyn std::error::Error + 'static;

/// Caller clock and provider expiry use the same monotonic millisecond domain.
pub trait KeyClock: ProviderBounds {
    fn now(&self) -> u64;
}

/// Owned AES-128 bytes. No Clone or serialization; the last shared owner wipes this buffer.
pub struct SecretKey(Zeroizing<Vec<u8>>);
impl SecretKey {
    pub fn new(bytes: Vec<u8>) -> Result<Self, KeyError> {
        let bytes = Zeroizing::new(bytes);
        if bytes.len() != 16 {
            return Err(KeyError::new(KeyErrorKind::InvalidKey));
        }
        Ok(Self(bytes))
    }
    /// Explicit sensitive access for the resource decryptor. Do not log or persist.
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}
impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretKey([REDACTED])")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProviderFailureKind {
    Authorization,
    Transport,
    InvalidResponse,
}
#[derive(Clone)]
pub struct ProviderFailure {
    kind: ProviderFailureKind,
    cause: Arc<ProviderCause>,
}
impl ProviderFailure {
    pub fn new(kind: ProviderFailureKind, cause: Arc<ProviderCause>) -> Self {
        Self { kind, cause }
    }
    pub fn kind(&self) -> ProviderFailureKind {
        self.kind
    }
    /// Raw provider errors may contain credentials. Explicit access only.
    pub fn raw_cause(&self) -> &ProviderCause {
        &*self.cause
    }
}
impl fmt::Debug for ProviderFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderFailure")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeyErrorKind {
    Cancelled,
    BudgetExceeded,
    Unsupported,
    Unavailable,
    Provider,
    InvalidKey,
    ConflictingMetadata,
    Expired,
    InvalidOptions,
    RevisionOverflow,
}
#[derive(Debug, Clone)]
pub struct KeyError {
    kind: KeyErrorKind,
    failure: Option<ProviderFailure>,
    resource: Option<Arc<KeyResource>>,
    reference: Option<Arc<KeyReference>>,
}
impl KeyError {
    pub(super) fn new(kind: KeyErrorKind) -> Self {
        Self {
            kind,
            failure: None,
            resource: None,
            reference: None,
        }
    }
    pub fn kind(&self) -> KeyErrorKind {
        self.kind
    }
    pub fn provider_failure(&self) -> Option<&ProviderFailure> {
        self.failure.as_ref()
    }
    pub fn resource(&self) -> Option<&KeyResource> {
        self.resource.as_deref()
    }
    pub fn reference(&self) -> Option<&KeyReference> {
        self.reference.as_deref()
    }
    fn for_reference(mut self, reference: &KeyReference) -> Self {
        self.reference = Some(Arc::new(reference.clone()));
        self
    }
    fn at(mut self, resource: &KeyResource) -> Self {
        self.resource = Some(Arc::new(resource.clone()));
        self
    }
}
impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "key resolution {:?}", self.kind)
    }
}
impl std::error::Error for KeyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.failure.as_ref().map(|e| e as _)
    }
}
impl fmt::Display for ProviderFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "key provider {:?}", self.kind)
    }
}
impl std::error::Error for ProviderFailure {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeyResourceKind {
    Media,
    Map,
}
/// Immutable request context copied from validated P1 metadata.
#[derive(Debug, Clone)]
pub struct KeyResource {
    slot: SegmentSlot,
    location: ResourceLocation,
    range: Option<ResourceRange>,
    kind: KeyResourceKind,
    keys: KeyContext,
    kid: Option<[u8; 16]>,
}
impl KeyResource {
    pub fn media(segment: &SegmentDescriptor) -> Self {
        Self {
            slot: segment.slot().clone(),
            location: segment.location().clone(),
            range: segment.range(),
            kind: KeyResourceKind::Media,
            keys: segment.keys().clone(),
            kid: None,
        }
    }
    pub fn map(segment: &SegmentDescriptor) -> Option<Self> {
        segment.map().map(|map| Self {
            slot: segment.slot().clone(),
            location: map.location().clone(),
            range: map.range(),
            kind: KeyResourceKind::Map,
            keys: map.keys().clone(),
            kid: None,
        })
    }
    /// A trusted container parser may supply a KID; P2 does not extract one.
    pub fn with_kid(mut self, kid: [u8; 16]) -> Self {
        self.kid = Some(kid);
        self
    }
    pub fn slot(&self) -> &SegmentSlot {
        &self.slot
    }
    pub fn location(&self) -> &ResourceLocation {
        &self.location
    }
    pub fn range(&self) -> Option<ResourceRange> {
        self.range
    }
    pub fn kind(&self) -> KeyResourceKind {
        self.kind
    }
    pub fn keys(&self) -> &KeyContext {
        &self.keys
    }
    pub fn kid(&self) -> Option<[u8; 16]> {
        self.kid
    }
}

#[derive(Clone)]
pub struct RequestCancellation(tokio::sync::watch::Sender<bool>);
impl RequestCancellation {
    pub(super) fn new() -> Self {
        Self(tokio::sync::watch::channel(false).0)
    }
    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }
    pub async fn cancelled(&self) {
        let _ = self.0.subscribe().wait_for(|v| *v).await;
    }
    pub(super) fn cancel(&self) {
        self.0.send_replace(true);
    }
}
impl fmt::Debug for RequestCancellation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("RequestCancellation")
            .field(&self.is_cancelled())
            .finish()
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RefreshReason {
    Initial,
    Expired,
    Invalidated,
}
#[derive(Clone)]
pub struct KeyRequest {
    operation: Arc<str>,
    auth_scope: Arc<str>,
    resource: KeyResource,
    reference: KeyReference,
    revision: u64,
    refresh_generation: u64,
    reason: RefreshReason,
    cancellation: RequestCancellation,
}
impl KeyRequest {
    pub fn operation(&self) -> &str {
        &self.operation
    }
    pub fn auth_scope(&self) -> &str {
        &self.auth_scope
    }
    pub fn resource(&self) -> &KeyResource {
        &self.resource
    }
    pub fn reference(&self) -> &KeyReference {
        &self.reference
    }
    pub fn resolve_revision(&self) -> u64 {
        self.revision
    }
    pub fn refresh_generation(&self) -> u64 {
        self.refresh_generation
    }
    pub fn refresh_reason(&self) -> RefreshReason {
        self.reason
    }
    pub fn cancellation(&self) -> &RequestCancellation {
        &self.cancellation
    }
}
impl fmt::Debug for KeyRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyRequest")
            .field("resource", &self.resource)
            .field("reference", &self.reference)
            .field("revision", &self.revision)
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}
/// Providers must bind their result to the selected method and optional KID.
pub struct AvailableKey {
    secret: SecretKey,
    version: Option<String>,
    valid_until: Option<u64>,
    method: EncryptionMethod,
    kid: Option<[u8; 16]>,
}
impl fmt::Debug for AvailableKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AvailableKey")
            .field("secret", &self.secret)
            .field("method", &self.method)
            .finish_non_exhaustive()
    }
}
impl AvailableKey {
    pub fn aes128(secret: SecretKey) -> Self {
        Self {
            secret,
            version: None,
            valid_until: None,
            method: EncryptionMethod::Aes128,
            kid: None,
        }
    }
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }
    pub fn with_valid_until(mut self, valid_until: u64) -> Self {
        self.valid_until = Some(valid_until);
        self
    }
    pub fn with_kid(mut self, kid: [u8; 16]) -> Self {
        self.kid = Some(kid);
        self
    }
}
#[derive(Debug)]
#[non_exhaustive]
pub enum KeyResolution {
    Available(AvailableKey),
    Unavailable,
    Failure(ProviderFailure),
}
/// Transport and network retry belong to this adapter, never to KeySession.
pub trait KeyProvider: ProviderBounds {
    fn resolve(&self, request: KeyRequest) -> KeyFuture<KeyResolution>;
    /// Synchronously signal host AbortController (if any). Called outside session locks,
    /// once per abandoned in-flight resolve; completed requests need no abort.
    fn abort(&self, _request: &KeyRequest) {}
}
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeyVersion {
    Provider(String),
    Revision(u64),
}
impl fmt::Debug for KeyVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KeyVersion(..)")
    }
}
pub struct ResolvedKey {
    secret: SecretKey,
    version: KeyVersion,
    valid_until: Option<u64>,
    reference: KeyReference,
    revision: u64,
}
impl ResolvedKey {
    pub fn secret(&self) -> &SecretKey {
        &self.secret
    }
    pub fn version(&self) -> &KeyVersion {
        &self.version
    }
    pub fn valid_until(&self) -> Option<u64> {
        self.valid_until
    }
    pub fn reference(&self) -> &KeyReference {
        &self.reference
    }
    pub fn resolve_revision(&self) -> u64 {
        self.revision
    }
    pub fn is_valid_at(&self, now: u64) -> bool {
        self.valid_until.is_none_or(|end| now < end)
    }
}
impl fmt::Debug for ResolvedKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedKey")
            .field("reference", &self.reference)
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}
/// Formats in preference order. Only the listed versions are supported by the adapter.
#[derive(Clone)]
pub struct KeyFormatSupport {
    format: String,
    versions: Vec<u32>,
}
impl KeyFormatSupport {
    pub fn new(format: impl Into<String>, versions: Vec<u32>) -> Self {
        Self {
            format: format.into(),
            versions,
        }
    }
}
#[derive(Clone)]
pub struct KeySessionOptions {
    max_in_flight: usize,
    max_cached: usize,
    max_waiters: usize,
    formats: Vec<KeyFormatSupport>,
}
impl Default for KeySessionOptions {
    fn default() -> Self {
        Self {
            max_in_flight: 2,
            max_cached: 8,
            max_waiters: 2,
            formats: vec![KeyFormatSupport::new("identity", vec![1])],
        }
    }
}
impl KeySessionOptions {
    pub fn with_limits(
        mut self,
        in_flight: usize,
        cached: usize,
        waiting_resources: usize,
    ) -> Self {
        self.max_in_flight = in_flight;
        self.max_cached = cached;
        self.max_waiters = waiting_resources;
        self
    }
    pub fn with_formats(mut self, formats: Vec<KeyFormatSupport>) -> Self {
        self.formats = formats;
        self
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeySessionStats {
    in_flight: usize,
    cached: usize,
    waiting_resources: usize,
}

impl KeySessionStats {
    pub fn in_flight(&self) -> usize {
        self.in_flight
    }
    pub fn cached(&self) -> usize {
        self.cached
    }
    pub fn waiting_resources(&self) -> usize {
        self.waiting_resources
    }
}

// Resource sequence/URI is diagnostic context, not the key identity. The input,
// generation, epoch, complete candidate declarations and KID remain in the identity.
#[derive(Clone, PartialEq, Eq)]
struct Identity {
    input: crate::playlist::InputId,
    generation: u64,
    epoch: u64,
    candidates: Vec<KeyReference>,
    kid: Option<[u8; 16]>,
}
type Reply = Result<Arc<ResolvedKey>, KeyError>;
type RequestFuture = Shared<KeyFuture<Reply>>;
struct Pending {
    id: u64,
    identity: Identity,
    generation: u64,
    future: RequestFuture,
    abort: AbortHandle,
    cancellation: RequestCancellation,
    waiters: usize,
    active_request: Arc<Mutex<Option<KeyRequest>>>,
}
struct Cached {
    identity: Identity,
    key: Arc<ResolvedKey>,
}
struct State {
    cancelled: bool,
    next_revision: u64,
    generation: u64,
    pending: Vec<Pending>,
    cache: Vec<Cached>,
    waiters: usize,
}
/// One owner per operation and authorization scope. Dropping it cancels every waiter.
pub struct KeySession {
    state: Arc<Mutex<State>>,
    provider: Arc<dyn KeyProvider>,
    clock: Arc<dyn KeyClock>,
    operation: Arc<str>,
    auth_scope: Arc<str>,
    options: KeySessionOptions,
}
impl KeySession {
    // Keep shared ownership identical across targets; WASM providers may own JS values.
    #[cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]
    pub fn new(
        operation: impl Into<String>,
        auth_scope: impl Into<String>,
        provider: Arc<dyn KeyProvider>,
        clock: Arc<dyn KeyClock>,
        options: KeySessionOptions,
    ) -> Result<Self, KeyError> {
        let operation = operation.into();
        let auth_scope = auth_scope.into();
        if operation.is_empty()
            || auth_scope.is_empty()
            || options.max_in_flight == 0
            || options.max_cached == 0
            || options.max_waiters == 0
            || options
                .formats
                .iter()
                .any(|f| f.format.is_empty() || f.versions.is_empty() || f.versions.contains(&0))
        {
            return Err(KeyError::new(KeyErrorKind::InvalidOptions));
        }
        Ok(Self {
            state: Arc::new(Mutex::new(State {
                cancelled: false,
                next_revision: 0,
                generation: 0,
                pending: Vec::new(),
                cache: Vec::new(),
                waiters: 0,
            })),
            provider,
            clock,
            operation: operation.into(),
            auth_scope: auth_scope.into(),
            options,
        })
    }
    pub(crate) fn key_is_valid(&self, key: &ResolvedKey) -> bool {
        key.is_valid_at(self.clock.now())
    }
    pub fn stats(&self) -> KeySessionStats {
        let s = self.state.lock().unwrap();
        KeySessionStats {
            in_flight: s.pending.len(),
            cached: s.cache.len(),
            waiting_resources: s.waiters,
        }
    }
    /// Bounded admission: BudgetExceeded means wait for existing work before retrying.
    /// The core does not enqueue additional futures or read additional resources.
    pub fn try_resolve(&self, resource: KeyResource) -> Result<KeyWaiter, KeyError> {
        self.resolve_inner(resource.clone())
            .map_err(|e| e.at(&resource))
    }
    /// Checks method/KEYFORMAT/version/IV agreement without I/O, admission or cache mutation.
    /// A successful check does not establish provider availability or authorization.
    pub fn validate_resource(&self, resource: &KeyResource) -> Result<(), KeyError> {
        if resource.keys().is_clear() {
            return Ok(());
        }
        self.select_candidates(resource)
            .map(|_| ())
            .map_err(|e| e.at(resource))
    }
    fn select_candidates(&self, resource: &KeyResource) -> Result<Vec<KeyReference>, KeyError> {
        let candidates = resource.keys.candidates();
        let first = candidates
            .first()
            .ok_or_else(|| KeyError::new(KeyErrorKind::Unsupported))?;
        if candidates
            .iter()
            .any(|k| k.method() != first.method() || k.explicit_iv() != first.explicit_iv())
        {
            return Err(KeyError::new(KeyErrorKind::ConflictingMetadata));
        }
        if first.method() != &EncryptionMethod::Aes128 {
            return Err(KeyError::new(KeyErrorKind::Unsupported));
        }
        let mut selected = Vec::new();
        for support in &self.options.formats {
            for key in candidates {
                if key.format() == support.format
                    && key.versions().iter().any(|v| support.versions.contains(v))
                    && !selected.contains(key)
                {
                    selected.push(key.clone());
                }
            }
        }
        if selected.is_empty() {
            return Err(KeyError::new(KeyErrorKind::Unsupported));
        }
        Ok(selected)
    }
    fn resolve_inner(&self, resource: KeyResource) -> Result<KeyWaiter, KeyError> {
        let selected = self.select_candidates(&resource)?;
        let candidates = resource.keys.candidates();
        let identity = Identity {
            input: resource.slot.input_id().clone(),
            generation: resource.slot.generation(),
            epoch: resource.slot.epoch(),
            candidates: candidates.to_vec(),
            kid: resource.kid,
        };
        let now = self.clock.now(); // External clock may reenter: never invoke it under State lock.
        let mut s = self.state.lock().unwrap();
        if s.cancelled {
            return Err(KeyError::new(KeyErrorKind::Cancelled));
        }
        if s.waiters >= self.options.max_waiters {
            return Err(KeyError::new(KeyErrorKind::BudgetExceeded));
        }
        let generation = s.generation;
        let mut reason = if generation == 0 {
            RefreshReason::Initial
        } else {
            RefreshReason::Invalidated
        };
        let cached = s.cache.iter().position(|entry| entry.identity == identity);
        if let Some(index) = cached {
            let entry = s.cache.remove(index);
            if entry.key.is_valid_at(now) {
                let key = entry.key.clone();
                s.cache.push(entry);
                s.waiters += 1;
                let future: KeyFuture<Reply> = Box::pin(async move { Ok(key) });
                return Ok(self.waiter(None, future.shared(), resource));
            }
            reason = RefreshReason::Expired;
        }
        // Purge other expired entries too; vector order is least -> most recently used.
        s.cache.retain(|entry| entry.key.is_valid_at(now));
        if let Some(p) = s
            .pending
            .iter_mut()
            .find(|p| p.identity == identity && p.generation == generation)
        {
            p.waiters += 1;
            let id = p.id;
            let future = p.future.clone();
            s.waiters += 1;
            return Ok(self.waiter(Some(id), future, resource));
        }
        if s.pending.len() >= self.options.max_in_flight {
            return Err(KeyError::new(KeyErrorKind::BudgetExceeded));
        }
        let id = s
            .next_revision
            .checked_add(1)
            .ok_or_else(|| KeyError::new(KeyErrorKind::RevisionOverflow))?;
        s.next_revision = id;
        let cancellation = RequestCancellation::new();
        let active_request = Arc::new(Mutex::new(None));
        let active = active_request.clone();
        let template = KeyRequest {
            operation: self.operation.clone(),
            auth_scope: self.auth_scope.clone(),
            resource: resource.clone(),
            reference: selected[0].clone(),
            revision: id,
            refresh_generation: generation,
            reason,
            cancellation: cancellation.clone(),
        };
        let provider = self.provider.clone();
        let clock = self.clock.clone();
        let weak = Arc::downgrade(&self.state);
        let cache_identity = identity.clone();
        let max_cached = self.options.max_cached;
        let (abort, registration) = AbortHandle::new_pair();
        let resolve = async move {
            for reference in selected {
                let request = KeyRequest {
                    reference: reference.clone(),
                    ..template.clone()
                };
                if request.cancellation.is_cancelled() {
                    return Err(KeyError::new(KeyErrorKind::Cancelled));
                }
                *active.lock().unwrap() = Some(request.clone());
                let resolution = provider.resolve(request.clone()).await;
                active.lock().unwrap().take();
                if request.cancellation.is_cancelled() {
                    return Err(KeyError::new(KeyErrorKind::Cancelled));
                }
                match resolution {
                    KeyResolution::Unavailable => continue,
                    KeyResolution::Failure(failure) => {
                        return Err(KeyError {
                            kind: KeyErrorKind::Provider,
                            failure: Some(failure),
                            resource: None,
                            reference: Some(Arc::new(reference.clone())),
                        });
                    }
                    KeyResolution::Available(value) => {
                        if value.method != *reference.method() || value.kid != request.resource.kid
                        {
                            return Err(KeyError::new(KeyErrorKind::ConflictingMetadata)
                                .for_reference(&reference));
                        }
                        if value.version.as_ref().is_some_and(|v| v.is_empty()) {
                            return Err(
                                KeyError::new(KeyErrorKind::InvalidKey).for_reference(&reference)
                            );
                        }
                        return Ok(Arc::new(ResolvedKey {
                            secret: value.secret,
                            version: value
                                .version
                                .map(KeyVersion::Provider)
                                .unwrap_or(KeyVersion::Revision(id)),
                            valid_until: value.valid_until,
                            reference,
                            revision: id,
                        }));
                    }
                }
            }
            Err(KeyError::new(KeyErrorKind::Unavailable))
        };
        let future: KeyFuture<Reply> = Box::pin(async move {
            let result = Abortable::new(resolve, registration)
                .await
                .unwrap_or_else(|_| Err(KeyError::new(KeyErrorKind::Cancelled)));
            let now = clock.now();
            let result = result.and_then(|key| {
                if key.is_valid_at(now) {
                    Ok(key)
                } else {
                    Err(KeyError::new(KeyErrorKind::Expired).for_reference(key.reference()))
                }
            });
            let Some(state) = weak.upgrade() else {
                return Err(KeyError::new(KeyErrorKind::Cancelled));
            };
            let mut s = state.lock().unwrap();
            if s.cancelled {
                return Err(KeyError::new(KeyErrorKind::Cancelled));
            }
            let Some(index) = s.pending.iter().position(|p| p.id == id) else {
                return Err(KeyError::new(KeyErrorKind::Cancelled));
            };
            let retired = s.pending.remove(index);
            if s.generation == generation
                && let Ok(key) = &result
            {
                if s.cache.len() == max_cached {
                    s.cache.remove(0);
                }
                s.cache.push(Cached {
                    identity: cache_identity,
                    key: key.clone(),
                });
            }
            drop(s);
            drop(retired);
            result
        });
        let future = future.shared();
        s.pending.push(Pending {
            id,
            identity,
            generation,
            future: future.clone(),
            abort,
            cancellation,
            waiters: 1,
            active_request,
        });
        s.waiters += 1;
        Ok(self.waiter(Some(id), future, resource))
    }
    fn waiter(&self, id: Option<u64>, future: RequestFuture, resource: KeyResource) -> KeyWaiter {
        KeyWaiter {
            id,
            future,
            resource,
            state: Arc::downgrade(&self.state),
            provider: self.provider.clone(),
            clock: self.clock.clone(),
            released: false,
        }
    }
    /// Invalidate this entire authorization scope. Existing waiters keep their immutable
    /// resolution; late results from them cannot populate the new generation's cache.
    pub fn invalidate(&self) -> Result<(), KeyError> {
        let mut s = self.state.lock().unwrap();
        if s.cancelled {
            return Err(KeyError::new(KeyErrorKind::Cancelled));
        }
        s.generation = s
            .generation
            .checked_add(1)
            .ok_or_else(|| KeyError::new(KeyErrorKind::RevisionOverflow))?;
        s.cache.clear();
        Ok(())
    }
    pub fn cancel(&self) {
        let pending = {
            let mut s = self.state.lock().unwrap();
            if s.cancelled {
                return;
            }
            s.cancelled = true;
            s.cache.clear();
            std::mem::take(&mut s.pending)
        };
        for p in pending {
            abandon(p, &*self.provider);
        }
    }
}
impl Drop for KeySession {
    fn drop(&mut self) {
        self.cancel();
    }
}
fn abandon(p: Pending, provider: &dyn KeyProvider) {
    p.cancellation.cancel();
    p.abort.abort();
    let request = p.active_request.lock().unwrap().take();
    if let Some(request) = request {
        provider.abort(&request);
    }
    // Drop the provider future outside every state/active-request lock.
}
/// One resource's wait. Drop/cancel only releases this resource; no Clone to bypass budgets.
pub struct KeyWaiter {
    id: Option<u64>,
    future: RequestFuture,
    resource: KeyResource,
    state: Weak<Mutex<State>>,
    provider: Arc<dyn KeyProvider>,
    clock: Arc<dyn KeyClock>,
    released: bool,
}
impl KeyWaiter {
    pub fn cancel(self) {
        drop(self);
    }
    fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        let pending = self.state.upgrade().and_then(|state| {
            let mut s = state.lock().unwrap();
            s.waiters -= 1;
            let index = s.pending.iter().position(|p| Some(p.id) == self.id)?;
            s.pending[index].waiters -= 1;
            if s.pending[index].waiters == 0 {
                Some(s.pending.remove(index))
            } else {
                None
            }
        });
        if let Some(p) = pending {
            abandon(p, &*self.provider);
        }
    }
}
impl Future for KeyWaiter {
    type Output = Reply;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Reply> {
        assert!(!self.released, "KeyWaiter polled after completion");
        let cancelled = self
            .state
            .upgrade()
            .is_none_or(|s| s.lock().unwrap().cancelled);
        let result = if cancelled {
            Poll::Ready(Err(KeyError::new(KeyErrorKind::Cancelled)))
        } else {
            self.future.poll_unpin(cx)
        };
        match result {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                let now = self.clock.now();
                // A reentrant provider/clock can cancel the operation while being polled.
                let cancelled = self
                    .state
                    .upgrade()
                    .is_none_or(|s| s.lock().unwrap().cancelled);
                let result = if cancelled {
                    Err(KeyError::new(KeyErrorKind::Cancelled))
                } else {
                    result.and_then(|key| {
                        if key.is_valid_at(now) {
                            Ok(key)
                        } else {
                            Err(KeyError::new(KeyErrorKind::Expired).for_reference(key.reference()))
                        }
                    })
                };
                self.release();
                Poll::Ready(result.map_err(|e| e.at(&self.resource)))
            }
        }
    }
}
impl Drop for KeyWaiter {
    fn drop(&mut self) {
        self.release();
    }
}
