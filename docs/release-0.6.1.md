# v0.6.1 release verification

Local verification completed on 2026-10-05 on macOS. This patch fixes
[Issue #1](https://github.com/Logosww/hls-transmux/issues/1): classic MP4 retained
20 HEVC video samples but decoded only 18 displayed frames with normal edit-list
handling. The media edit now starts at the minimum PTS minus the first DTS, in
the track's timescale, so its presentation span covers every sample.

Public APIs remain compatible. Signed composition offsets, initial audio/video
offsets and complete audio tails are retained.

## Verification

- The independent reproduction failed before the fix with 18/20 video frames.
- `python3 scripts/verify_keyed_decode.py`: 30 outputs passed, each with 20/20
  video frame hashes and complete audio matching independently generated clear
  inputs. Covers HEVC/AVC, clear/AES-128, TS/fMP4, encrypted MAPs, signed CTS,
  delayed audio and longer audio tails across bytes, file and writer APIs.
- `python3 scripts/verify_multi_input.py`: 36 selected dual-input outputs passed
  packet, payload, timestamp, duration and decoded video-frame checks.
- `python3 scripts/verify_media.py`: 14 retained inputs and 42 outputs passed.
- All-feature Rust tests: 198 passed, 3 existing ignored tests.
- Rust tests without default features: 162 passed, 3 existing ignored tests.
- Formatting, all-feature/all-target Clippy and minimal WASM compilation: passed.

The keyed decode regression runs in CI and requires FFmpeg with libx264/libx265,
FFprobe and OpenSSL. Inputs are generated locally; no media downloads are needed.
