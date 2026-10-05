use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use url::Url;

use crate::error::{Error, Result};

use std::collections::HashMap;

/// A byte sub-range within a larger resource, used for `#EXT-X-BYTERANGE`
/// segment reads and Range requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ByteRange {
    pub offset: u64,
    pub length: u64,
}

/// A resolvable input location: either a local filesystem path or a URL.
///
/// Exposed publicly because it is part of the [`Source`] trait contract and
/// the [`HlsInput::Custom`] variant. The transmuxer resolves relative URIs
/// found in playlists against the location of the playlist they came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceLocation {
    File(PathBuf),
    Url(Url),
}

impl SourceLocation {
    pub(crate) fn resolve(&self, uri: &str) -> Result<Self> {
        if uri.starts_with("http://") || uri.starts_with("https://") {
            return Ok(Self::Url(
                Url::parse(uri).map_err(|error| Error::invalid(error.to_string()))?,
            ));
        }

        match self {
            Self::File(path) => {
                let base = path.parent().unwrap_or_else(|| std::path::Path::new(""));
                Ok(Self::File(base.join(uri)))
            }
            Self::Url(url) => Ok(Self::Url(
                url.join(uri)
                    .map_err(|error| Error::invalid(error.to_string()))?,
            )),
        }
    }
}

/// A text resource (typically a playlist) returned by [`Source::read_text`].
#[derive(Debug, Clone)]
pub struct TextResource {
    pub content: String,
    pub location: SourceLocation,
}

/// Settings for an isolated operation-owned resource reader.
#[derive(Debug, Clone, Default)]
pub struct SourceSessionOptions {
    pub(crate) demand_driven: bool,
    pub(crate) max_resource_bytes: Option<u64>,
}
impl SourceSessionOptions {
    pub fn demand_driven(&self) -> bool {
        self.demand_driven
    }
    pub fn max_resource_bytes(&self) -> Option<u64> {
        self.max_resource_bytes
    }
}

/// Abstracts how the transmuxer reads playlists and segment bytes.
///
/// The crate ships a built-in reqwest-backed implementation (`ReqwestSource`,
/// available with the `default-source` cargo feature) used by default for
/// [`HlsInput::Path`] and [`HlsInput::Url`]. Callers that
/// want to plug in a different HTTP client, caching layer, proxy, retry policy,
/// or a fully offline source should implement this trait and pass it via
/// [`HlsInput::custom`].
///
/// The trait uses boxed futures (no `async-trait` dependency) so it is
/// object-safe and can be used as `Arc<dyn Source>`.
pub trait Source: Send + Sync + std::fmt::Debug {
    /// Creates an isolated session. Demand-driven sessions must not perform
    /// autonomous prefetch. Dropping read futures must stop visible effects.
    /// Implementations allocating whole resources must enforce the optional
    /// byte limit themselves during reads; the caller also checks the result.
    fn create_session_with_options(
        &self,
        _options: &SourceSessionOptions,
    ) -> Option<Arc<dyn Source>> {
        self.create_session()
    }

    /// Creates an isolated task session. Returning `None` uses this source
    /// directly. Custom sources owning background work should return a session
    /// whose Drop stops that work; sessions must not cancel other tasks.
    fn create_session(&self) -> Option<Arc<dyn Source>> {
        None
    }

    /// Stops background work for a task session. The pipeline calls this on
    /// cancellation and session teardown. Custom sources overriding this method
    /// should also implement create_session to avoid stopping unrelated tasks.
    fn stop_session(&self) {}

    /// Reads the full text content at `location` (typically a `.m3u8`
    /// playlist). Implementations should follow HTTP redirects and return the
    /// final resolved location in [`TextResource::location`].
    fn read_text<'a>(
        &'a self,
        location: &'a SourceLocation,
    ) -> Pin<Box<dyn Future<Output = Result<TextResource>> + Send + 'a>>;

    /// Reads raw bytes at `location`, optionally restricted to `range`. For
    /// HTTP sources, `range` maps to a `Range: bytes=start-end` request; the
    /// implementation must verify the server actually returned partial content
    /// (status 206) and reject short reads. For local files, `range` is a
    /// simple slice.
    fn read_bytes<'a>(
        &'a self,
        location: &'a SourceLocation,
        range: Option<&'a ByteRange>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>>> + Send + 'a>>;
}

/// Built-in `reqwest`-backed [`Source`] implementation. Available with the
/// `default-source` cargo feature (enabled by default).
///
/// Use [`ReqwestSource::new`] for a default client, or construct a custom
/// `reqwest::Client` (e.g. with proxies, custom TLS, retry policies) and pass
/// it to [`ReqwestSource::with_client`].
///
/// # Custom request headers (auth, cookies, etc.)
///
/// Use [`ReqwestSource::with_headers`] or
/// [`ReqwestSource::with_concurrency_and_headers`] to attach a
/// `reqwest::header::HeaderMap` to every outbound request (playlist `GET`s
/// and segment `GET`s, including Range requests). The headers are applied to
/// both the sequential path and the v3 concurrent prefetch workers. Typical
/// use cases: `Authorization: Bearer <token>`, `Cookie: ...`, `Origin: ...`,
/// custom CDN signing headers.
///
/// ```no_run
/// use std::sync::Arc;
/// use hls_transmux::{HlsInput, ReqwestSource, SourceLocation};
/// use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
///
/// let mut headers = HeaderMap::new();
/// headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer secret"));
/// let source = Arc::new(ReqwestSource::with_concurrency_and_headers(4, headers));
/// let location = SourceLocation::Url(
///     url::Url::parse("https://example.com/media.m3u8").unwrap()
/// );
/// // source now sends Authorization on both playlist and segment requests
/// let _input = HlsInput::custom(source, location);
/// ```
///
/// Note: if you also pass a custom `reqwest::Client` (via
/// [`ReqwestSource::with_client`] /
/// [`ReqwestSource::with_client_and_concurrency`]), prefer setting default
/// headers on the client itself via
/// `reqwest::ClientBuilder::default_headers` — the `headers` field on
/// `ReqwestSource` is for the no-custom-client case. The two layers
/// compose: client default headers + per-source `headers` are both applied
/// (per-source overrides client defaults for the same header name).
///
/// # Concurrent segment downloads
///
/// By default `ReqwestSource` downloads segments sequentially. To enable
/// bounded concurrent prefetch, use [`ReqwestSource::with_concurrency`] (or
/// [`ReqwestSource::with_client_and_concurrency`]) with `concurrency > 1`.
/// When enabled, the source detects media playlists returned by `read_text`
/// and spawns `concurrency` background workers that prefetch up to
/// `concurrency * 3` segments ahead of the transmuxer's sequential
/// consumption. Memory is bounded: at most `concurrency * 3` segment bodies
/// are held in flight or ready at any time (matching the backpressure of
/// `semaphore(N) in-flight + channel(2N) buffered = 3N`). When the
/// transmuxer races ahead of prefetch, the consumer self-builds a slot and
/// spawns a one-shot fetch (no redundant direct-fetch competing for
/// bandwidth). This is transparent to the transmuxer — it still calls
/// `read_bytes(url)` sequentially, but the bytes may already be cached.
///
/// Local file inputs and master playlists are never prefetched.
#[cfg(feature = "default-source")]
pub struct ReqwestSource {
    http: reqwest::Client,
    policy: HttpRequestPolicy,
    concurrency: usize,
    /// Headers applied to every outbound HTTP request (playlist + segment,
    /// sequential + concurrent). Cloned into `PrefetchState` on prefetch init
    /// so workers apply the same headers. `HeaderMap::clone` is cheap
    /// (internally `Arc`-shared until mutation).
    headers: reqwest::header::HeaderMap,
    /// Lazy prefetch state, initialized on the first `read_text` that returns
    /// a media playlist (when `concurrency > 1`). `OnceLock` is used because
    /// `read_text` may be called twice (master → variant) and only the variant
    /// (media) playlist should trigger prefetch.
    state: std::sync::OnceLock<std::sync::Arc<PrefetchState>>,
    /// Cancel signal held by `ReqwestSource` (NOT by `PrefetchState`) so it
    /// drops when `ReqwestSource` drops, even if workers still hold
    /// `Arc<PrefetchState>` references. Without this split, workers would
    /// hold the only `Arc<PrefetchState>` refs forever (deadlock) since
    /// `cancel_tx` would never drop.
    cancel_tx: std::sync::OnceLock<tokio::sync::watch::Sender<bool>>,
    tasks: std::sync::Mutex<Vec<tokio::task::AbortHandle>>,
}

