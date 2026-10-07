# hls-engine

A Rust HLS media engine with a shared native and WebAssembly core. It handles
selected VOD and Live/EVENT inputs, resource and sample decryption, timeline
ranges, multiple audio tracks, WebVTT cue muxing and MP4 output.

Transport, playlist refresh, rendition selection, key acquisition and WebVTT
parsing belong to the application. The engine does not transcode media.

## Install

```toml
[dependencies]
hls-engine = "1.0"
```

Disable `default-source` with `default-features = false` to provide your own
`Source` without the built-in reqwest dependency. WASM builds use this option.

| Feature | Purpose |
| --- | --- |
| `default-source` (default) | Native reqwest HTTP source |
| `serde` | Lossless playlist, report and checkpoint serialization |
| `ffmpeg-finalize` | Optional FFmpeg 9 finalizer for compatible legacy file APIs |
| `experimental-gcm` | Fixed HLS draft-22 AES-256-GCM resource profile; runtime opt-in also required |

## Start a session

1. Parse each selected media playlist into a `PlaylistSnapshot` with a stable `InputId`.
2. Construct `EngineInputs`, a `KeySession` and `EngineOptions`.
3. Submit snapshots through `EngineHandle`, and end each finite input independently.
4. Run `into_bytes`, `write_to`, `write_to_file` or a split-output provider.

The executable example uses a synthetic TS resource and needs no network access:

```sh
cargo run --locked --example engine_demo
```

```rust,no_run
use hls_engine::{EngineInputs, EngineOptions, EngineSession, OutputFormat};
use hls_engine::crypto::key::KeySession;
use hls_engine::playlist::PlaylistSnapshot;

async fn collect(
    inputs: EngineInputs,
    keys: KeySession,
    snapshot: PlaylistSnapshot,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let id = snapshot.context().input_id().clone();
    let session = EngineSession::new(inputs, keys, EngineOptions::default())?;
    session.handle().accept_snapshot(&id, &snapshot)?;
    session.handle().end_input(&id)?;
    let (bytes, _) = session
        .into_bytes(64 * 1024 * 1024, OutputFormat::FragmentedMp4)
        .await?;
    Ok(bytes)
}
```

For multiple inputs, supply the host waiter and clock-alignment evidence described
in the [Engine guide](docs/engine.md). A rendition sequence number is not a shared
presentation clock. Admission is atomic: retry `WouldBlock` after capacity becomes
available. `stop` drains accepted work; `cancel` aborts it.

## Media and output

- MPEG-TS: AVC/HEVC video and AAC-LC audio.
- Fragmented MP4: supported AVC/HEVC/AAC-LC tracks with validated sample timing.
- Packed AAC: ID3 transport timestamps and validated ADTS frames.
- Decryption: AES-128 resources, TS SAMPLE-AES, supported fMP4 `cbcs`/`cenc`,
  and the explicitly enabled experimental GCM profile.
- Fixed multi-track sessions: selected AAC audio tracks and application-supplied
  WebVTT cues, with explicit epoch, range, gap and configuration-change policies.
- Fragmented MP4 output supports bounded recording. Native classic MP4 uses disk
  staging and a sample index whose size grows with the recording.

Container support and player support are separate decisions. Consult
[capabilities and player limits](docs/support.md) before enabling a combination.
DRM/CDM, license exchange, LL-HLS partial segments, dynamic track replacement and
transcoding are outside the supported profile.

## Recovery and ownership

Native `write_recoverable_to_file` emits schema-2 `EngineCheckpoint` values through
an independent fallible callback. Persist them atomically. Restoration verifies
resource identities, key versions, committed file prefixes and completed split
children before appending. A fresh output never overwrites a competing file.

`Flush` and `SyncAll` describe file durability. The caller still owns checkpoint
and directory persistence, source/provider reconstruction and bounded replay of
queued resources and cues. Runtime events and reports are not checkpoints.
See [recovery](docs/engine.md#native-file-recovery) for the complete contract.

Borrowed writers are flushed and remain caller-owned. Their close/shutdown must
finish before the application reports completion. Collected byte output requires
an explicit capacity. Native file recovery is not exposed for WASM borrowed
writers.

## Documentation

The [documentation index](docs/README.md) covers Engine sessions, encryption,
playlists, WASM bridges, memory limits and compatibility APIs.
`hls_engine::legacy` retains existing entry points and schema-1 tasks. New sessions
use the root `Engine*` API; checkpoint schemas are never silently converted.

## Development

```sh
cargo test --locked --features serde,experimental-gcm
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
python3 -m unittest discover -s scripts -p test_verify_engine_release.py
```

[CI](https://github.com/Logosww/hls-engine/blob/main/.github/workflows/ci.yml) runs the feature matrix on Linux, macOS and Windows,
real Node/Chrome WASM contracts, independent FFmpeg inspection, recovery and
package checks. Repository scripts are limited to that workflow and its
transitive dependencies; they are excluded from the published crate.

## 中文说明

本项目提供共用的 Rust/native/WASM HLS 处理核心，支持有限和持续输入、解密、
时间范围、多音轨、字幕 cue 合流、MP4 输出及 native 文件恢复。应用负责网络传输、
清单刷新、选轨、密钥获取和 WebVTT 解析。新接入使用 `EngineSession`，完整契约见
[接入指南](docs/engine.md)与[支持范围](docs/support.md)。

## License

MIT.
