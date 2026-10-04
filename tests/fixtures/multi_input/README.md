# Selected-audio corpus

`ts/` and `fmp4/` contain 7.3 seconds of 880 Hz AAC-LC, 44.1 kHz,
segmented at approximately 1.3 seconds. Existing primary fixtures use 440 Hz /
48 kHz audio, six-second video and two-second segments. Selection is verified
by normalized AAC payload, not merely track metadata. This also exercises
unequal boundaries/counts, different timescales and an audio tail after video EOF.

`manifest.json` records generation commands and SHA-256 hashes.
Regenerate and validate: `python3 scripts/verify_multi_input.py --generate`.
Validate both native and FFmpeg finalize: add `--ffmpeg`.
