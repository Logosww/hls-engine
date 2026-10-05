import init from '../../target/runtime/pkg/hls_transmux_runtime_tests.js';
import { verify as playlist } from './playlist.mjs';
import { verify as keys } from './keys.mjs';
import { verify as resources } from './resources.mjs';
import { verify as prepared } from './prepared.mjs';
import { verify as contracts } from './contracts.mjs';
import { verify as timeline } from './timeline.mjs';

export async function verify(wasm) {
  const module = await init(wasm ? { module_or_path: wasm } : undefined);
  const results = { playlist: await playlist(), keys: await keys(),
    resources: await resources(), prepared: await prepared(), contracts: await contracts(), timeline: await timeline(module.memory) };
  return {
    profile: results.timeline.profile,
    report: Object.fromEntries(Object.entries(results).map(([name, result]) => [name, result.report])),
    bridge: Object.fromEntries(Object.entries(results).map(([name, result]) => [name, result.bridge])),
  };
}
