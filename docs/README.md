# Documentation

Start with the [README](../README.md), then the [Engine integration guide](engine.md).

## Engine and media contracts

- [Engine sessions, native recovery and compatibility](engine.md)
- [Container capabilities and player limits](support.md)
- [Typed playlists and lossless metadata](typed-playlists.md)
- [Key provider lifecycle and budgets](key-sessions.md)
- [AES-128 resources and byte ranges](aes-resources.md)
- [Subtitle resources and acknowledged sidecars](subtitle-sidecars.md)
- [Sample encryption](sample-encryption.md)
- [Memory bounds](benchmarks.md)
- [Native, Node and Chrome CI checks](runtime-tests.md)

## Compatibility API guides

These guides describe APIs in `hls_engine::legacy`. New applications can use the
root `Engine*` interface for finite and continuous selected inputs.

- [Fixed multi-track, wvtt and Packed AAC](multitrack-sessions.md)
- [Continuous Live/EVENT sessions](continuous-sessions.md)
- [Timeline ranges, epochs and split outputs](timeline-sessions.md)
- [Clear prepared sessions](prepared-sessions.md)
- [Keyed prepared sessions](keyed-sessions.md)
- [Diagnostics, progress and capability queries](keyed-contracts.md)
- [Streaming writer API](writer-streaming-api.md)
- [WASM Promise provider example](keyed-wasm.md)

API guides, runnable examples and their fixtures are included in the crate archive.
Repository-only CI tooling and runtime harnesses are excluded.
