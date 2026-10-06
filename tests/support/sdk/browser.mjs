import init, { timeline_browser } from '/target/sdk-compat/pkg/hls_transmux_browser_wasm.js';
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
  let cancelledSamples=0;
  const unhandled=[];
  const onRejected=event=>unhandled.push(String(event.reason));
  window.addEventListener('unhandledrejection',onRejected);
  for(const name of ['sample-fmp4_avc_cenc','sample-fmp4_hevc_cbcs','sample-ts_avc_sample']) {
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
    const pending=timeline_browser(JSON.stringify(test.request),JSON.stringify(test.selection),read,()=>{cancelledWrites++;},()=>{requested();return new Promise(resolve=>late=resolve);},()=>{cancelledAborts++;},cancellation);
    await waiting;
    cancel();
    const result=JSON.parse(await pending);
    if(result.error?.code!=='ABORTED'||cancelledWrites||!cancelledAborts)throw new Error('sample cancellation failed: '+JSON.stringify(result));
    late({status:'available',key:new Uint8Array(16)});
    await new Promise(resolve=>setTimeout(resolve,0));
    if(cancelledWrites||unhandled.length)throw new Error('late sample Promise changed cancelled output');
    cancelledSamples++;
  }
  window.removeEventListener('unhandledrejection',onRejected);
  if(!reads||!keys||!writes||!aborts)throw new Error('host bridge was not exercised');
  return {status:'PASS',cases:results.length,reads,keys,writes,aborts,cancelledSamples,lateSampleCompletions:cancelledSamples,unhandledSampleRejections:unhandled.length,nativeBrowserTimelineEqual:true};
}
