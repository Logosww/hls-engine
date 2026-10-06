import { run_continuous, profile_continuous } from '../../target/runtime/pkg/hls_transmux_runtime_tests.js';
export async function verify(memory) {
  let calls=0;
  const before=memory.buffer.byteLength;
  const key=Uint8Array.from('2b7e151628aed2a6abf7158809cf4f3c'.match(/../g),h=>parseInt(h,16));
  const report=JSON.parse(await run_continuous(async()=>{calls++;await new Promise(r=>setTimeout(r,0));return key.slice();}));
  if(report.length!==34||!calls)throw new Error('continuous profiles incomplete');
  if(report.some(r=>r.report.end_reason!=='Stop'||r.report.inputs[0].total!==null||r.report.mappings[0].generation!=='9007199254740993'))throw new Error('continuous contract lost');
  const heap=()=>globalThis.process?.memoryUsage?.().heapUsed??globalThis.performance?.memory?.usedJSHeapSize??null;
  const measurements=[];
  for (const count of [8,64,256]) {
    const wasmBytesBefore=memory.buffer.byteLength,jsHeapBefore=heap();
    const [row]=JSON.parse(await profile_continuous(count));
    measurements.push({...row,wasmBytesBefore,wasmBytesAfter:memory.buffer.byteLength,jsHeapBefore,jsHeapAfter:heap()});
  }
  if(measurements.some(r=>r.retainedMappings>4||Number(r.peaks.queued_descriptors)>2))throw new Error('continuous budget exceeded');
  return {report,profile:{measurements,wasmBytesBefore:before,wasmBytesAfter:memory.buffer.byteLength,jsHeap:globalThis.process?.memoryUsage?.().heapUsed??globalThis.performance?.memory?.usedJSHeapSize??null},bridge:{promises:calls,wasmBytesBefore:before,wasmBytesAfter:memory.buffer.byteLength}};
}
