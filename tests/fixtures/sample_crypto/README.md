# Independent sample-encryption fixtures

Shaka Packager v3.9.3 (0a8ba4f) packaged the repository's synthetic MIT FFmpeg
corpus into clear and encrypted AVC/HEVC/AAC fMP4 and AVC/AAC TS. `manifest.json`
records tool SHA-256, version, commands, public test key/KID/IV and file hashes.
The downloaded macOS arm64 tool's SHA-256 was verified against its release asset
metadata. The tool is Apache-2.0; no third-party media or production secrets occur.

For cenc, the harness adds the deprecated HLS SAMPLE-AES-CTR declaration omitted
by Shaka's HLS writer; the independently generated encrypted container is unchanged.
The suite compares remuxed clear/encrypted outputs byte-for-byte after zeroing
only MP4 creation/modification clock fields. It exercises original senc+saiz/saio,
inline-only and auxiliary-only variants and a fragment-local seig KID override.
The variant keeps Shaka ciphertext intact and uses the same public key for the
second test KID; it is not evidence of an independent encryption implementation.

The retained Rust tests verify fixture hashes and compare clear/encrypted output.
Runtime tests embed the same corpus; they need no network or packager. Generation
commands and public test parameters are preserved in `manifest.json`.

Additional 1/2-byte AVC-prefix multirun fixtures use OpenSSL CTR over the existing
independent clear corpus using OpenSSL. The manifest
records source hashes, OpenSSL version and commands. Prefix bytes are encrypted,
so these also verify that normalization waits for decryption.
