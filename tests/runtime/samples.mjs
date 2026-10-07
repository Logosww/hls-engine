import { run_samples, profile_samples } from '../../target/runtime/pkg/hls_engine_runtime_tests.js';
export async function verify(memory) {
  let calls=0, kids=0;
  const before=memory.buffer.byteLength;
  const started=performance.now();
  const key=Uint8Array.from('2b7e151628aed2a6abf7158809cf4f3c'.match(/../g),h=>parseInt(h,16));
  const resolveKey=(method,kid)=>{
    if(!['SAMPLE-AES','SAMPLE-AES-CTR'].includes(method)) throw new Error('sample method lost in bridge');
    if(kid!==null){if(!['00112233445566778899aabbccddeeff','42'.repeat(16)].includes(kid))throw new Error('incorrect KID');kids++;}
    calls++;
    return new Promise(resolve=>setTimeout(()=>resolve(key.slice()),0));
  };
  const report=JSON.parse(await run_samples(resolveKey));
  const measurements=JSON.parse(await profile_samples(resolveKey));
  if(!calls||!kids)throw new Error('sample provider was not exercised');
  return {report,bridge:{promises:calls,kids},profile:{...measurements,wasmBytesBefore:before,wasmBytesAfter:memory.buffer.byteLength,elapsedMs:performance.now()-started,jsHeap:globalThis.process?.memoryUsage?.().heapUsed??globalThis.performance?.memory?.usedJSHeapSize??null}};
}
