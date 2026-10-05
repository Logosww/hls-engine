# Documentation

The [root README](../README.md) is the introduction and quick start for the
`hls-transmux` crate. Version 0.7.0 adds finite ranges, epochs, gap policies and split outputs;
its verification record is [release-0.7.0.md](release-0.7.0.md).

## Choose an entry point

- [Timeline ranges, epochs and split outputs](timeline-sessions.md)
- [Clear prepared sessions](prepared-sessions.md)
- [Clear/AES-128 keyed prepared sessions](keyed-sessions.md)
- [Streaming writer API](writer-streaming-api.md)
- [WASM Promise provider example](keyed-wasm.md)

## API contracts and limits

- [Typed playlists and lossless metadata](typed-playlists.md)
- [Key provider lifecycle and budgets](key-sessions.md)
- [AES-128 resources and range validation](aes-resources.md)
- [Diagnostics, progress and capability queries](keyed-contracts.md)
- [Benchmarks and memory bounds](benchmarks.md)

## Verification

- [Native/Node/Chrome runtime regression](runtime-tests.md)
- [v0.7.0 verification record](release-0.7.0.md)
- [v0.6.2 fragmented duration verification](release-0.6.2.md)
- [v0.6.1 B-frame fix verification](release-0.6.1.md)
- [v0.6.0 verification record](release-0.6.0.md)
- [Machine-readable v0.6.0 evidence](release-0.6.0-evidence.json)

These guides and both keyed examples are included in the crate archive. Build the
examples from the repository or an unpacked crate; the WASM guide lists the target
and wasm-bindgen prerequisites.
