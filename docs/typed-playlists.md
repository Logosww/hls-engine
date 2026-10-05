# Typed playlist metadata (v0.6)

`parse_playlist_snapshot` parses a selected media playlist without fetching any
resource. The immutable result preserves encryption and timeline metadata for
inspection and future execution. Existing legacy/prepared entry points still use
their original parser and still reject encryption, including `METHOD=NONE`.
For execution, combine [key sessions](key-sessions.md) with the
[keyed prepared entry](keyed-sessions.md). Parsing metadata alone does not fetch
keys or decrypt resources.

```rust
use hls_transmux::{parse_playlist_snapshot, SourceLocation, TextResource};
use hls_transmux::playlist::{InputId, PlaylistContext};

let resource = TextResource {
    content: "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:1.5,\na.ts\n#EXT-X-ENDLIST\n".into(),
    // Use the final location returned by Source, after redirects.
    location: SourceLocation::Url("https://example.test/media/list.m3u8".parse()?),
};
let context = PlaylistContext::new(InputId::new("primary")?, 0).with_revision(1);
let snapshot = parse_playlist_snapshot(&resource, context)?;
assert_eq!(snapshot.segments()[0].duration().ticks(), 15);
assert_eq!(snapshot.segments()[0].duration().timescale(), 10);
assert!(snapshot.validate_finite_vod().is_ok());
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Preserved metadata and parsing rules

- Snapshot: protocol version, playlist type, ENDLIST, target duration, original
  media/discontinuity sequences, independent-segments and I-frame flags.
- Segment: resolved URI, checked byte range, exact decimal duration, explicit PDT,
  GAP, discontinuity/epoch, MAP and active key candidates. PDT retains its original
  timezone and fractional precision; no wall clock is inferred for later segments.
- KEY candidates update by KEYFORMAT. Defaults are `identity` and version `1`;
  version lists are normalized to sorted unique integers. A repeated declaration
  remains a new declaration even if its URI is unchanged. `NONE` clears all active
  candidates. MAP freezes the key context at its own declaration.
- Known sample methods, GCM and unknown method names remain inspectable.
  Unknown tag text and unknown KEY attributes are retained explicitly. The full
  original manifest is retained in the archive, including unused trailing tags.
- `parse_session_keys` extracts master SESSION-KEY hints separately. It is not a
  master rendition selector or complete master validator. `validate_session_keys`
  checks hints against media KEYs, including MAP keys; hints do not authorize,
  fetch or replace media keys. Duplicate hints and SESSION-KEY `NONE` fail.
- Relative URIs resolve against `TextResource::location`. Provider URI schemes
  can be retained, but parsing does not establish that execution supports them.
- An implicit byte offset needs the immediately preceding media segment's range
  on the same resolved URI. MAP declarations do not advance that cursor. An
  implicit MAP range without such a predecessor fails; specify `length@offset`.
  This stricter new parser does not change legacy MAP behavior.
- Durations use checked u64 ticks and u32 timescales, with up to nine meaningful
  fractional decimal places. Excess trailing zeros are accepted. Negative,
  exponential, overflowing or higher-precision values fail without rounding.
  Sequences, range ends and epoch increments also use checked arithmetic.

`validate_finite_vod` is a manifest preflight for the planned finite profile:
ENDLIST is required; EVENT, empty input, missing target duration, duration rounding
above target, GAP, discontinuity, I-frame-only input, unvalidated tags/attributes,
sample/unknown encryption, conflicting candidate methods and encrypted MAPs
without explicit IVs are rejected. A nonzero initial discontinuity sequence is
an identity value and is accepted. Successful preflight does not validate codecs,
containers, providers, complete encrypted resource ranges or media bytes, and
cannot start execution. Open/empty/EVENT snapshots can still be parsed.

## Identity

A `SegmentSlot` is `(input_id, generation, original_sequence, epoch)`. The caller
supplies input ID and generation; a restarted input uses a new generation.
`compare_resource` compares the immutable descriptor, including complete URI
(query and fragment included), range, duration, PDT, GAP, MAP and key context.
Different slots return `DifferentSlot`; identical descriptors return `Duplicate`;
changed descriptors within a comparable slot return `Rewritten`.

Declaration IDs contain the caller's snapshot revision and local declaration
ordinal. Reuse a revision only when reparsing that snapshot. They are finite
snapshot identities, not rolling-window key epochs. When both descriptors carry
KEY/MAP declarations from different revisions, comparison returns
`NeedsReconciliation`. The caller must not treat this as a duplicate or accepted
update. Cross-snapshot alignment and incremental admission remain v0.9 work;
P1 provides no update/commit API. A resource identity describes metadata, not
content integrity or a resolved provider key version.

## Serde / JavaScript transport

The optional `serde` feature adds a versioned snapshot archive (`schema_version:
1`). Every u64 field, including sequence, generation, revision, ordinal, range,
target duration, protocol version and duration ticks, is a decimal **string**.
Booleans and u32 fields remain JSON primitives; IVs are exactly 16 byte values.
No i128 values are exported by this model. JSON numbers are rejected for u64
fields even when their values would fit JavaScript's exact integer range.

```rust,ignore
let archive = serde_json::to_string(&snapshot)?;
let restored: hls_transmux::playlist::PlaylistSnapshot = serde_json::from_str(&archive)?;
assert_eq!(restored, snapshot);
```

In JavaScript use `JSON.parse` / `JSON.stringify` without converting integer
strings to Number. On deserialization the snapshot reparses the retained source
and checks the entire structured projection. Inconsistent URIs, keys, IVs,
ranges, slots or durations fail, as do unknown fields and unsupported archive
versions. This is structural validation, not authentication: an entirely replaced
consistent manifest is still valid metadata. Deserialize the complete snapshot
at trust boundaries; independently deserialized descriptor DTOs do not constitute
validated snapshots or execution permission. Non-UTF-8 file paths cannot be
represented losslessly by JSON and serialization returns an error.

Archives deliberately include the original manifest and complete resource URLs,
which may contain credentials. They are transport data, **not logs, reports or
resume checkpoints**. `Debug` and parser error `Display` omit raw manifests,
unknown payloads, input IDs and key attributes. HTTP diagnostic URLs strip
username/password/query/fragment; non-HTTP provider URLs are fully redacted.
Explicit raw accessors and generic serde errors are not diagnostic-safe. Use a
generic transport error at the host boundary. Never use redacted URLs as cache
keys. Existing checkpoint schema v1 is unaffected.

The parser retains an entire snapshot plus per-segment context. It does not
impose a total memory limit; bound manifest size before parsing untrusted input.

## Reproducible checks

`cargo test --features serde --test typed_playlist` covers metadata, numeric and
range boundaries, key/MAP transitions, preflight, identity and archive validation.
The normal compatibility suite includes existing prepared and released v0.3/v0.4
checkpoint tests plus exhaustive v0.5 role/phase matches.

[Runtime regression](runtime-tests.md) compares native Rust with
actual WASM in Node, performs a JavaScript JSON round trip, and rejects numeric
coercion and modified projections. CI also runs its production provider, resource and prepared contracts in real
Chrome. It adds no production dependencies.
