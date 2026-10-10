import { subtitle_suite, subtitle_stream } from '../../target/runtime/pkg/hls_engine_runtime_tests.js';
export async function verify() {
  const report = JSON.parse(await subtitle_suite());
  const never = () => new Promise(() => {});
  let writes = 0, closes = 0;
  const writer = new WritableStream({
    async write(text) {
      const batch = JSON.parse(text);
      if (!Array.isArray(batch.cues) || !Array.isArray(batch.frontiers)) throw new Error('invalid subtitle batch');
      writes++;
      await Promise.resolve();
    },
    async close() { closes++; await Promise.resolve(); }
  }).getWriter();
  const result = JSON.parse(await subtitle_stream(text => writer.write(text), () => writer.close(), never()));
  if (result.error || writes < 2 || closes !== 1) throw new Error('sidecar Writable acknowledgement failed');
  for (const phase of ['write','close']) {
    const failure = () => Promise.reject(new Error('private sink failure'));
    const result = JSON.parse(await subtitle_stream(phase==='write' ? failure : () => {}, phase==='close' ? failure : () => {}, never()));
    if (result.error !== 'SubtitleOutput') throw new Error(`${phase} failure not propagated`);
  }
  let entered, release, cancel, lateCloses = 0, calls = 0;
  const started = new Promise(r => entered=r);
  const cancelled = new Promise(r => cancel=r);
  const running = subtitle_stream(() => { calls++; entered(); return new Promise(r => release=r); }, () => lateCloses++, cancelled);
  await started;
  cancel();
  if (JSON.parse(await running).error !== 'Cancelled') throw new Error('sidecar cancellation blocked');
  release();
  await Promise.resolve(); await Promise.resolve();
  if (calls !== 1 || lateCloses) throw new Error('late acknowledgement revived cancelled sidecar');
  const concurrent = await Promise.all([1,2].map(() => subtitle_stream(async () => {}, async () => {}, never())));
  if (concurrent.some(result => JSON.parse(result).error)) throw new Error('concurrent sidecar operations interfered');
  return {report,bridge:{writable:true,closeAwaited:true,writeAndCloseFailure:true,cancelledLateAck:true,concurrent:true}};
}
