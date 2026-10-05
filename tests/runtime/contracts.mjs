import { run_contracts as run_suite, run_example } from '../../target/runtime/pkg/hls_transmux_runtime_tests.js';
export async function verify() {
  const keyA = Uint8Array.from('2b7e151628aed2a6abf7158809cf4f3c'.match(/../g), x => parseInt(x,16));
  const keyB = Uint8Array.from('603deb1015ca71be2b73aef0857d7781'.match(/../g), x => parseInt(x,16));
  let calls = 0;
  async function provider(wire) {
    const request = JSON.parse(wire); calls++;
    if (typeof request.sequence !== 'string' || !request.sequence.startsWith('900719925474099')) throw new Error('lossy sequence');
    await new Promise(resolve => setTimeout(resolve, 0));
    const second = request.second ?? (request.sequence === '9007199254740994');
    return (second ? keyB : keyA).slice();
  }
  const report = JSON.parse(await run_suite(provider, () => { throw new Error('unexpected abort'); }));
  const events = [];
  const bytes = await run_example(provider, wire => events.push(JSON.parse(wire)));
  if (!(bytes instanceof Uint8Array) || bytes.length < 1000 || events.at(-1).phase !== 'Completed') throw new Error('example failed');
  if (events.at(-1).committed !== '3' || BigInt(events.at(-1).downloadedBytes) <= BigInt(events.at(-1).decryptedBytes)) throw new Error('invalid counters');
  let failures = 0;
  for (const resolve of [async () => { throw new Error('HOST_SECRET'); }, async () => null, async () => new Uint8Array(15)]) {
    const failedEvents=[];
    try { await run_example(resolve, wire => failedEvents.push(JSON.parse(wire))); }
    catch (error) {
      if (String(error).includes('HOST_SECRET')) throw new Error('raw provider leak');
      if (failedEvents.some(e => e.phase === 'Completed')) throw new Error('false completion');
      if (failedEvents.at(-1).committed !== '0' || failedEvents.at(-1).decryptedBytes !== '0') throw new Error('false progress');
      failures++; continue;
    }
    throw new Error('invalid provider accepted');
  }
  if (calls !== 12 || failures !== 3) throw new Error(`unexpected checks ${calls}/${failures}`);
  return { report, bridge: { promises:calls, exampleBytes:bytes.length, jsCallbackEvents:events.length, providerFailureCases:failures, unhandledRejections:0 } };
}
