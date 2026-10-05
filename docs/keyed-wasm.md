# Minimal keyed WASM host adapter

Build from the repository or unpacked crate root (wasm32 target and wasm-bindgen CLI 0.2.100 required):

```sh
cargo build --no-default-features --example keyed_wasm --target wasm32-unknown-unknown
wasm-bindgen target/wasm32-unknown-unknown/debug/examples/keyed_wasm.wasm --target web --out-dir target/keyed-wasm-pkg
```

```js
import init, { transmux } from './target/keyed-wasm-pkg/keyed_wasm.js';
await init();
const mp4 = await transmux(
  playlistText,
  'https://media.example/input.m3u8',
  resourceBuffers, // { 'https://media.example/seg0.ts': Uint8Array, ... }
  async wire => {
    const { uri, sequence, revision, kind } = JSON.parse(wire);
    // sequence/revision are decimal strings; do not convert them to Number.
    // Apply your authorization and transport policy here.
    return resolveAuthorizedKey(uri, { sequence, revision, kind }); // Uint8Array(16) or null
  },
  wire => {
    const progress = JSON.parse(wire);
    // downloadedBytes/decryptedBytes/committed are decimal strings.
    showProgress(progress);
  },
);
```

`resourceBuffers`, `resolveAuthorizedKey` and `showProgress` are host-owned example
inputs/functions. All source buffers are copied into MemorySource; the host must
bound prefetch/buffering itself. Provider exceptions/rejections and invalid replies
become safe typed library failures; null means unavailable. Progress observer
exceptions are ignored. This minimal adapter returns classic MP4 bytes and does
not expose a sink, cancellation handle, auth retry or HTTP fetcher; wire those
through the full Rust API for a production streaming adapter.

The example emits no URLs, IDs, key versions or key bytes in progress/errors.
Only the explicitly trusted provider receives the key URI. It assigns no expiry;
the clock clamps wall-clock regressions within this operation. Rust key buffers
are zeroized by SecretKey; the host owns JS input/key/output copies and their cleanup.

The [runtime suite](runtime-tests.md) executes this exact adapter in Node and Chrome with real Promises
and a non-Send JS callback, including provider rejection, unavailable keys and
invalid lengths.
