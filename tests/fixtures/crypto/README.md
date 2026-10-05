# Independent AES-128 resource fixtures

Synthetic MIT media comes from the retained FFmpeg corpus in `../media`; no
third-party media or production credentials are used. `manifest.json` records
public keys, original sequence > 2^53, explicit/implicit IVs, OpenSSL version,
clear-source paths/hashes and ciphertext hashes. `scripts/verify_crypto.py`
is the reproducible generator and independent checker.

Six inputs cover AVC/HEVC/AAC in TS and fMP4. Each has two complete encrypted
segments (a new KEY declaration at the same key URI rotates A to B) followed by
METHOD=NONE clear data. fMP4 adds an encrypted MAP with its own explicit IV. Range
playlists reference independently encrypted complete resources in a bundle;
the MAP bundle has a seven-byte prefix to verify arbitrary resource offsets.
The clear third segment is sourced from the existing corpus by the Rust adapter.

`vector.cbc` is the NIST SP800-38A F.2.1 plaintext encrypted with OpenSSL PKCS7;
Rust also checks the published unpadded NIST CBC blocks. `invalid-padding.cbc`
is one independently encrypted zero block with no padding (decoded last byte 0,
an invalid PKCS7 length). `invalid-container.cbc` is valid PKCS7 encryption of
plain non-media text. These distinguish cryptographic and container failures.

```sh
python3 scripts/verify_crypto.py                         # retained hashes
python3 scripts/verify_crypto.py --openssl --ffmpeg      # external decrypt + decode
python3 scripts/verify_crypto.py --generate --openssl --ffmpeg
cargo test --test aes128_resource
```

Regeneration requires OpenSSL and uses the existing FFmpeg corpus without changing
it. Verification with `--ffmpeg` decodes the independently decrypted media sequence.
Ordinary verification never modifies fixtures. Binary ciphertext/bundles are marked
`-text` in `.gitattributes` to preserve their hashes across platforms.
