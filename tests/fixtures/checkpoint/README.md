# Frozen Engine compatibility fixtures

MIT-licensed metadata produced from this repository's clear AVC/AAC and subtitle
fixtures. It contains no resource URLs, raw keys or media/cue payloads.

`engine-schema2.bin` is the frozen 1.0 binary envelope, SHA-256
`81130c105a7d0383fe6008132feba20800688db5944c14dbbcbe6a02f095c9d5`.
The ignored `generate_schema2_fixture` integration helper records its provenance;
it must not be run to update the existing fixture after schema freeze. A future
incompatible format needs a new schema/fixture and an explicit migration path.

The checkpoint's destination digest intentionally binds the original test output.
Binary/serde compatibility is checked against this fixture. Actual file replay,
identity verification and sample reconstruction are covered by the separate
194-output checkpoint matrix and process-crash tests.

The schema-3 archive and `engine-schema2-start.bin` / `engine-schema3-start.bin`
were produced by the unmodified execution core from the pre-migration 1.0.2
checkout. The start fixtures stop at the initial checkpoint, after probe/parse
and before output, and retain pending samples and resource identity evidence.
Integration tests restore their execution state and compare media and subtitle
files with uninterrupted output. The test-only adapter rebinds destination hashes
to an isolated temporary directory and updates the envelope checksum; it does not
rewrite configuration digests, resource evidence or execution state.

`engine-strict-v1-hashes.json` freezes all 194 normalized outputs from the original
Engine (the MultiTrack implementation). These hashes cover clear/encrypted media,
multiple audio tracks, ranges, epochs, subtitles and split outputs, and run in the
shared native/Node/Chrome corpus. Creation timestamps alone are normalized.
Do not regenerate these baselines with the 1.1 encoder.

SHA-256 provenance:

- `engine-schema2-start.bin`: `ede75a8ff1806662207e5bf60affb101c411632301993dde4015ad25d44ec3b5`
- `engine-schema2.bin`: `81130c105a7d0383fe6008132feba20800688db5944c14dbbcbe6a02f095c9d5`
- `engine-schema3-start.bin`: `70330eabbf894c016cdeb2891f7fa35b5bd3336356a3ab31f3650e7c068647df`
- `engine-schema3.bin`: `f1cfb4895dcadb3cb469969d767785aa888bf3c6ac95e3c23803f0bae780f98a`
- `engine-strict-v1-hashes.json`: `f0dc5dafd6eba84b8e19bd9ebf3e8b2c84ca07999c1554a18535f32d827e70cd`
