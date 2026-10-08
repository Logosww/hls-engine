import { readFile } from 'node:fs/promises';
const path = process.argv[2] ?? new URL('../target/wasm32-unknown-unknown/debug/examples/wasm_session.wasm', import.meta.url);
const { instance } = await WebAssembly.instantiate(await readFile(path), {});
if (instance.exports.run_contract_tests() !== 1) throw new Error('WASM contracts did not complete');
console.log('WASM mixed-input bytes/writer/timeline/cancellation contracts passed');
