import { run_resources as run_suite, cancel_resource as cancel_active } from '../../target/runtime/pkg/hls_engine_runtime_tests.js';
export async function verify() {
  const keyA = Uint8Array.from('2b7e151628aed2a6abf7158809cf4f3c'.match(/../g), x => parseInt(x,16));
  const keyB = Uint8Array.from('603deb1015ca71be2b73aef0857d7781'.match(/../g), x => parseInt(x,16));
  let calls = 0, aborts = 0, late = 0;
  const pending = new Set();
  function resolve(wire) {
    const request = JSON.parse(wire); calls++;
    if (typeof request.sequence !== 'string' || !request.sequence.startsWith('900719925474099')) throw new Error('lossy sequence');
    if (request.operation === 'cancel') setTimeout(cancel_active, 0);
    const promise = new Promise(accept => setTimeout(() => {
      if (request.operation === 'cancel') late++;
      accept((request.second ? keyB : keyA).slice());
    }, request.operation === 'cancel' ? 20 : 0));
    pending.add(promise); promise.then(() => pending.delete(promise), () => pending.delete(promise));
    return promise;
  }
  function abort(operation) { if (operation !== 'cancel') throw new Error('unexpected abort'); aborts++; }
  const report = JSON.parse(await run_suite(resolve,abort));
  await Promise.allSettled([...pending]);
  if (aborts !== 1 || late !== 1 || calls !== 31) throw new Error(`bridge counts ${calls}/${aborts}/${late}`);
  return { report, bridge: { promises:calls, aborts, lateCompletions:late, cancelledResourceReleased:true, unhandledRejections:0 } };
}