/// Optional HTTP safeguards, shared by sequential reads and prefetch workers.
/// Defaults preserve the client's own timeout and perform no extra retries.
#[cfg(feature = "default-source")]
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct HttpRequestPolicy {
    pub request_timeout: Option<std::time::Duration>,
    /// Additional attempts after the initial request.
    pub max_retries: u32,
    pub backoff_base: std::time::Duration,
    pub backoff_max: std::time::Duration,
    /// Maximum response body bytes, including playlists and initialization.
    pub max_resource_bytes: Option<u64>,
}

#[cfg(feature = "default-source")]
impl Default for HttpRequestPolicy {
    fn default() -> Self {
        Self {
            request_timeout: None,
            max_retries: 0,
            backoff_base: std::time::Duration::from_millis(200),
            backoff_max: std::time::Duration::from_secs(5),
            max_resource_bytes: None,
        }
    }
}

#[cfg(feature = "default-source")]
impl std::fmt::Debug for ReqwestSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReqwestSource")
            .field("http", &self.http)
            .field("concurrency", &self.concurrency)
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "default-source")]
impl Default for ReqwestSource {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "default-source")]
impl Clone for ReqwestSource {
    fn clone(&self) -> Self {
        Self {
            http: self.http.clone(),
            policy: self.policy.clone(),
            concurrency: self.concurrency,
            headers: self.headers.clone(),
            // Each clone gets its own (lazily-initialized) prefetch state.
            // Clones are independent — they do not share prefetch caches.
            state: std::sync::OnceLock::new(),
            cancel_tx: std::sync::OnceLock::new(),
            tasks: std::sync::Mutex::new(Vec::new()),
        }
    }
}

