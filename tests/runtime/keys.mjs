import { run_keys as run_suite, inspect } from '../../target/runtime/pkg/hls_engine_runtime_tests.js';
const check = (ok, message) => { if (!ok) throw new Error(message); };
export async function verify() {
  const calls = new Map(), aborts = new Map(), controllers = new Map(), pending = new Set();
  let reentries = 0, late = 0, settled = 0;
  function resolve(wire) {
    const request = JSON.parse(wire);
    check(request.sequence === '9007199254740993' && request.generation === '18446744073709551615' && request.snapshotRevision === '18446744073709551615', 'lossy request identity');
    check(/^\d+:\d+$/.test(inspect()), 'synchronous provider reentry'); reentries++;
    const token = request.token;
    calls.set(token, (calls.get(token) || 0) + 1);
    const controller = new AbortController(); controllers.set(token, controller);
    const uri = new URL(request.uri).pathname;
    if (uri === '/throw') throw new Error('RAW-CREDENTIALS');
    const promise = new Promise((accept, reject) => {
      setTimeout(() => {
        settled++;
        if (controller.signal.aborted) late++;
        if (uri === '/reject' || request.operation === 'reject-late') reject(new Error('RAW-CREDENTIALS'));
        else if (uri === '/invalid') accept(new Uint8Array(15));
        else if (uri === '/wrong-type') accept(Array(16).fill(171));
        else if (uri === '/unavailable') accept(null);
        else accept(new Uint8Array(16).fill(171));
      }, ['cancel', 'drop', 'last', 'reject-late'].includes(request.operation) || (request.operation === 'refresh' && request.revision === '1') ? 20 : 0);
    });
    pending.add(promise);
    promise.then(() => pending.delete(promise), () => pending.delete(promise));
    return promise;
  }
  function abort(token) {
    aborts.set(token, (aborts.get(token) || 0) + 1);
    controllers.get(token)?.abort();
    // State must already be detached when external abort callbacks reenter WASM.
    const state = inspect();
    check(state === 'dropped' || /^\d+:\d+$/.test(state), 'synchronous abort reentry'); reentries++;
  }
  const report = JSON.parse(await run_suite(resolve, abort));
  await Promise.allSettled([...pending]);
  check(calls.get('shared:1') === 1, 'coalescing/cache failed');
  check(calls.get('budget:3') === undefined, 'budget invoked a third provider');
  check(calls.get('ttl:2') === 1 && calls.get('refresh:2') === 1, 'refresh not exercised');
  for (const op of ['cancel', 'drop', 'last', 'reject-late']) check(aborts.get(`${op}:1`) === 1, `${op} abort count`);
  check(aborts.size === 4, 'aborted successful or shared work');
  check(late === 4, 'late success/rejection not exercised');
  return { report, bridge: { promises: settled, reentries, lateCompletions: late, aborts: [...aborts], unhandledRejections: 0 } };
}
