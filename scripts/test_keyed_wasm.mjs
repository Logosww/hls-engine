// Execute the actual packaged WASM example, including asynchronous host failures.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const [modulePath, crateRoot] = process.argv.slice(2);
const { default: init, transmux } = await import(pathToFileURL(resolve(modulePath)));
await init({ module_or_path: await readFile(modulePath.replace(/\.js$/, '_bg.wasm')) });
const fixture = resolve(crateRoot, 'tests/fixtures');
const playlist = await readFile(`${fixture}/crypto/ts_avc_regular/input.m3u8`, 'utf8');
const resources = {};
for (const name of ['seg0.cbc', 'seg1.cbc', 'clear.bin']) {
  resources[`https://media.test/${name}`] = new Uint8Array(await readFile(name === 'clear.bin'
    ? `${fixture}/media/ts_avc_regular/seg2.ts` : `${fixture}/crypto/ts_avc_regular/${name}`));
}
const keys = ['2b7e151628aed2a6abf7158809cf4f3c', '603deb1015ca71be2b73aef0857d7781']
  .map(hex => Uint8Array.from(Buffer.from(hex, 'hex')));
const unhandled = [];
process.on('unhandledRejection', error => unhandled.push(String(error)));
let requests = 0;
const events = [];
const bytes = await transmux(playlist, 'https://media.test/list.m3u8', resources, async wire => {
  const request = JSON.parse(wire);
  assert.equal(typeof request.sequence, 'string');
  assert.ok(['9007199254740993', '9007199254740994'].includes(request.sequence));
  assert.equal(typeof request.revision, 'string');
  requests++;
  await new Promise(resolve => setTimeout(resolve, 0));
  return keys[request.sequence === '9007199254740994' ? 1 : 0].slice();
}, wire => events.push(JSON.parse(wire)));
assert.ok(bytes instanceof Uint8Array && bytes.length > 1000);
assert.equal(events.at(-1).phase, 'Completed');
assert.equal(events.at(-1).committed, '3');
assert.equal(requests, 2);
assert.ok(BigInt(events.at(-1).downloadedBytes) > BigInt(events.at(-1).decryptedBytes));
for (const provider of [async () => { throw new Error('HOST_SECRET'); }, async () => null, async () => new Uint8Array(15)]) {
  const failed = [];
  await assert.rejects(async () => {
    await transmux(playlist, 'https://media.test/list.m3u8', resources, provider, wire => failed.push(JSON.parse(wire)));
  }, error => !String(error).includes('HOST_SECRET'));
  assert.ok(!failed.some(event => event.phase === 'Completed'));
}
await new Promise(resolve => setImmediate(resolve));
assert.deepEqual(unhandled, []);
console.log(JSON.stringify({ packagedWasm: true, requests, outputBytes: bytes.length,
  callbackEvents: events.length, providerFailureCases: 3, unhandledRejections: 0 }));
