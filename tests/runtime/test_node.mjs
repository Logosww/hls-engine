import { readFile, writeFile, mkdir } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';
import { verify } from './harness.mjs';

const root = fileURLToPath(new URL('../../', import.meta.url));
const unhandled = [];
process.on('unhandledRejection', error => unhandled.push(String(error)));
const wasm = await readFile(new URL('../../target/runtime/pkg/hls_transmux_runtime_tests_bg.wasm', import.meta.url));
const result = await verify(wasm);
await new Promise(resolve => setImmediate(resolve));
assert.deepEqual(unhandled, []);
const native = spawnSync('cargo', ['run', '--locked', '--quiet',
  '--manifest-path', 'tests/runtime/Cargo.toml', '--target-dir', 'target', '--bin', 'probe'],
{ cwd: root, encoding: 'utf8' });
assert.equal(native.status, 0, native.stderr);
assert.deepEqual(result.report, JSON.parse(native.stdout), 'native/WASM semantic mismatch');
await mkdir(new URL('../../target/runtime/', import.meta.url), { recursive: true });
await writeFile(new URL('../../target/runtime/node-evidence.json', import.meta.url),
  JSON.stringify({ runtime: process.version, nativeWasmEqual: true, ...result }, null, 2) + '\n');
console.log(JSON.stringify({ nativeWasmEqual: true, bridge: result.bridge }));
