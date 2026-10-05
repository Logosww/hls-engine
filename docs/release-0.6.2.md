# v0.6.2 release verification

This patch fixes [Issue #2](https://github.com/Logosww/hls-transmux/issues/2):
prepared fragmented MP4 with offset external audio counted the delayed track's
initial offset twice in FFprobe and Chrome file playback duration.

Prepared clear and keyed sessions now write track-local fragment decode times
with exact, unit-rate movie edits carrying the initial offsets. The media edit
has zero duration so subsequent fragments determine the end. Movie/track/media
header durations remain zero; streaming needs no seek or full-input buffering.
Packet PTS/DTS, payloads, shared-clock reports and public APIs remain compatible.
Optional random-access indexes use media-local times, and native finalization
restores the movie timeline when scanning the temporary fragments. Legacy
single-input streaming and checkpoint schema v1 are unchanged.

## Independent regression

`python3 scripts/verify_fragmented_timeline.py --browser` generates 2-second
AVC/AAC inputs locally with FFmpeg and synthetic AES-128 with OpenSSL. Both mixed
container directions retain their original clock differences. It checks all
packets, selected payloads, video frame hashes, exact cross-output timestamps,
FFprobe container duration and real Chrome `video.duration`. Indexed and
unindexed writers are both covered (16 outputs total).

| Primary / external AAC | 0.6.1 writer duration | 0.6.2 bytes / file / writer duration |
| --- | --- | --- |
| TS / fMP4 | 4.842666 s | 3.421333 s |
| fMP4 / TS | 4.784083 s | 3.405375 s |

Clear and AES-128 results agree. The original TS/fMP4 writer case failed before
the fix. Input edit rounding is allowed within one input timescale tick;
container duration is checked against actual packet presentation end within
one 48 kHz tick. The independent FFprobe regression also runs in CI.

Chrome verification covers ordinary file/Blob playback. Chromium MSE ignores
leading empty edits, so these offset prepared files require an MSE-aware host
to apply track offsets separately; they should not be appended unchanged to a
single multiplexed SourceBuffer. This is a separate player path, documented in
the [prepared-session guide](prepared-sessions.md).

## Additional checks

- B-frame regression: 30 keyed outputs retain every video frame and complete audio.
- Dual-input regression: 48 outputs, including both native and FFmpeg finalization.
- Rust regression covers exact fractional offsets, 64-bit edit durations,
  signed CTS and media-local index times through native scanning.
- All-feature Rust tests: 199 passed, 3 existing ignored tests.
- Native, actual Node/WASM and Chrome runtime contracts agree.
- Publication dry-run and extracted-package native/WASM examples passed.

Local verification: macOS, 2026-10-05.
