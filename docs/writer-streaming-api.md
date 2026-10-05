# Streaming writer API

Use `transmux_hls_to_writer_async(input, &mut writer, options)` for legacy clear
input. Its writer bound is `AsyncWrite + Send + Unpin`. `FragmentedMp4` writes
incrementally; `Mp4` collects samples before producing classic MP4 bytes.
`StreamingMp4` and resume require the native file entry and are rejected here.

For prepared input, `PreparedTransmux::write_to(&mut writer)` and
`KeyedPreparedTransmux::write_to(&mut writer)` emit fragmented MP4 and accept
`AsyncWrite + Unpin` without requiring Send. The keyed entry supports finite
clear/AES-128 snapshots and an external key provider.

The library borrows and flushes the writer; it never closes it. The caller owns
close/abort and delivery beyond the library flush boundary. A failed write or
final flush cannot produce a successful completion report. Prepared reads are
demand-driven, so a blocked writer suppresses further progress. This is not a
constant total-memory guarantee: resource buffers, playlist metadata and optional
fragment indexes still consume memory.

Disable `write_mfra` when the trailing random-access index is unnecessary. This
also avoids accumulating that index; it does not remove media/playlist buffering.
Use file output for native classic-MP4 finalization and legacy checkpoint resume.
Keyed prepared sessions do not support resume.

See [prepared sessions](prepared-sessions.md), [keyed sessions](keyed-sessions.md),
[resource budgets](aes-resources.md) and [benchmarks](benchmarks.md).
The [main README](../README.md) contains Vec and duplex writer examples.