#[cfg(feature = "default-source")]
impl ReqwestSource {
    /// Creates a new `ReqwestSource` with a default `reqwest::Client` and
    /// sequential (non-concurrent) downloads.
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::new(),
            policy: HttpRequestPolicy::default(),
            concurrency: 1,
            headers: reqwest::header::HeaderMap::new(),
            state: std::sync::OnceLock::new(),
            cancel_tx: std::sync::OnceLock::new(),
            tasks: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Creates a new `ReqwestSource` with a custom `reqwest::Client` and
    /// sequential (non-concurrent) downloads.
    pub fn with_client(http: reqwest::Client) -> Self {
        Self {
            http,
            policy: HttpRequestPolicy::default(),
            concurrency: 1,
            headers: reqwest::header::HeaderMap::new(),
            state: std::sync::OnceLock::new(),
            cancel_tx: std::sync::OnceLock::new(),
            tasks: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Creates a new `ReqwestSource` with a default `reqwest::Client` and
    /// concurrent segment prefetch enabled. `concurrency` is clamped to a
    /// minimum of 1; values ≤ 1 disable prefetch (equivalent to [`Self::new`]).
    pub fn with_concurrency(concurrency: usize) -> Self {
        Self {
            http: reqwest::Client::new(),
            policy: HttpRequestPolicy::default(),
            concurrency: concurrency.max(1),
            headers: reqwest::header::HeaderMap::new(),
            state: std::sync::OnceLock::new(),
            cancel_tx: std::sync::OnceLock::new(),
            tasks: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Creates a new `ReqwestSource` with a custom `reqwest::Client` and
    /// concurrent segment prefetch enabled. `concurrency` is clamped to a
    /// minimum of 1; values ≤ 1 disable prefetch (equivalent to
    /// [`Self::with_client`]).
    pub fn with_client_and_concurrency(http: reqwest::Client, concurrency: usize) -> Self {
        Self {
            http,
            policy: HttpRequestPolicy::default(),
            concurrency: concurrency.max(1),
            headers: reqwest::header::HeaderMap::new(),
            state: std::sync::OnceLock::new(),
            cancel_tx: std::sync::OnceLock::new(),
            tasks: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Creates a new `ReqwestSource` with a default `reqwest::Client`,
    /// sequential downloads, and the given `headers` attached to every
    /// outbound HTTP request (playlist `GET`s and segment `GET`s).
    ///
    /// Typical use: `Authorization: Bearer <token>`, `Cookie: ...`, custom
    /// CDN signing headers. See the type-level doc for an example.
    ///
    /// To combine a custom `reqwest::Client` with headers, build the client
    /// via `reqwest::ClientBuilder::default_headers(headers)` and pass it to
    /// [`Self::with_client`] / [`Self::with_client_and_concurrency`].
    pub fn with_headers(headers: reqwest::header::HeaderMap) -> Self {
        Self {
            http: reqwest::Client::new(),
            policy: HttpRequestPolicy::default(),
            concurrency: 1,
            headers,
            state: std::sync::OnceLock::new(),
            cancel_tx: std::sync::OnceLock::new(),
            tasks: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Creates a new `ReqwestSource` with a default `reqwest::Client`,
    /// concurrent segment prefetch enabled, and the given `headers` attached
    /// to every outbound HTTP request. `concurrency` is clamped to a minimum
    /// of 1; values ≤ 1 disable prefetch.
    ///
    /// Equivalent to [`Self::with_concurrency`] but with custom request
    /// headers. Headers are propagated to both the consumer-facing
    /// `read_text`/`read_bytes` and the v3 prefetch workers.
    pub fn with_concurrency_and_headers(
        concurrency: usize,
        headers: reqwest::header::HeaderMap,
    ) -> Self {
        Self {
            http: reqwest::Client::new(),
            policy: HttpRequestPolicy::default(),
            concurrency: concurrency.max(1),
            headers,
            state: std::sync::OnceLock::new(),
            cancel_tx: std::sync::OnceLock::new(),
            tasks: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn track_task(&self, task: tokio::task::JoinHandle<()>) {
        let mut tasks = self.tasks.lock().unwrap();
        tasks.retain(|task| !task.is_finished());
        if self.cancel_tx.get().is_some_and(|signal| *signal.borrow()) {
            task.abort();
        } else {
            tasks.push(task.abort_handle());
        }
    }

    /// Returns the configured concurrency level (1 = sequential).
    pub fn concurrency(&self) -> usize {
        self.concurrency
    }

    /// Applies optional timeout, retry and body-size safeguards to every HTTP read.
    pub fn with_request_policy(mut self, policy: HttpRequestPolicy) -> Self {
        self.stop_session();
        self.state.take();
        self.cancel_tx.take();
        self.policy = policy;
        self
    }

    /// Returns a reference to the headers applied to every outbound HTTP
    /// request. Empty by default unless set via [`Self::with_headers`] or
    /// [`Self::with_concurrency_and_headers`].
    pub fn headers(&self) -> &reqwest::header::HeaderMap {
        &self.headers
    }
}

#[cfg(feature = "default-source")]
impl Drop for ReqwestSource {
    fn drop(&mut self) {
        self.stop_session();
    }
}

#[cfg(feature = "default-source")]
impl Source for ReqwestSource {
    fn create_session_with_options(
        &self,
        options: &SourceSessionOptions,
    ) -> Option<Arc<dyn Source>> {
        let mut session = self.clone();
        if options.demand_driven {
            session.concurrency = 1;
        }
        if let Some(limit) = options.max_resource_bytes {
            session.policy.max_resource_bytes = Some(
                session
                    .policy
                    .max_resource_bytes
                    .map_or(limit, |old| old.min(limit)),
            );
        }
        Some(Arc::new(session))
    }

    fn create_session(&self) -> Option<Arc<dyn Source>> {
        Some(Arc::new(self.clone()))
    }

    fn stop_session(&self) {
        if let Some(signal) = self.cancel_tx.get() {
            signal.send_replace(true);
        }
        for task in self.tasks.lock().unwrap().drain(..) {
            task.abort();
        }
    }

    fn read_text<'a>(
        &'a self,
        location: &'a SourceLocation,
    ) -> Pin<Box<dyn Future<Output = Result<TextResource>> + Send + 'a>> {
        Box::pin(async move {
            let resource = match location {
                SourceLocation::File(path) => {
                    let content = tokio::fs::read_to_string(path).await?;
                    TextResource {
                        content,
                        location: location.clone(),
                    }
                }
                SourceLocation::Url(url) => {
                    let (bytes, final_url) =
                        fetch_resource(&self.http, &self.headers, url, None, &self.policy).await?;
                    let content = String::from_utf8(bytes)
                        .map_err(|_| Error::invalid("playlist is not UTF-8"))?;
                    TextResource {
                        content,
                        location: SourceLocation::Url(final_url),
                    }
                }
            };
            // Try to start prefetching if this is a media playlist and
            // concurrency is enabled. No-op for master playlists, local
            // files, or concurrency == 1.
            self.try_start_prefetch(&resource);
            Ok(resource)
        })
    }

    fn read_bytes<'a>(
        &'a self,
        location: &'a SourceLocation,
        range: Option<&'a ByteRange>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>>> + Send + 'a>> {
        Box::pin(async move {
            match location {
                SourceLocation::File(path) => {
                    if let Some(limit) = self.policy.max_resource_bytes {
                        read_limited_file(path, range, limit).await
                    } else {
                        let bytes = tokio::fs::read(path).await?;
                        apply_range(bytes, range)
                    }
                }
                SourceLocation::Url(url) => {
                    // Fast path: prefetch slot exists for this (url, range).
                    // Wait for it to become Ready, take the bytes, evict.
                    if let Some(state_ref) = self.state.get() {
                        let state = std::sync::Arc::clone(state_ref);
                        let key = (http_cache_url(url), range.copied());
                        // Try the cache first — MutexGuard is dropped before
                        // the await boundary (MutexGuard is !Send).
                        let cached_slot = { state.slots.lock().unwrap().get(&key).cloned() };
                        if let Some(slot) = cached_slot {
                            return read_from_slot(state, &key, &slot).await;
                        }
                        // Slow path: consumer raced ahead of prefetch (no
                        // slot exists yet). Use the Entry API to atomically
                        // either grab a worker-spawned InFlight slot (raced
                        // with a worker between our cache miss and the lock)
                        // or self-build one with `_buffer_permit = None` and
                        // spawn a one-shot fetch. This avoids redundant
                        // direct-fetch bandwidth competing with workers.
                        let slot = {
                            let mut slots = state.slots.lock().unwrap();
                            match slots.entry(key.clone()) {
                                std::collections::hash_map::Entry::Occupied(e) => {
                                    // A worker inserted between our cache miss
                                    // and this lock acquisition — use its slot.
                                    e.get().clone()
                                }
                                std::collections::hash_map::Entry::Vacant(v) => {
                                    // Self-build a slot. Permit is None so it
                                    // doesn't count against `buffer_sem`.
                                    let slot = std::sync::Arc::new(Slot {
                                        state: tokio::sync::Mutex::new(SlotState::InFlight),
                                        notify: tokio::sync::Notify::new(),
                                        _buffer_permit: None,
                                    });
                                    v.insert(std::sync::Arc::clone(&slot));
                                    // Spawn one-shot fetch using the cloned
                                    // http client + headers — `state` is not
                                    // captured (avoid holding an Arc into the
                                    // one-shot task).
                                    let http = state.http.clone();
                                    let headers = state.headers.clone();
                                    let policy = state.policy.clone();
                                    let fetch_url = url.clone();
                                    let fetch_range = range.copied();
                                    let fetch_slot = std::sync::Arc::clone(&slot);
                                    let task = tokio::spawn(async move {
                                        let result = fetch_bytes_with_range(
                                            &http,
                                            &headers,
                                            &fetch_url,
                                            fetch_range.as_ref(),
                                            &policy,
                                        )
                                        .await;
                                        let mut s = fetch_slot.state.lock().await;
                                        match result {
                                            Ok(bytes) => {
                                                *s = SlotState::Ready(std::sync::Arc::new(bytes));
                                            }
                                            Err(e) => {
                                                *s = SlotState::Failed(e.to_string());
                                            }
                                        }
                                        drop(s);
                                        fetch_slot.notify.notify_waiters();
                                    });
                                    self.track_task(task);
                                    slot
                                }
                            }
                        };
                        return read_from_slot(state, &key, &slot).await;
                    }
                    // Fall through: init segment, URL not in prefetch list,
                    // or concurrency == 1 (state never initialized).
                    fetch_bytes_with_range(&self.http, &self.headers, url, range, &self.policy)
                        .await
                }
            }
        })
    }
}

/// Input source for the async transmux entry point.
///
/// `Path` and `Url` use the built-in `ReqwestSource` (when the
/// `default-source` feature is enabled). Callers that want to provide their
/// own HTTP client / cache / proxy / offline source should use
/// [`HlsInput::custom`] (or the [`HlsInput::Custom`] variant directly).
#[derive(Debug, Clone)]
pub enum HlsInput {
    /// A local filesystem path to a `.m3u8` playlist.
    Path(PathBuf),
    /// An HTTP/HTTPS URL pointing at a playlist.
    Url(String),
    /// Custom [`Source`] with an explicit starting [`SourceLocation`]. The
    /// source is responsible for resolving both the root location and any
    /// relative URIs the transmuxer derives from it.
    Custom(Arc<dyn Source>, SourceLocation),
}

impl HlsInput {
    /// Builds a [`HlsInput::Custom`] from a source implementation and a
    /// starting location.
    pub fn custom(source: Arc<dyn Source>, location: SourceLocation) -> Self {
        Self::Custom(source, location)
    }

    /// Splits the input into a starting location and a [`Source`] instance.
    pub(crate) fn into_parts(self) -> Result<(SourceLocation, Arc<dyn Source>)> {
        match self {
            Self::Path(path) => {
                #[cfg(feature = "default-source")]
                {
                    Ok((SourceLocation::File(path), Arc::new(ReqwestSource::new())))
                }
                #[cfg(not(feature = "default-source"))]
                {
                    let _ = path;
                    Err(Error::unsupported(
                        "HlsInput::Path requires the `default-source` cargo feature; \
                         enable it or use HlsInput::custom with your own Source impl",
                    ))
                }
            }
            Self::Url(url) => {
                #[cfg(feature = "default-source")]
                {
                    let location = SourceLocation::Url(
                        Url::parse(&url).map_err(|error| Error::invalid(error.to_string()))?,
                    );
                    Ok((location, Arc::new(ReqwestSource::new())))
                }
                #[cfg(not(feature = "default-source"))]
                {
                    let _ = url;
                    Err(Error::unsupported(
                        "HlsInput::Url requires the `default-source` cargo feature; \
                         enable it or use HlsInput::custom with your own Source impl",
                    ))
                }
            }
            Self::Custom(source, location) => Ok((location, source)),
        }
    }
}

/// Internal adapter that wraps an [`Arc<dyn Source>`] so the rest of the
/// transmux pipeline can keep using a small, owned `&SourceReader` handle.
#[derive(Debug, Clone)]
pub(crate) struct SourceReader {
    source: Arc<dyn Source>,
    cancel: Option<Arc<dyn crate::CancelToken>>,
    _session: Arc<SessionGuard>,
}

#[derive(Debug)]
struct SessionGuard {
    source: Arc<dyn Source>,
    monitor: Option<tokio::task::AbortHandle>,
}
impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.source.stop_session();
        if let Some(monitor) = &self.monitor {
            monitor.abort();
        }
    }
}

impl SourceReader {
    pub(crate) fn new(
        source: Arc<dyn Source>,
        cancel: Option<Arc<dyn crate::CancelToken>>,
    ) -> Self {
        let source = source.create_session().unwrap_or(source);
        let monitor = cancel.as_ref().map(|token| {
            let token = token.clone();
            let session = source.clone();
            tokio::spawn(async move {
                token.cancelled().await;
                session.stop_session();
            })
            .abort_handle()
        });
        let guard = Arc::new(SessionGuard {
            source: source.clone(),
            monitor,
        });
        Self {
            source,
            cancel,
            _session: guard,
        }
    }

    pub(crate) async fn read_text(&self, location: &SourceLocation) -> Result<TextResource> {
        crate::cancel::wait(self.cancel.as_ref(), async {
            self.source.read_text(location).await
        })
        .await
        .map_err(|error| error.context(format!("playlist resource {}", safe_location(location))))
    }

    pub(crate) async fn read_bytes(
        &self,
        location: &SourceLocation,
        range: Option<&ByteRange>,
    ) -> Result<Vec<u8>> {
        crate::cancel::wait(self.cancel.as_ref(), async {
            self.source.read_bytes(location, range).await
        })
        .await
    }
}

#[cfg(feature = "default-source")]
fn apply_range(bytes: Vec<u8>, range: Option<&ByteRange>) -> Result<Vec<u8>> {
    let Some(range) = range else {
        return Ok(bytes);
    };
    let start = usize::try_from(range.offset)
        .map_err(|_| Error::invalid("byterange offset exceeds usize"))?;
    let length = usize::try_from(range.length)
        .map_err(|_| Error::invalid("byterange length exceeds usize"))?;
    let end = start
        .checked_add(length)
        .ok_or_else(|| Error::invalid("byterange overflows usize"))?;
    if end > bytes.len() {
        return Err(Error::invalid(
            "byterange extends past the end of the segment",
        ));
    }
    Ok(bytes[start..end].to_vec())
}

// ---------------------------------------------------------------------------
// MemorySource: in-memory Source for WASM / browser contexts.
// ---------------------------------------------------------------------------

/// In-memory [`Source`] implementation for environments where the caller has
/// pre-fetched all playlist text and segment bytes (e.g. a browser that
/// downloaded everything via `fetch()` before invoking the WASM transmuxer).
///
/// Keys are absolute URL strings (or filesystem path strings). The
/// transmuxer resolves relative URIs found in playlists against the
/// playlist's [`SourceLocation`], so the keys must match the resolved
/// absolute URLs/paths.
///
/// `#EXT-X-BYTERANGE` is supported: when [`Source::read_bytes`] is called
/// with a [`ByteRange`], the full segment bytes are looked up by URL and then
/// sliced.
///
/// # Example (WASM / no-default-features)
///
/// ```no_run
/// use std::collections::HashMap;
/// use std::sync::Arc;
/// use hls_transmux::{HlsInput, MemorySource, OutputFormat, SourceLocation,
///     TransmuxOptions, transmux_hls_to_writer_async};
///
/// # async fn run() -> hls_transmux::Result<()> {
/// let mut texts = HashMap::new();
/// texts.insert(
///     "https://example.com/media.m3u8".to_string(),
///     "#EXTM3U\n#EXT-X-TARGETDURATION:8\n#EXTINF:7.0,\nseg0.ts\n#EXT-X-ENDLIST\n".to_string(),
/// );
/// let mut bytes = HashMap::new();
/// bytes.insert(
///     "https://example.com/seg0.ts".to_string(),
///     std::fs::read("seg0.ts").unwrap(),
/// );
/// let source = MemorySource::with_data(texts, bytes);
/// let location = SourceLocation::Url(
///     url::Url::parse("https://example.com/media.m3u8").unwrap(),
/// );
/// let input = HlsInput::custom(Arc::new(source), location);
/// let mut buf: Vec<u8> = Vec::new();
/// transmux_hls_to_writer_async(input, &mut buf, TransmuxOptions {
///     output_format: OutputFormat::FragmentedMp4,
///     ..Default::default()
/// }).await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Default, Clone)]
pub struct MemorySource {
    texts: Arc<HashMap<String, String>>,
    bytes: Arc<HashMap<String, Vec<u8>>>,
    max_resource_bytes: Option<u64>,
}

impl MemorySource {
    /// Creates an empty `MemorySource`. Use [`Self::text`] / [`Self::segment`]
    /// (builder style) or [`Self::with_data`] to populate.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a `MemorySource` from pre-fetched playlist texts and segment
    /// bytes. Both maps are keyed by absolute URL string (or filesystem path
    /// string for `SourceLocation::File`).
    pub fn with_data(texts: HashMap<String, String>, bytes: HashMap<String, Vec<u8>>) -> Self {
        Self {
            texts: Arc::new(texts),
            bytes: Arc::new(bytes),
            max_resource_bytes: None,
        }
    }

    /// Adds a playlist text keyed by its absolute URL (builder style).
    pub fn text(mut self, url: impl Into<String>, content: impl Into<String>) -> Self {
        Arc::make_mut(&mut self.texts).insert(url.into(), content.into());
        self
    }

    /// Adds segment bytes keyed by the segment's absolute URL (builder style).
    pub fn segment(mut self, url: impl Into<String>, data: impl Into<Vec<u8>>) -> Self {
        Arc::make_mut(&mut self.bytes).insert(url.into(), data.into());
        self
    }
}

impl Source for MemorySource {
    fn create_session_with_options(
        &self,
        options: &SourceSessionOptions,
    ) -> Option<Arc<dyn Source>> {
        let mut session = self.clone();
        session.max_resource_bytes = options.max_resource_bytes;
        Some(Arc::new(session))
    }

    fn read_text<'a>(
        &'a self,
        location: &'a SourceLocation,
    ) -> Pin<Box<dyn Future<Output = Result<TextResource>> + Send + 'a>> {
        Box::pin(async move {
            let key = location_key(location);
            let content = self
                .texts
                .get(&key)
                .ok_or_else(|| Error::invalid(format!("MemorySource: no text found for {key}")))?;
            Ok(TextResource {
                content: content.clone(),
                location: location.clone(),
            })
        })
    }

    fn read_bytes<'a>(
        &'a self,
        location: &'a SourceLocation,
        range: Option<&'a ByteRange>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>>> + Send + 'a>> {
        Box::pin(async move {
            let key = location_key(location);
            let bytes = self
                .bytes
                .get(&key)
                .ok_or_else(|| Error::invalid(format!("MemorySource: no bytes found for {key}")))?;
            let data = if let Some(range) = range {
                let end = range
                    .offset
                    .checked_add(range.length)
                    .ok_or_else(|| Error::invalid("byterange overflow"))?;
                let start = usize::try_from(range.offset)
                    .map_err(|_| Error::invalid("byterange exceeds address space"))?;
                let end = usize::try_from(end)
                    .map_err(|_| Error::invalid("byterange exceeds address space"))?;
                bytes
                    .get(start..end)
                    .ok_or_else(|| Error::invalid("byterange extends past resource"))?
            } else {
                bytes.as_slice()
            };
            if self
                .max_resource_bytes
                .is_some_and(|limit| data.len() as u64 > limit)
            {
                return Err(resource_limit_error());
            }
            Ok(data.to_vec())
        })
    }
}

/// Produces the lookup key for a `SourceLocation`. For URLs, the canonical
/// string form; for files, the path as a string.
fn location_key(location: &SourceLocation) -> String {
    match location {
        SourceLocation::Url(url) => url.to_string(),
        SourceLocation::File(path) => path.to_string_lossy().into_owned(),
    }
}

// ---------------------------------------------------------------------------
// Concurrent download infrastructure (only compiled with `default-source`).
//
// Design: "worker + buffer_sem(3N) + consumer self-built slot" — N workers
// pop targets from a shared queue and fetch concurrently; a
// `Semaphore(concurrency * 3)` bounds total outstanding slots (InFlight +
// Ready-unconsumed) to 3N, matching v1 `semaphore(N) + channel(2N) = 3N`
// backpressure. Each worker stores its `OwnedSemaphorePermit` inside the
// slot it creates; the permit is released when the consumer drops the slot
// after reading.
//
// When the consumer (transmuxer) races ahead of prefetch — i.e. calls
// `read_bytes` for a target no worker has reached yet — it self-builds a
// slot with `_buffer_permit = None` and spawns a one-shot fetch. Workers
// popping that target later see an existing slot and skip it (releasing
// their freshly-acquired buffer_permit). This mirrors v1's "consumer waits
// on `rx.recv()` when channel is empty" semantics and avoids redundant
// downloads competing for bandwidth.
//
// Why not BTreeMap by index? The Source trait API is URL-based, not
// index-based. The transmuxer already consumes segments in playlist order,
// so the Source only needs point lookups by URL — no ordered reassembly.
// ---------------------------------------------------------------------------

#[cfg(feature = "default-source")]
fn http_cache_url(url: &Url) -> Url {
    let mut url = url.clone();
    url.set_fragment(None);
    url
}

#[cfg(feature = "default-source")]
type SlotKey = (Url, Option<ByteRange>);

#[cfg(feature = "default-source")]
struct PrefetchState {
    /// URL+range → slot. `std::sync::Mutex` because critical sections are
    /// tiny (insert / lookup / evict) and never await.
    slots: std::sync::Mutex<HashMap<SlotKey, std::sync::Arc<Slot>>>,
    /// Ordered queue of (Url, Option<ByteRange>) targets to prefetch.
    /// Workers pop from the front; consumers never touch this except for
    /// the consumer-self-built-slot fast path, which uses `slots` not
    /// `targets`.
    targets: std::sync::Mutex<std::collections::VecDeque<SlotKey>>,
    /// Shared HTTP client (clone is cheap — internally `Arc`).
    http: reqwest::Client,
    policy: HttpRequestPolicy,
    /// Headers applied to every worker fetch (cloned from `ReqwestSource`).
    /// Workers read this without mutation, so no `Mutex` needed. The
    /// consumer-self-built-slot path also clones this for its one-shot
    /// fetch task.
    headers: reqwest::header::HeaderMap,
    /// Buffer semaphore: bounds total outstanding slots (InFlight +
    /// Ready-unconsumed) to `concurrency * 3`. Workers acquire_owned() a
    /// permit before fetching; the permit is stored in the slot and
    /// released when the consumer drops the slot. This matches v1's
    /// `semaphore(N) in-flight + channel(2N) buffered = 3N` backpressure.
    buffer_sem: std::sync::Arc<tokio::sync::Semaphore>,
}

#[cfg(feature = "default-source")]
struct Slot {
    /// `tokio::sync::Mutex` because the consumer awaits state transitions.
    state: tokio::sync::Mutex<SlotState>,
    /// Efficient zero-poll wakeup for consumers waiting on InFlight → Ready.
    notify: tokio::sync::Notify,
    /// Worker-created slots hold `Some(permit)` so the buffer semaphore
    /// releases when the consumer drops the slot. Consumer-self-built
    /// slots hold `None` — they don't count against `buffer_sem` (at most
    /// one such slot exists per pending target).
    _buffer_permit: Option<tokio::sync::OwnedSemaphorePermit>,
}

#[cfg(feature = "default-source")]
enum SlotState {
    InFlight,
    Ready(std::sync::Arc<Vec<u8>>),
    Failed(String),
}

#[cfg(feature = "default-source")]
impl ReqwestSource {
    /// Tries to start prefetching after `read_text` returns a text resource.
    /// No-op unless: concurrency > 1, the location is a URL, the content
    /// parses as a media playlist with at least one segment.
    fn try_start_prefetch(&self, resource: &TextResource) {
        if self.concurrency <= 1 || self.state.get().is_some() {
            return;
        }
        let SourceLocation::Url(playlist_url) = &resource.location else {
            return; // Don't prefetch local files.
        };

        // Parse the playlist content to extract segment URIs. Only media
        // playlists trigger prefetch — master playlists are skipped because
        // the variant hasn't been selected yet.
        let Ok(crate::hls::HlsPlaylist::Media(media)) =
            crate::hls::parse_hls_playlist_content(None, &resource.content)
        else {
            return;
        };
        if media.segments.is_empty() {
            return;
        }

        // Resolve each segment URI against the playlist's final URL. This
        // produces the exact `Url` values the transmuxer will later request
        // via `read_bytes`, so the slot keys will match.
        let targets: std::collections::VecDeque<SlotKey> = media
            .segments
            .iter()
            .filter_map(|seg| {
                let seg_url = playlist_url.join(&seg.uri).ok()?;
                Some((http_cache_url(&seg_url), seg.byte_range))
            })
            .collect();
        if targets.is_empty() {
            return;
        }

        // Lazy-init the prefetch state. `get_or_init` only runs once; if
        // `read_text` was called before (e.g. master playlist), the earlier
        // call would have returned early and left `state` uninitialized.
        //
        // `cancel_tx` is held by `self` (NOT by `PrefetchState`) so that
        // dropping `ReqwestSource` drops the Sender — workers see
        // `cancel_rx.changed()` return Err and exit. If `cancel_tx` lived
        // inside `PrefetchState`, workers would hold the only `Arc` refs
        // forever (deadlock: PrefetchState won't drop until workers exit,
        // but workers won't exit until cancel_tx drops).
        let cancel_tx = self
            .cancel_tx
            .get_or_init(|| tokio::sync::watch::channel(false).0);
        let cancel_rx = cancel_tx.subscribe();
        let state = self.state.get_or_init(|| {
            std::sync::Arc::new(PrefetchState {
                slots: std::sync::Mutex::new(HashMap::new()),
                targets: std::sync::Mutex::new(targets),
                http: self.http.clone(),
                policy: self.policy.clone(),
                headers: self.headers.clone(),
                buffer_sem: std::sync::Arc::new(tokio::sync::Semaphore::new(self.concurrency * 3)),
            })
        });

        // Spawn N workers. Each worker loops independently: pop target
        // → acquire buffer_permit → fetch → store slot → continue.
        // Workers stop when targets is empty OR cancel_tx is dropped
        // (i.e. ReqwestSource is dropped).
        for _ in 0..self.concurrency {
            let task = tokio::spawn(prefetch_worker(
                std::sync::Arc::clone(state),
                cancel_rx.clone(),
            ));
            self.track_task(task);
        }
    }
}

/// Worker: pops targets in order, acquires a buffer permit (backpressure),
/// fetches the segment, stores the result in a slot. Runs concurrently
/// with other workers and with consumers. Exits when the target queue is
/// empty or the cancel signal fires.
#[cfg(feature = "default-source")]
async fn prefetch_worker(
    state: std::sync::Arc<PrefetchState>,
    mut cancel_rx: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        // Fast cancel check: if cancel_tx was dropped (returns Err) or
        // the flag flipped to true, stop the worker.
        if *cancel_rx.borrow() {
            return;
        }
        // Pop the next target. Empty queue → prefetch complete → exit.
        let target = state.targets.lock().unwrap().pop_front();
        let Some((url, range)) = target else {
            return;
        };

        // Acquire a buffer permit before fetching. This blocks when total
        // outstanding slots reach `concurrency * 3`, providing backpressure
        // until the consumer frees a slot. Race against cancel so we don't
        // hang forever if the source is dropped while waiting.
        let buffer_permit = tokio::select! {
            biased;
            _ = cancel_rx.changed() => return,
            permit = state.buffer_sem.clone().acquire_owned() => {
                match permit {
                    Ok(p) => p,
                    Err(_) => return, // Semaphore closed (shouldn't happen)
                }
            }
        };

        // Race: if the consumer already self-built a slot for this target
        // (it caught up to prefetch front), skip — drop the buffer_permit
        // to release the semaphore slot. Use Entry API for atomic insert.
        let slot = {
            let mut slots = state.slots.lock().unwrap();
            match slots.entry((url.clone(), range)) {
                std::collections::hash_map::Entry::Occupied(_) => {
                    // Consumer got here first. Drop buffer_permit and
                    // continue to next target.
                    drop(buffer_permit);
                    continue;
                }
                std::collections::hash_map::Entry::Vacant(v) => {
                    let slot = std::sync::Arc::new(Slot {
                        state: tokio::sync::Mutex::new(SlotState::InFlight),
                        notify: tokio::sync::Notify::new(),
                        _buffer_permit: Some(buffer_permit),
                    });
                    v.insert(std::sync::Arc::clone(&slot));
                    slot
                }
            }
        };

        // Fetch the bytes. Don't hold any locks across the await.
        let http = state.http.clone();
        let headers = state.headers.clone();
        let result = tokio::select! {
            biased;
            _ = cancel_rx.changed() => return,
            result = fetch_bytes_with_range(&http, &headers, &url, range.as_ref(), &state.policy) => result,
        };

        // Store the result and wake any waiting consumers.
        let mut s = slot.state.lock().await;
        match result {
            Ok(bytes) => *s = SlotState::Ready(std::sync::Arc::new(bytes)),
            Err(e) => *s = SlotState::Failed(e.to_string()),
        }
        drop(s);
        slot.notify.notify_waiters();
        // Loop back to pop the next target — don't wait for the consumer!
    }
}

/// Consumer side: wait for the slot to become Ready (or Failed), extract
/// the bytes, and evict the slot. Eviction drops the `Arc<Slot>`; if this
/// is the last strong reference, the slot's `_buffer_permit` is dropped,
/// releasing the buffer semaphore and unblocking a waiting worker.
#[cfg(feature = "default-source")]
async fn read_from_slot(
    state: std::sync::Arc<PrefetchState>,
    key: &SlotKey,
    slot: &std::sync::Arc<Slot>,
) -> Result<Vec<u8>> {
    read_from_slot_with_hook(state, key, slot, || async {}).await
}

#[cfg(feature = "default-source")]
async fn read_from_slot_with_hook<F, Fut>(
    state: Arc<PrefetchState>,
    key: &SlotKey,
    slot: &Arc<Slot>,
    mut before_wait: F,
) -> Result<Vec<u8>>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = ()>,
{
    loop {
        let notified = slot.notify.notified();
        let mut s = slot.state.lock().await;
        match &mut *s {
            SlotState::InFlight => {
                // Drop the lock before awaiting, so the fetch task can
                // acquire it to store the result.
                drop(s);
                before_wait().await;
                notified.await;
                continue;
            }
            SlotState::Ready(bytes) => {
                state.slots.lock().unwrap().remove(key);
                // Other consumers already holding this slot must still see the data.
                let bytes = if Arc::strong_count(slot) == 1 {
                    Arc::unwrap_or_clone(std::mem::replace(bytes, Arc::new(Vec::new())))
                } else {
                    (**bytes).clone()
                };
                drop(s);
                // Evict the slot. The Arc<Slot> returned by `remove` will
                // drop at end of scope, releasing the buffer_permit (if
                // any) and freeing memory.
                state.slots.lock().unwrap().remove(key);
                return Ok(bytes);
            }
            SlotState::Failed(msg) => {
                let msg = msg.clone();
                drop(s);
                // Evict even on failure — the worker consumed a permit but
                // didn't produce consumable bytes. Releasing the permit
                // lets another worker fetch the next target.
                state.slots.lock().unwrap().remove(key);
                return Err(Error::Http(msg));
            }
        }
    }
}

/// Fetches bytes from `url` with an optional Range request, applying the
/// given `headers` to the outbound request. Extracted from the original
/// `read_bytes` URL branch so it can be shared by the sequential path
/// (`read_bytes` fallthrough), the v3 prefetch workers, and the consumer
/// self-built-slot one-shot fetch.
///
/// Header precedence: caller-supplied `headers` are applied first; if the
/// caller also passed a `range`, the `Range` header is set explicitly
/// afterwards (via `RequestBuilder::header`, which uses `HeaderMap::insert`
/// and replaces any `Range` set by the caller). Callers should not set
/// `Range` in `headers` — let the `range` parameter drive it.
#[cfg(feature = "default-source")]
async fn fetch_bytes_with_range(
    http: &reqwest::Client,
    headers: &reqwest::header::HeaderMap,
    url: &Url,
    range: Option<&ByteRange>,
    policy: &HttpRequestPolicy,
) -> Result<Vec<u8>> {
    fetch_resource(http, headers, url, range, policy)
        .await
        .map(|(bytes, _)| bytes)
}

/// A URL safe to include in diagnostics. Never expose credentials or signatures.
pub(crate) fn safe_location(location: &SourceLocation) -> String {
    match location {
        SourceLocation::File(path) => path.display().to_string(),
        SourceLocation::Url(url) => {
            let mut clean = url.clone();
            let _ = clean.set_username("");
            let _ = clean.set_password(None);
            clean.set_query(None);
            clean.set_fragment(None);
            clean.to_string()
        }
    }
}

#[cfg(feature = "default-source")]
fn validate_content_range(value: &str, range: &ByteRange) -> Result<()> {
    let invalid = || Error::Http("invalid Content-Range for requested interval".into());
    let value = value.strip_prefix("bytes ").ok_or_else(invalid)?;
    let (interval, total) = value.split_once('/').ok_or_else(invalid)?;
    let (start, end) = interval.split_once('-').ok_or_else(invalid)?;
    let start = start.parse::<u64>().map_err(|_| invalid())?;
    let end = end.parse::<u64>().map_err(|_| invalid())?;
    let expected = range
        .offset
        .checked_add(range.length)
        .and_then(|v| v.checked_sub(1))
        .ok_or_else(invalid)?;
    if range.length == 0 || start != range.offset || end != expected {
        return Err(invalid());
    }
    if total != "*" && total.parse::<u64>().map_err(|_| invalid())? <= end {
        return Err(invalid());
    }
    Ok(())
}

#[cfg(feature = "default-source")]
async fn fetch_resource(
    http: &reqwest::Client,
    headers: &reqwest::header::HeaderMap,
    url: &Url,
    range: Option<&ByteRange>,
    policy: &HttpRequestPolicy,
) -> Result<(Vec<u8>, Url)> {
    use reqwest::header::{CONTENT_RANGE, RANGE};
    let resource = safe_location(&SourceLocation::Url(url.clone()));
    let transport = |e: reqwest::Error| {
        let retryable = e.is_connect() || e.is_timeout();
        (Error::Http(e.without_url().to_string()), retryable)
    };
    let end = range
        .map(|r| {
            if r.length == 0 {
                return Err(Error::invalid("zero length HTTP range"));
            }
            r.offset
                .checked_add(r.length)
                .and_then(|v| v.checked_sub(1))
                .ok_or_else(|| Error::invalid("HTTP range overflow"))
        })
        .transpose()?;
    let mut attempt = 0u32;
    loop {
        let fetch = async {
            let mut request = http.get(url.clone()).headers(headers.clone());
            if let Some(end) = end {
                request = request.header(RANGE, format!("bytes={}-{end}", range.unwrap().offset));
            }
            let mut response = request.send().await.map_err(transport)?;
            let status = response.status();
            if !status.is_success() {
                let retryable = matches!(status.as_u16(), 408 | 429 | 500 | 502 | 503 | 504);
                return Err((
                    Error::Http(format!("GET returned status {status}")),
                    retryable,
                ));
            }
            if let Some(range) = range {
                if status != reqwest::StatusCode::PARTIAL_CONTENT {
                    return Err((
                        Error::Http("Range request requires status 206".into()),
                        false,
                    ));
                }
                let value = response
                    .headers()
                    .get(CONTENT_RANGE)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| (Error::Http("missing Content-Range".into()), false))?;
                validate_content_range(value, range).map_err(|e| (e, false))?;
            }
            let limit = match (policy.max_resource_bytes, range.map(|r| r.length)) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            if response
                .content_length()
                .is_some_and(|n| limit.is_some_and(|max| n > max))
            {
                return Err((
                    Error::Http("response exceeds resource size limit".into()),
                    false,
                ));
            }
            let final_url = response.url().clone();
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(transport)? {
                let next = (bytes.len() as u64)
                    .checked_add(chunk.len() as u64)
                    .ok_or_else(|| (Error::Http("response size overflow".into()), false))?;
                if limit.is_some_and(|max| next > max) {
                    return Err((
                        Error::Http("response exceeds resource size limit".into()),
                        false,
                    ));
                }
                bytes
                    .try_reserve(chunk.len())
                    .map_err(|_| (Error::Http("response allocation failed".into()), false))?;
                bytes.extend_from_slice(&chunk);
            }
            if range.is_some_and(|r| bytes.len() as u64 != r.length) {
                return Err((
                    Error::Http("Range response length does not match requested interval".into()),
                    false,
                ));
            }
            Ok((bytes, final_url))
        };
        let result = if let Some(timeout) = policy.request_timeout {
            tokio::time::timeout(timeout, fetch)
                .await
                .unwrap_or_else(|_| Err((Error::Http("request timed out".into()), true)))
        } else {
            fetch.await
        };
        match result {
            Ok(value) => return Ok(value),
            Err((error, retryable)) if retryable && attempt < policy.max_retries => {
                let _ = error;
                let delay = policy
                    .backoff_base
                    .saturating_mul(1u32.checked_shl(attempt).unwrap_or(u32::MAX))
                    .min(policy.backoff_max);
                attempt += 1;
                tokio::time::sleep(delay).await;
            }
            Err((error, _)) => return Err(Error::Http(format!("resource {resource}: {error}"))),
        }
    }
}

// Preserve v0.5 budget/HTTP variants while classifying them for the additive resource API.
fn resource_limit_error() -> Error {
    Error::invalid("resource exceeds configured byte limit")
}
pub(crate) fn is_resource_limit(error: &Error) -> bool {
    match error {
        Error::InvalidInput(message) => message == "resource exceeds configured byte limit",
        Error::Http(message) => {
            message == "response exceeds resource size limit"
                || message.ends_with(": HTTP error: response exceeds resource size limit")
        }
        _ => false,
    }
}
#[cfg(feature = "default-source")]
async fn read_limited_file(
    path: &std::path::Path,
    range: Option<&ByteRange>,
    limit: u64,
) -> Result<Vec<u8>> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let mut file = tokio::fs::File::open(path).await?;
    let length = file.metadata().await?.len();
    let expected = if let Some(range) = range {
        let end = range
            .offset
            .checked_add(range.length)
            .ok_or_else(|| Error::invalid("file range overflow"))?;
        if end > length {
            return Err(Error::invalid("file range exceeds resource"));
        }
        file.seek(std::io::SeekFrom::Start(range.offset)).await?;
        range.length
    } else {
        length
    };
    if expected > limit {
        return Err(resource_limit_error());
    }
    let mut bytes = Vec::new();
    // For full files, also catch growth after metadata; range reads stop at the requested end.
    let take = range.map_or(limit.saturating_add(1), |r| r.length);
    file.take(take).read_to_end(&mut bytes).await?;
    if bytes.len() as u64 > limit {
        return Err(resource_limit_error());
    }
    if range.is_some_and(|r| bytes.len() as u64 != r.length) {
        return Err(Error::invalid("short file range"));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "default-source")]
    use std::task::Poll;

