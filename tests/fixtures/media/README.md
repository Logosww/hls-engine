# Retained media verification corpus

Fourteen six-second HLS inputs generated independently with FFmpeg from `testsrc2`
(or a 16×16 black frame) and a 440 Hz sine wave. Each playlist has at least three
consecutive, distinct segments. They are continuous encoder runs, not copies of a
single segment with patched timestamps. These synthetic fixtures use the
repository's MIT license and contain no downloaded third-party media.

| Input | Coverage | Fragmented / Native streaming / batch |
| --- | --- | --- |
| `ts_avc_regular` | Continuous AVC/AAC TS, B frames | pass / pass / pass |
| `ts_avc_vfr` | Continuous variable frame rate TS | pass / pass / pass |
| `ts_hevc_regular` | Continuous HEVC/AAC TS, B frames | pass / pass / pass |
| `fmp4_avc_regular` | External AVC/AAC fMP4, B frames | pass / pass / pass |
| `fmp4_avc_negative_cts` | Signed composition offsets | pass / pass / pass |
| `fmp4_avc_vfr` | Variable frame rate fMP4 | pass / pass / pass |
| `fmp4_avc_offset` | Delayed audio start | pass / pass / pass |
| `fmp4_avc_nal2_multirun` | avc3, two-byte NAL lengths, multiple trun | pass / pass / pass |
| `fmp4_avc_nal1_multirun` | avc3, one-byte NAL lengths, multiple trun | pass / pass / pass |
| `fmp4_hevc_regular` | External HEVC/AAC fMP4, hev1 | pass / pass / pass |
| `ts_aac_audio_only` | Pure AAC TS | pass / pass / pass |
| `ts_avc_video_only` | Pure AVC TS | pass / pass / pass |
| `fmp4_aac_audio_only` | Pure AAC fMP4 | pass / pass / pass |
| `fmp4_avc_video_only` | Pure AVC fMP4 | pass / pass / pass |

Pure-track cases are supported in all three modes starting in v0.5.0.

`manifest.json` contains the generator's FFmpeg/FFprobe versions, portable command
arguments, derived-fixture transformations, SHA-256 hashes of every input file,
and external reference track codecs, packet counts, time bases and timestamps.
The NAL/multi-run variants are transformations of FFmpeg output performed by
`rewrite_nal2_and_runs` in the verification script, then read and decoded by the
external tools. They are not outputs of this crate. For TS, the reference packet
probe reads concatenated physical segments: FFprobe's HLS demuxer repeats the
first packet of the pure AAC playlist in the recorded environment.

From the repository root:

```sh
# No FFmpeg required: validate fixture hashes and all 42 API combinations.
cargo test --offline --test media_corpus
cargo test --offline --no-default-features --test media_corpus

# FFmpeg/FFprobe required: verify retained inputs and outputs independently.
python3 scripts/verify_media.py

# Rebuild all inputs, provenance, hashes and reference metadata, then verify.
python3 scripts/verify_media.py --generate
```

The external check asserts segment uniqueness, monotonic input DTS, track types,
codec selection, B-frame/VFR/signed-offset/audio-delay properties; compares packet
counts, DTS/PTS and duration within one output tick and normalized NAL/AAC content;
checks faststart or fragmented top-level layouts; decodes and seeks every present
track. The current suite expects 42 successful outputs.
The FFmpeg CI job runs this retained corpus through the existing script entry.
No target player is required or invoked.

Generation needs FFmpeg with `libx264`, `libx265` and the native AAC encoder.
Regeneration may change binary hashes and encoder packet choices across FFmpeg
versions; review the manifest and media together. Ordinary validation never
rewrites fixtures. This small corpus is a correctness baseline, not a long-video
performance benchmark, browser test, or production/device capture corpus.
