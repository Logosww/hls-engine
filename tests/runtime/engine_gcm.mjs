import { engine_gcm_suite, engine_gcm_stream } from '../../target/runtime/pkg/hls_engine_runtime_tests.js';
export async function verify() {
  const report = JSON.parse(await engine_gcm_suite());
  if (report.cases.length !== 16 || report.clearEncryptedEqual !== true) {
    throw new Error('GCM media parity failed');
  }
  let resolves = 0, writes = 0;
  const chunks = [];
  const resolve = async text => {
    const request = JSON.parse(text);
    if (request.method !== 'AES-256-GCM' || typeof request.sequence !== 'string') throw new Error('invalid GCM key request');
    resolves++;
    await Promise.resolve();
    const start = request.uri.endsWith('rotated') ? 32 : 0;
    return Uint8Array.from({length:32}, (_, i) => start + i);
  };
  const stream = new WritableStream({async write(bytes) { writes++; chunks.push(bytes.slice()); await Promise.resolve(); }}).getWriter();
  const result = JSON.parse(await engine_gcm_stream(resolve, bytes => stream.write(bytes), () => {}, new Promise(() => {})));
  if (result.error || !resolves || !writes || Number(result.bytes) !== chunks.reduce((n,b) => n+b.length,0)) throw new Error('GCM Promise/writable failed');
  await stream.close();
  for (const response of [() => Promise.reject(new Error('secret-provider-error')), () => new Uint8Array(16), () => 'wrong-type']) {
    let output = 0;
    const rejected = JSON.parse(await engine_gcm_stream(response, () => { output++; }, () => {}, new Promise(() => {})));
    if (!rejected.error || output) throw new Error('invalid GCM provider emitted output');
  }
  let pendingKey, start, cancelled, aborted = 0, lateWrites = 0;
  const started = new Promise(resolve => start=resolve);
  const cancel = new Promise(resolve => cancelled=resolve);
  const running = engine_gcm_stream(() => { start(); return new Promise(resolve => pendingKey=resolve); }, () => { lateWrites++; }, () => aborted++, cancel);
  await started;
  cancelled();
  if (JSON.parse(await running).error !== 'Cancelled' || !aborted || lateWrites) throw new Error('GCM cancellation failed');
  pendingKey(new Uint8Array(32));
  await Promise.resolve(); await Promise.resolve();
  if (lateWrites) throw new Error('late GCM key wrote output');
  return {report, bridge: {profile: report.profile, clearEncryptedEqual: true, cases: 16, promiseProvider: true, writable: true, rejectedResponses: 3, cancelledLateKey: true}};
}