    #[test]
    fn resolves_file_relative_paths() {
        let base = SourceLocation::File(PathBuf::from("/tmp/hls/master.m3u8"));
        assert_eq!(
            base.resolve("media/playlist.m3u8").unwrap(),
            SourceLocation::File(PathBuf::from("/tmp/hls/media/playlist.m3u8"))
        );
    }

    #[test]
    fn resolves_url_relative_paths() {
        let base = SourceLocation::Url(Url::parse("https://example.test/hls/master.m3u8").unwrap());
        assert_eq!(
            base.resolve("../media/playlist.m3u8").unwrap(),
            SourceLocation::Url(Url::parse("https://example.test/media/playlist.m3u8").unwrap())
        );
    }

    #[test]
    #[cfg(feature = "default-source")]
    fn applies_local_byterange() {
        let bytes = apply_range(
            b"0123456789".to_vec(),
            Some(&ByteRange {
                offset: 3,
                length: 4,
            }),
        )
        .unwrap();
        assert_eq!(bytes, b"3456");
    }
    #[cfg(feature = "default-source")]
    #[tokio::test]
    async fn completion_between_state_check_and_wait_never_loses_notification() {
        for worker_slot in [false, true] {
            for failure in [false, true] {
                let state = Arc::new(PrefetchState {
                    slots: std::sync::Mutex::new(HashMap::new()),
                    targets: std::sync::Mutex::new(Default::default()),
                    http: reqwest::Client::new(),
                    policy: HttpRequestPolicy::default(),
                    headers: Default::default(),
                    buffer_sem: Arc::new(tokio::sync::Semaphore::new(1)),
                });
                let permit = if worker_slot {
                    Some(state.buffer_sem.clone().acquire_owned().await.unwrap())
                } else {
                    None
                };
                let slot = Arc::new(Slot {
                    state: tokio::sync::Mutex::new(SlotState::InFlight),
                    notify: tokio::sync::Notify::new(),
                    _buffer_permit: permit,
                });
                let key = (Url::parse("https://example.test/segment.ts").unwrap(), None);
                state
                    .slots
                    .lock()
                    .unwrap()
                    .insert(key.clone(), slot.clone());
                let checked = Arc::new(tokio::sync::Notify::new());
                let finished = Arc::new(tokio::sync::Notify::new());
                let producer_slot = slot.clone();
                let begin = checked.clone();
                let done = finished.clone();
                let producer = tokio::spawn(async move {
                    begin.notified().await;
                    *producer_slot.state.lock().await = if failure {
                        SlotState::Failed("controlled failure".into())
                    } else {
                        SlotState::Ready(Arc::new(vec![1, 2, 3]))
                    };
                    producer_slot.notify.notify_waiters();
                    done.notify_one();
                });
                let result = tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    read_from_slot_with_hook(state.clone(), &key, &slot, || {
                        let begin = checked.clone();
                        let done = finished.clone();
                        async move {
                            begin.notify_one();
                            done.notified().await;
                        }
                    }),
                )
                .await
                .expect("lost wakeup");
                assert_eq!(result.is_err(), failure);
                assert!(state.slots.lock().unwrap().is_empty());
                producer.await.unwrap();
                drop(slot);
                assert_eq!(state.buffer_sem.available_permits(), 1);
            }
        }
    }

    #[cfg(feature = "default-source")]
    #[tokio::test]
    async fn dropping_session_aborts_worker_and_consumer_tasks() {
        let original = Arc::new(ReqwestSource::with_concurrency(2));
        // A cloned session has its own lifecycle even while the original Arc lives.
        let session = original.as_ref().clone();
        session.try_start_prefetch(&TextResource {
            content: "#EXTM3U\n#EXT-X-TARGETDURATION:10\n#EXTINF:10,\nworker.ts\n#EXT-X-ENDLIST\n"
                .into(),
            location: SourceLocation::Url(Url::parse("http://127.0.0.1:9/media.m3u8").unwrap()),
        });
        // Poll a consumer-created slot without giving its fetch time to finish.
        let location = SourceLocation::Url(Url::parse("http://127.0.0.1:9/consumer.ts").unwrap());
        let mut read = session.read_bytes(&location, None);
        assert!(std::future::poll_fn(|cx| Poll::Ready(read.as_mut().poll(cx).is_pending())).await);
        drop(read);
        let handles = session.tasks.lock().unwrap().clone();
        assert_eq!(
            handles.len(),
            3,
            "two workers and one consumer-created fetch"
        );
        drop(session);
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while handles.iter().any(|h| !h.is_finished()) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("session background work outlived task");
        assert!(original.state.get().is_none());
        assert!(original.tasks.lock().unwrap().is_empty());
    }
    #[derive(Debug, Clone)]
    struct SessionSource {
        stopped: Arc<std::sync::atomic::AtomicUsize>,
        signal: Arc<tokio::sync::Notify>,
        once: Arc<std::sync::atomic::AtomicBool>,
    }
    impl Source for SessionSource {
        fn create_session(&self) -> Option<Arc<dyn Source>> {
            Some(Arc::new(Self {
                once: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                ..self.clone()
            }))
        }
        fn stop_session(&self) {
            if !self.once.swap(true, std::sync::atomic::Ordering::SeqCst) {
                self.stopped
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.signal.notify_one();
            }
        }
        fn read_text<'a>(
            &'a self,
            _: &'a SourceLocation,
        ) -> Pin<Box<dyn Future<Output = Result<TextResource>> + Send + 'a>> {
            Box::pin(std::future::pending())
        }
        fn read_bytes<'a>(
            &'a self,
            _: &'a SourceLocation,
            _: Option<&'a ByteRange>,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>>> + Send + 'a>> {
            Box::pin(std::future::pending())
        }
    }
    #[derive(Debug)]
    struct WatchCancel(tokio::sync::watch::Sender<bool>);
    impl crate::CancelToken for WatchCancel {
        fn is_cancelled(&self) -> bool {
            *self.0.borrow()
        }
        fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            let mut receiver = self.0.subscribe();
            Box::pin(async move {
                let _ = receiver.wait_for(|value| *value).await;
            })
        }
    }
    #[tokio::test]
    async fn cancel_stops_session_before_reader_is_dropped() {
        let stopped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let signal = Arc::new(tokio::sync::Notify::new());
        let original = Arc::new(SessionSource {
            stopped: stopped.clone(),
            signal: signal.clone(),
            once: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
        let token = Arc::new(WatchCancel(tokio::sync::watch::channel(false).0));
        let first = SourceReader::new(original.clone(), Some(token.clone()));
        let second = SourceReader::new(original.clone(), None);
        token.0.send_replace(true);
        tokio::time::timeout(std::time::Duration::from_secs(1), signal.notified())
            .await
            .unwrap();
        assert_eq!(stopped.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(!original.once.load(std::sync::atomic::Ordering::SeqCst));
        drop(first);
        drop(second);
        assert_eq!(stopped.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
}
