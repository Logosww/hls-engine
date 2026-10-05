import { parse, roundtrip, playlist_fixture_text } from '../../target/runtime/pkg/hls_transmux_runtime_tests.js';

const check = (ok, message) => { if (!ok) throw new Error(message); };
export async function verify() {
  const context = { input_id: 'primary', generation: '18446744073709551615', revision: '9007199254740995' };
  const archive = parse(playlist_fixture_text(), 'https://final.test/redirect/list.m3u8', JSON.stringify(context));
  const value = JSON.parse(archive);
  check(value.context.generation === '18446744073709551615', 'lossy generation');
  check(value.media_sequence === '9007199254740993', 'lossy sequence');
  check(value.segments[0].duration.ticks === '1000000001', 'lossy ticks');
  check(value.segments[0].duration.timescale === 1000000000, 'wrong timescale');
  check(value.segments[1].slot.epoch === '18446744073709551615', 'lossy epoch');
  check(value.segments[1].range.offset === '9007199254741041', 'lossy offset');
  check(value.segments[1].keys.candidates.length === 0, 'NONE not captured');
  check(value.segments[1].map.keys.candidates.length === 1, 'MAP context changed');
  check(roundtrip(JSON.stringify(value)) === archive, 'JS round trip changed the archive');
  let rejected = 0;
  for (const mutate of [
    v => { v.media_sequence = Number(v.media_sequence); },
    v => { v.context.generation = Number(v.context.generation); },
    v => { v.segments[0].range.offset = Number(v.segments[0].range.offset); },
    v => { v.segments[0].duration.ticks = Number(v.segments[0].duration.ticks); },
    v => { v.segments[0].slot.sequence = '18446744073709551616'; },
    v => { v.segments[0].keys.candidates[0].iv[0] = 0; },
    v => { v.segments[1].map.keys.candidates = []; },
    v => { v.segments[0].location.value = 'https://rewritten.test/a'; },
    v => { v.segments[0].location.unexpected = true; },
  ]) {
    const changed = structuredClone(value);
    mutate(changed);
    let refused = false;
    try { roundtrip(JSON.stringify(changed)); } catch { refused = true; }
    check(refused, 'invalid archive accepted');
    rejected++;
  }
  let refusedContext = false;
  try { parse('#EXTM3U', 'https://example.test/a', JSON.stringify({ ...context, revision: 1 })); }
  catch { refusedContext = true; }
  check(refusedContext, 'numeric context accepted');
  return { report: value, bridge: { jsRoundtripEqual: true, rejectedArchives: rejected } };
}
