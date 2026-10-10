# WebVTT AES-128 fixture provenance

Generated with the system OpenSSL CLI, independently of RustCrypto.
Public fixture key: bytes `00..0f`; rotated key: bytes `10..1f`.
`sequence.cbc` uses the 128-bit big-endian IV 42; `rotated.cbc` uses 43.
`explicit.cbc` and `header.cbc` use IV bytes `00..0f`. All use PKCS7.

Command: `openssl enc -aes-128-cbc -K <hex-key> -iv <hex-iv>` with the
corresponding `.vtt` file on stdin (`segment.vtt` for all except header).

SHA-256:

- `explicit.cbc`: `30477544823141535017d32e5cca02790d9b80e7c630ae53098f46c0cc9be8b8`
- `header.cbc`: `ead9ad23dc13b94525ec9aeced1be9fa0d99fbce042ca12ae2732a5f45dca16b`
- `rotated.cbc`: `18f4d7a07a18a634c61080d1e7f305ee23a9feb889a6d81211c72dbc02537571`
- `sequence.cbc`: `31e404ca320cfb886974af9e30a21bb994712958635c99548e7441fdd5bc0631`
