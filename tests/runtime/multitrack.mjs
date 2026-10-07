import {run_multitrack, profile_multitrack} from '../../target/runtime/pkg/hls_transmux_runtime_tests.js';
export async function verify(memory) {
  let calls=0;
  const before=memory.buffer.byteLength;
  const key=Uint8Array.from('2b7e151628aed2a6abf7158809cf4f3c'.match(/../g),v=>parseInt(v,16));
  const report=JSON.parse(await run_multitrack(async(_method,_kid,rotated)=>{calls++;await new Promise(r=>setTimeout(r,0));return rotated?Uint8Array.from('603deb1015ca71be2b73aef0857d7781'.match(/../g),v=>parseInt(v,16)):key.slice();}));
  if(report.length!==194||calls===0)throw new Error('missing multitrack runtime coverage');
  for(const suffix of ['classic','fragmented']) {
    const clear=report.find(r=>r.name===`packed-clear-${suffix}`);
    for(const encryption of ['aes128','sample_aes','aes128_rotation','sample_aes_rotation']) {
      if(report.find(r=>r.name===`packed-${encryption}-${suffix}`).hash!==clear.hash)throw new Error('Packed AAC clear mismatch');
    }
    const multi=report.find(r=>r.name===`multitrack-${suffix}`);
    if(multi.report.tracks.length!==5||multi.report.media.mappings[0].generation!=='9007199254740993')throw new Error('track identity/clock loss');
  }
  const heap=()=>globalThis.process?.memoryUsage?.().heapUsed??globalThis.performance?.memory?.usedJSHeapSize??null;
  const measurements=[];
  for(const count of [8,64,256]) {
    const wasmBytesBefore=memory.buffer.byteLength,jsHeapBefore=heap();
    const [row]=JSON.parse(await profile_multitrack(count));
    measurements.push({...row,wasmBytesBefore,wasmBytesAfter:memory.buffer.byteLength,jsHeapBefore,jsHeapAfter:heap()});
  }
  if(measurements.some(r=>r.retainedMappings>4||Number(r.peaks.queued_descriptors)>8))throw new Error('multitrack budget exceeded');
  return {report,profile:{measurements},bridge:{promises:calls,wasmBytesBefore:before,wasmBytesAfter:memory.buffer.byteLength}};
}
