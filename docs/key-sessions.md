# Key sessions

`crypto::key` provides operation-scoped AES key resolution over typed playlist
metadata. It performs no HTTP, decryption or media execution.
Legacy `Source`, prepared sessions, errors and checkpoint serialization are unchanged.

```rust,no_run
use std::sync::Arc;
use hls_engine::legacy::{crypto::key::*, playlist::SegmentDescriptor};

async fn resolve(
    provider: Arc<dyn KeyProvider>,
    clock: Arc<dyn KeyClock>,
    segment: &SegmentDescriptor,
) -> Result<Arc<ResolvedKey>, KeyError> {
    let session = KeySession::new(
        "operation-42", "opaque-authorization-scope", provider, clock,
        KeySessionOptions::default(),
    )?;
    let key = session.try_resolve(KeyResource::media(segment))?.await?;
    // A future decryptor must recheck validity immediately before using retained keys.
    // Keep the session alive throughout an operation, rather than per resource.
    Ok(key)
}
```

The caller supplies a monotonic millisecond `KeyClock`; provider `valid_until`
uses that same domain (convert server wall-clock expiry in the adapter). There
is no WASM timer requirement. Providers resolve lazily on first poll. Native
provider/clock traits require Send + Sync and returned futures require Send;
WASM permits non-Send JS values/futures. No runtime spawning or unsafe Send shim
is used. `KeyResource::map(segment)` preserves the MAP declaration's frozen key
context. Clear resources bypass key resolution. Sample schemes and GCM remain
unsupported; supplying a KID does not enable parsing or executing those schemes.

## Candidate policy and transport

`KeySessionOptions::with_formats` declares the adapter's supported KEYFORMATs
and versions in preference order. Default is identity/version 1. Within a
preference, source declaration order is retained. A candidate needs a supported
version intersection; unknown formats/versions are skipped. Conflicting methods
or IVs fail before calling a provider. Only `Unavailable` advances to another
candidate; authorization, transport and response failures are terminal. The core
never retries transport and never tries keys in response to decryption failure.
Available values bind to a method via `aes128`, `sample_aes` or `sample_aes_ctr`
and must echo an expected KID exactly with `with_kid`, if supplied.
Sample keys share candidate selection, request merging, TTL, invalidation and
cancellation with whole-resource AES-128.

`KeyRequest` carries operation and opaque auth scope, selected `KeyReference`,
resource/input/generation/epoch/original sequence, optional KID, resolve revision,
refresh generation/reason and cancellation. Accessors retain full transport URIs;
Debug does not expose credentials. All u64 identities must be bridged as decimal
strings, as demonstrated by the actual WASM test adapter. JS response adapters
must check the byte type and length without coercing arbitrary objects/arrays.
The core does not bundle a browser or HTTP adapter; the runtime suite demonstrates
Promise/AbortController wiring against the production API.

## Cache, versions and budgets

Each session fixes one operation/auth scope and owns independent state, even if
another session uses the same scope strings and provider. Sharing across inputs
or generations is deliberately conservative. The internal identity includes
input/generation/epoch, complete key candidates (full URI, declaration revision
and ordinal, method/format/versions/IV/extensions), and KID. Resource sequence and
media URI are per-waiter diagnostic context, so adjacent resources under the same
key declaration coalesce. A new KEY declaration at the same URI resolves again.

A successful cache entry retains provider version and expiry. An absent provider
version becomes `KeyVersion::Revision`, a checked operation-local counter, never
a key hash. `ResolvedKey::resolve_revision` distinguishes resolves even when a
provider repeats its version. The cache uses LRU order, checks expiry on admission,
completion and delivery, and rejects keys already expired on arrival. Retained
`Arc<ResolvedKey>` values must also be checked with `is_valid_at` at use time.
`Expired` is a typed result; retrying resolution refreshes without hidden retries.

`invalidate()` clears the whole session's cache and advances a checked generation.
Existing waiters remain bound to their old request; their results cannot replace
the new cache. New resolves use the new generation and still consume normal
budgets. Cache eviction/refresh does not revoke already returned key handles.
Authorization changes require a new session, with cancellation of the old owner.

Defaults are **2 in-flight resolutions, 8 cached keys, 2 waiting resources**.
`with_limits` accepts positive values only. Each `KeyWaiter` occupies one resource
slot, including shared requests and cache hits until delivered. Admission returns
`BudgetExceeded` immediately when full: stop scheduling/reading and retry after
existing work finishes. There is no hidden or unbounded admission queue. Successful,
failed, dropped and cancelled waiters release their slots. `stats()` counts current
core-owned entries, not host Promise allocations or caller-held resolved keys.
Resource byte caps and media pipeline backpressure belong to resource and writer modules, not this module.

## Cancellation and sensitive data

Dropping or consuming `KeyWaiter::cancel` removes only that waiter. The final
waiter cancels and removes its pending request. `KeySession::cancel` and owner Drop
abort all pending requests and clear cache; cancellation is idempotent. Existing
waiters wake and return `Cancelled`. Completed requests do not receive abort calls.
A provider must check `request.cancellation().is_cancelled()` on entry (cancellation
can race invocation), observe its async `cancelled()` signal and/or implement
`abort` for a synchronous host AbortController. No state lock is held across a
provider/clock callback, await, abort callback, or provider-future destruction.
Late results cannot write cache after cancellation or owner Drop. An uncooperative
host Promise can still occupy host memory until it settles; it cannot be forcibly
erased by Rust. The bridge retains rejection handling for such promises.

`SecretKey` accepts exactly 16 bytes, is not Clone/Serialize, and zeroizes its owned
buffer on final release. Errors never contain key bytes. Debug masks secrets,
provider versions, operation/auth identifiers and raw provider causes. Request and
error resource diagnostics strip URL credentials, query and fragment. Provider
errors retain a typed kind and raw cause behind explicit `raw_cause()` access;
`Error::source()` intentionally does not expose the potentially sensitive cause
to generic error-chain loggers. Explicit URI/version/cause/secret access is sensitive.
JS arrays, host buffers, registers and copies owned by a provider are outside the
Rust zeroization guarantee. Cancellation clears core ownership; it cannot revoke
key handles the caller has already received.

## Verification

`tests/key_provider.rs` covers candidate policy, typed failure/cause, declarations,
MAP context, KID, scope isolation, coalescing, budgets, LRU, injected expiry,
invalidation with out-of-order completion, per-waiter diagnostics, actual wakeups,
cancellation, synchronous reentry and redaction. The non-published
[Runtime regression](runtime-tests.md) runs the production implementation in native,
Node/WASM and real headless Chrome, including Promise rejection, synchronous throw,
strict Uint8Array conversion, abort, late success/rejection and decimal identities.
