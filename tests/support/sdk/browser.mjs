import init, { timeline_fixture_browser as timeline_browser, continuous_browser } from '/target/sdk-compat/pkg/hls_transmux_browser_wasm.js';
export async function verify() {
  await init();
  const cases = await (await fetch('/target/sdk-compat/cases.json')).json();
  const expected = await (await fetch('/target/sdk-compat/native-timeline.json')).json();
  const results = [];
  let reads = 0, keys = 0, writes = 0, aborts = 0;
  function canonical(bytes) {
    const walk = (a,b) => {
      const view = new DataView(bytes.buffer,bytes.byteOffset,bytes.byteLength);
      for (let p=a;p<b;) {
        const n=view.getUint32(p), type=String.fromCharCode(...bytes.subarray(p+4,p+8));
        if(n<8 || p+n>b) throw new Error('invalid output box');
        if(['moov','trak','mdia'].includes(type)) walk(p+8,p+n);
        if(['mvhd','tkhd','mdhd'].includes(type)) bytes.fill(0,p+12,p+(bytes[p+8]===1?28:20));
        p+=n;
      }
    };
    walk(0,bytes.length);
    return bytes;
  }
  for (const test of cases) {
    const chunks = [];
    const read = async text => {
      reads++;
      const request=JSON.parse(text);
      const bytes=new Uint8Array(await (await fetch('/'+test.files[request.url])).arrayBuffer());
      return request.offset===null?bytes:bytes.slice(Number(request.offset),Number(request.offset)+Number(request.length));
    };
    const resolve = async text => {
      keys++;
      const r=JSON.parse(text);
      if(r.method==='SAMPLE-AES-CTR'&&r.kid!=='00112233445566778899aabbccddeeff')throw new Error('SDK lost KID');
      const hex=r.resourceKind==='media'&&r.originalSequence==='9007199254740994'?'603deb1015ca71be2b73aef0857d7781':'2b7e151628aed2a6abf7158809cf4f3c';
      await new Promise(r=>setTimeout(r,0));
      return {status:'available',key:Uint8Array.from(hex.match(/../g),s=>parseInt(s,16))};
    };
    const write = async (bytes,index='0') => {
      writes++;
      (chunks[Number(index)]??=[]).push(bytes.slice());
      await new Promise(r=>setTimeout(r,0));
    };
    const value=JSON.parse(await timeline_browser(JSON.stringify(test.request),JSON.stringify(test.selection),read,write,resolve,()=>aborts++,new Promise(()=>{})));
    if(value.error) throw new Error(JSON.stringify(value));
    const hashes=[];
    for(const list of chunks){const bytes=new Uint8Array(list.reduce((n,b)=>n+b.length,0));let offset=0;for(const b of list){bytes.set(b,offset);offset+=b.length;}const hash=await crypto.subtle.digest('SHA-256',canonical(bytes));hashes.push(Array.from(new Uint8Array(hash),b=>b.toString(16).padStart(2,'0')).join(''));}
    results.push({name:test.name,hashes,report:value.report});
  }
  if(JSON.stringify(results)!==JSON.stringify(expected)) {
    // Object property order is not part of the JSON wire contract.
    const stable=v=>Array.isArray(v)?v.map(stable):v&&typeof v==='object'?Object.fromEntries(Object.keys(v).sort().map(k=>[k,stable(v[k])])):v;
    if(JSON.stringify(stable(results))!==JSON.stringify(stable(expected)))throw new Error('SDK native/browser timeline mismatch');
  }
  const continuousExpected=await(await fetch('/target/sdk-compat/native-continuous.json')).json();
  let continuousCases=0, continuousCloseFailure=false;
  for(const expected of continuousExpected) {
    const test=cases.find(c=>c.name===expected.name);
    const read=async text=>{const r=JSON.parse(text);const bytes=new Uint8Array(await(await fetch('/'+test.files[r.url])).arrayBuffer());return r.offset===null?bytes:bytes.slice(Number(r.offset),Number(r.offset)+Number(r.length));};
    const resolve=async text=>{const r=JSON.parse(text);const hex=r.resourceKind==='media'&&r.originalSequence==='9007199254740994'?'603deb1015ca71be2b73aef0857d7781':'2b7e151628aed2a6abf7158809cf4f3c';await new Promise(r=>setTimeout(r,0));return {status:'available',key:Uint8Array.from(hex.match(/../g),s=>parseInt(s,16))};};
    const chunks=[];let closed=false;
    const sink=new WritableStream({async write(bytes){chunks.push(bytes.slice());await new Promise(r=>setTimeout(r,0));},close(){closed=true;}}).getWriter();
    const value=JSON.parse(await continuous_browser(JSON.stringify(test.request),read,b=>sink.write(b),resolve,()=>aborts++,new Promise(()=>{})));
    if(value.error)throw new Error(JSON.stringify(value));
    await sink.close();if(!closed)throw new Error('SDK sink not closed');
    const bytes=new Uint8Array(chunks.reduce((n,b)=>n+b.length,0));let offset=0;for(const chunk of chunks){bytes.set(chunk,offset);offset+=chunk.length;}
    const hash=Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256',canonical(bytes))),b=>b.toString(16).padStart(2,'0')).join('');
    const stable=v=>Array.isArray(v)?v.map(stable):v&&typeof v==='object'?Object.fromEntries(Object.keys(v).sort().map(k=>[k,stable(v[k])])):v;
    if(hash!==expected.hash||JSON.stringify(stable(value.report))!==JSON.stringify(stable(expected.report)))throw new Error('continuous SDK native/browser mismatch: '+test.name);
    continuousCases++;
    if(!continuousCloseFailure){
      const failing=new WritableStream({close(){throw new Error('close failed');}}).getWriter();
      const result=JSON.parse(await continuous_browser(JSON.stringify(test.request),read,b=>failing.write(b),resolve,()=>aborts++,new Promise(()=>{})));
      if(result.error)throw new Error('expected core flush success before SDK close');
      let publicComplete=false;try{await failing.close();publicComplete=true;}catch{continuousCloseFailure=true;}
      if(publicComplete)throw new Error('SDK published completion before close');
    }
  }
  let cancelledSamples=0, cancelledContinuous=0, continuousPromiseFailures=0;
  const unhandled=[];
  const onRejected=event=>unhandled.push(String(event.reason));
  window.addEventListener('unhandledrejection',onRejected);
  for(const continuous of [false,true]) for(const name of ['sample-fmp4_avc_cenc','sample-fmp4_hevc_cbcs','sample-ts_avc_sample']) {
    const test=cases.find(c=>c.name===name);
    let cancel,late,requested;
    let cancelledWrites=0, cancelledAborts=0;
    const waiting=new Promise(resolve=>requested=resolve);
    const cancellation=new Promise(resolve=>cancel=resolve);
    const read=async text=>{
      const request=JSON.parse(text);
      const bytes=new Uint8Array(await(await fetch('/'+test.files[request.url])).arrayBuffer());
      return request.offset===null?bytes:bytes.slice(Number(request.offset),Number(request.offset)+Number(request.length));
    };
    const write=()=>{cancelledWrites++;};
    const key=()=>{requested();return new Promise(resolve=>late=resolve);};
    const abort=()=>{cancelledAborts++;};
    const pending=continuous?continuous_browser(JSON.stringify(test.request),read,write,key,abort,cancellation):timeline_browser(JSON.stringify(test.request),JSON.stringify(test.selection),read,write,key,abort,cancellation);
    await waiting;
    cancel();
    const result=JSON.parse(await pending);
    if(result.error?.code!=='ABORTED'||cancelledWrites||!cancelledAborts)throw new Error('sample cancellation failed: '+JSON.stringify(result));
    late({status:'available',key:new Uint8Array(16)});
    await new Promise(resolve=>setTimeout(resolve,0));
    if(cancelledWrites||unhandled.length)throw new Error('late sample Promise changed cancelled output');
    if(continuous)cancelledContinuous++;else cancelledSamples++;
  }
  const faultCase=cases.find(c=>c.name==='sample-fmp4_avc_cenc');
  for(const stage of ['read','key','write']) {
    const read=async text=>{if(stage==='read')throw new Error('read rejected');const r=JSON.parse(text);return new Uint8Array(await(await fetch('/'+faultCase.files[r.url])).arrayBuffer());};
    const key=async()=>{if(stage==='key')throw new Error('key rejected');return {status:'available',key:Uint8Array.from('2b7e151628aed2a6abf7158809cf4f3c'.match(/../g),s=>parseInt(s,16))};};
    const result=JSON.parse(await continuous_browser(JSON.stringify(faultCase.request),read,async()=>{throw new Error('write rejected');},key,()=>aborts++,new Promise(()=>{})));
    if(!result.error)throw new Error('continuous Promise rejection completed successfully: '+stage);
    continuousPromiseFailures++;
  }
  await new Promise(r=>setTimeout(r,0));
  if(unhandled.length)throw new Error('unhandled continuous rejection');
  window.removeEventListener('unhandledrejection',onRejected);
  if(!reads||!keys||!writes||!aborts)throw new Error('host bridge was not exercised');
  return {status:'PASS',continuousCases,cancelledContinuous,continuousPromiseFailures,continuousCloseFailure,cases:results.length,reads,keys,writes,aborts,cancelledSamples,lateSampleCompletions:cancelledSamples,unhandledSampleRejections:unhandled.length,nativeBrowserTimelineEqual:true};
}
