import { run_timeline, profile_timeline } from '../../target/runtime/pkg/hls_engine_runtime_tests.js';
export async function verify(memory) {
  const keyA = Uint8Array.from('2b7e151628aed2a6abf7158809cf4f3c'.match(/../g), x => parseInt(x,16));
  const keyB = Uint8Array.from('603deb1015ca71be2b73aef0857d7781'.match(/../g), x => parseInt(x,16));
  let calls = 0;
  const report = JSON.parse(await run_timeline(second => {
    calls++;
    return new Promise(resolve => setTimeout(() => resolve((second ? keyB : keyA).slice()), 0));
  }));
  if (calls === 0) throw new Error('timeline did not await the key bridge');
  for (const entry of report.cases) {
    if (typeof entry.report.actual.start.ticks !== 'string') throw new Error('lossy timeline ticks');
  }
  const wasmBefore = memory.buffer.byteLength;
  const heapBefore = globalThis.process?.memoryUsage?.().heapUsed ?? globalThis.performance?.memory?.usedJSHeapSize ?? null;
  const profile = JSON.parse(await profile_timeline());
  const heapAfter = globalThis.process?.memoryUsage?.().heapUsed ?? globalThis.performance?.memory?.usedJSHeapSize ?? null;
  return { report, profile: { ...profile, jsHeapBefore: heapBefore, jsHeapAfter: heapAfter, wasmBytesBefore: wasmBefore, wasmBytesAfter: memory.buffer.byteLength }, bridge: { promises: calls, losslessTimes: true } };
}
